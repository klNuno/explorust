//! The independently running logon-app batch. The guard starts it when the native desktop answers.

mod registry;
#[cfg(test)]
mod tests;

use crate::{events, launch_timing, log, startup_rules};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0},
    System::{
        Com::CoTaskMemFree,
        Environment::ExpandEnvironmentStringsW,
        Ole::{OleInitialize, OleUninitialize},
        Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_EXPAND_SZ},
        Threading::{INFINITE, WaitForSingleObject},
    },
    UI::{
        Shell::{
            FOLDERID_CommonStartup, FOLDERID_Profile, FOLDERID_Startup, KF_FLAG_DONT_VERIFY,
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            SHGetKnownFolderPath, ShellExecuteExW,
        },
        WindowsAndMessaging::{GetSystemMetrics, SM_CLEANBOOT, SW_SHOWNOACTIVATE},
    },
};
use windows::core::{GUID, HSTRING, PCWSTR};

pub fn spawn_legacy_runner() {
    let result = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--startup-apps").spawn());
    if let Err(e) = result {
        log::line(&format!("startup: could not launch the session runner: {e}"));
    }
}

pub fn run(ack: Option<&str>) -> i32 {
    if events::Owned::is_signalled(launch_timing::FALLBACK_EVENT) {
        log::line("startup: handoff requested before claim; Explorer owns startup");
        return 0;
    }
    let claimed = match registry::claim() {
        Ok(claimed) => claimed,
        Err(e) => {
            log::line(&format!("startup: could not claim this logon: {e}"));
            return 1;
        }
    };
    if let Some(name) = ack && let Err(e) = events::set_existing(name) {
        log::line(&format!("startup: could not acknowledge claim: {e}"));
    }
    if claimed {
        log::line("startup: session runner claimed Explorer's startup marker");
        launch_all();
    } else {
        log::line("startup: apps already handled for this logon");
    }
    0
}

const LAUNCH_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Batch(Vec<JoinHandle<()>>);

impl Batch {
    // Give ordinary entries their usual launch order. A provider stuck in ShellExecute or a
    // redirected Startup folder gets its own STA, so it cannot hold up the next entry or folder.
    fn step(&mut self, label: &str, work: impl FnOnce(SyncSender<()>) + Send + 'static) {
        let (tx, rx) = mpsc::sync_channel(1);
        match thread::Builder::new().name("startup-entry".into()).spawn(move || work(tx)) {
            Ok(worker) => {
                if rx.recv_timeout(LAUNCH_GRACE) == Err(mpsc::RecvTimeoutError::Timeout) {
                    log::line(&format!("startup: {label} still pending after {} s; continuing", LAUNCH_GRACE.as_secs()));
                }
                self.0.push(worker);
            }
            Err(e) => log::line(&format!("startup: {label} worker: {e}")),
        }
    }

    fn finish(self) {
        for worker in self.0 {
            let _ = worker.join();
        }
    }
}

fn launch_all() {
    // SAFETY: system metric for the current boot mode.
    let safe_mode = unsafe { GetSystemMetrics(SM_CLEANBOOT) } != 0;
    let mut batch = Batch::default();
    let profile = known_folder(&FOLDERID_Profile);
    if machine_run_once_allowed() {
        launch_registry(&mut batch, HKEY_LOCAL_MACHINE, "RunOnce", registry::NATIVE, true, safe_mode, profile.as_deref());
    }
    for (root, subkey, view, once) in [
        (HKEY_LOCAL_MACHINE, "Run", registry::NATIVE, false),
        (HKEY_LOCAL_MACHINE, "Run", registry::WOW32, false),
        (HKEY_CURRENT_USER, "Run", registry::NATIVE, false),
        (HKEY_CURRENT_USER, "RunOnce", registry::NATIVE, true),
    ] {
        launch_registry(&mut batch, root, subkey, view, once, safe_mode, profile.as_deref());
    }
    if !safe_mode {
        for (id, common, label) in [(FOLDERID_CommonStartup, true, "Common Startup"), (FOLDERID_Startup, false, "user Startup")] {
            batch.step(label, move |done| {
                let mut folder = Batch::default();
                let root = if common { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
                launch_folder(&mut folder, &id, root);
                let _ = done.send(());
                folder.finish();
            });
        }
    }
    batch.finish();
}

fn machine_run_once_allowed() -> bool {
    use windows::Win32::Security::{CheckTokenMembership, CreateWellKnownSid, PSID, SECURITY_MAX_SID_SIZE, WinBuiltinAdministratorsSid};
    use windows::core::BOOL;
    let mut size = SECURITY_MAX_SID_SIZE;
    let mut sid = [0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<u32>())];
    let sid = PSID(sid.as_mut_ptr().cast());
    let mut member = BOOL(0);
    // SAFETY: the aligned buffer is large enough for any SID. A null token checks this thread.
    unsafe {
        CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(sid), &mut size)
            .and_then(|()| CheckTokenMembership(None, sid, &mut member))
    }.is_ok() && member.as_bool()
}

fn launch_registry(
    batch: &mut Batch,
    root: HKEY,
    subkey: &str,
    view: registry::View,
    once: bool,
    safe_mode: bool,
    profile: Option<&Path>,
) {
    let path = registry::version_path(subkey);
    let Some(key) = registry::Key::open(root, &path, KEY_QUERY_VALUE | view) else {
        return;
    };
    let approval = if view == registry::WOW32 {
        "Run32"
    } else {
        "Run"
    };
    for value in key.values() {
        let (deferred, safe_once) = startup_rules::run_once_flags(&value.name);
        if safe_mode && !(once && safe_once) {
            continue;
        }
        if !once && registry::disabled(root, approval, &value.name) {
            continue;
        }
        let Some(command) = command_line(&value.data, value.kind == REG_EXPAND_SZ.0) else {
            continue;
        };
        let writer = match once
            .then(|| registry::Key::writer(root, &path, view))
            .transpose()
        {
            Ok(writer) => writer,
            Err(e) => {
                log::line(&format!("startup: could not consume {}: {e}", value.name));
                continue;
            }
        };
        let label = format!("{subkey} {}", value.name);
        let description = label.clone();
        let profile = profile.map(Path::to_path_buf);
        batch.step(&label, move |done| {
            let Some(ole) = OleSession::new() else { return };
            if !deferred && let Some(key) = &writer && !consume(key, &value) {
                return;
            }
            let launched = launch_command(&command, profile.as_deref(), once && deferred);
            drop(ole);
            let _ = done.send(());
            match launched {
                Ok(process) => {
                    log::line(&format!("startup: launched {description}"));
                    if deferred && let Some(key) = writer {
                        consume_after_run(key, value, process);
                    }
                }
                Err(e) => log::line(&format!("startup: {description}: {e}")),
            }
        });
    }
}

struct OleSession;

impl OleSession {
    fn new() -> Option<Self> {
        // SAFETY: this is a fresh worker thread. Drop balances initialization on it.
        match unsafe { OleInitialize(None) } {
            Ok(()) => Some(Self),
            Err(e) => {
                log::line(&format!("startup: OLE: {e}"));
                None
            }
        }
    }
}

impl Drop for OleSession {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

fn consume(key: &registry::Key, value: &registry::Value) -> bool {
    if !key.matches(value) {
        log::line(&format!("startup: RunOnce {} changed or was removed; leaving it intact", value.name));
        return false;
    }
    match key.delete(&value.name) {
        Ok(()) => true,
        Err(e) => {
            log::line(&format!("startup: could not consume {}: {e}", value.name));
            false
        }
    }
}

fn consume_after_run(key: registry::Key, value: registry::Value, process: Option<OwnedHandle>) {
    // ShellExecuteEx can hand the command to an existing app without returning a process.
    if process.is_none_or(|process| {
        // SAFETY: this thread owns the returned process handle for the entire wait.
        (unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), INFINITE) }) == WAIT_OBJECT_0
    }) {
        consume(&key, &value);
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the returned NUL-terminated string is copied before freeing it.
    unsafe {
        let name = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0.cast()));
        Some(path)
    }
}

fn launch_folder(batch: &mut Batch, id: &GUID, root: HKEY) {
    let Some(dir) = known_folder(id) else { return };
    let Ok(entries) = dir.read_dir() else { return };
    for entry in entries.flatten() {
        use std::os::windows::fs::MetadataExt;
        let path = entry.path();
        // Explorer omits hidden/system files (including desktop.ini) and subdirectories.
        let has_item =
            std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.file_attributes() & 0x6 == 0);
        if !has_item
            || registry::disabled(
                root,
                "StartupFolder",
                &HSTRING::from(entry.file_name().as_os_str()),
            )
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let label = name.clone();
        batch.step(&label, move |_| {
            let Some(_ole) = OleSession::new() else { return };
            let file = HSTRING::from(path.as_os_str());
            // ShellExecute uses the working directory stored in .lnk files.
            match execute(&file, &HSTRING::new(), None, false) {
                Ok(_) => log::line(&format!("startup: launched shortcut {name}")),
                Err(e) => log::line(&format!("startup: shortcut {name}: {e}")),
            }
        });
    }
}

fn command_line(data: &[u16], expand: bool) -> Option<Vec<u16>> {
    let mut input: Vec<_> = data
        .iter()
        .copied()
        .take_while(|&c| c != 0)
        .chain([0])
        .collect();
    if expand {
        // SAFETY: the input is NUL terminated and both calls use a correctly sized buffer.
        unsafe {
            let source = PCWSTR(input.as_ptr());
            let required = ExpandEnvironmentStringsW(source, None) as usize;
            if required == 0 {
                return None;
            }
            let mut expanded = vec![0u16; required];
            let written = ExpandEnvironmentStringsW(source, Some(&mut expanded)) as usize;
            if written == 0 || written > required {
                return None;
            }
            input = expanded;
        }
    }
    input.pop();
    Some(input)
}

fn launch_command(
    command: &[u16],
    profile: Option<&Path>,
    keep_process: bool,
) -> windows::core::Result<Option<OwnedHandle>> {
    let command = trim_spaces(command);
    let (file, params) = split_command(command);
    let path = PathBuf::from(OsString::from_wide(file));
    let working_dir = if path.is_absolute() {
        path.parent()
    } else {
        profile
    };
    let file = HSTRING::from(path.as_os_str());
    let params = HSTRING::from(OsString::from_wide(trim_spaces(params)).as_os_str());
    execute(&file, &params, working_dir, keep_process)
}

fn split_command(command: &[u16]) -> (&[u16], &[u16]) {
    if let Some(quoted) = command.strip_prefix(&[b'"' as u16]) {
        return match quoted.iter().position(|&c| c == b'"' as u16) {
            Some(end) => (&quoted[..end], &quoted[end + 1..]),
            None => (quoted, &[]),
        };
    }
    // For unquoted paths, try each token boundary up to the full path. This accepts
    // the common unquoted "C:\Program Files\App\app.exe --flag" without invoking a command shell.
    let mut first_space = command.len();
    for (end, _) in command
        .iter()
        .enumerate()
        .filter(|(_, c)| space(**c))
        .chain([(command.len(), &0)])
    {
        first_space = first_space.min(end);
        if end != 0 && Path::new(&OsString::from_wide(&command[..end])).is_file() {
            return command.split_at(end);
        }
    }
    command.split_at(first_space)
}

fn space(c: u16) -> bool {
    c == b' ' as u16 || c == b'\t' as u16
}

fn trim_spaces(data: &[u16]) -> &[u16] {
    let start = data.iter().take_while(|&&c| space(c)).count();
    &data[start..]
}

fn execute(
    file: &HSTRING,
    params: &HSTRING,
    directory: Option<&Path>,
    keep_process: bool,
) -> windows::core::Result<Option<OwnedHandle>> {
    let directory = directory.map(|p| HSTRING::from(p.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC
            | SEE_MASK_FLAG_NO_UI
            | if keep_process {
                SEE_MASK_NOCLOSEPROCESS
            } else {
                0
            },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: directory
            .as_ref()
            .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
        nShow: SW_SHOWNOACTIVATE.0,
        ..Default::default()
    };
    // SAFETY: all strings and the structure are valid through the synchronous call.
    unsafe { ShellExecuteExW(&mut info)? };
    // SAFETY: ShellExecuteEx transfers ownership when SEE_MASK_NOCLOSEPROCESS returns a handle.
    Ok((keep_process && !info.hProcess.is_invalid())
        .then(|| unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) }))
}
