//! Launches the user's logon apps after the desktop and tray have come up.

mod registry;

use crate::{components, log, startup_rules};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0},
    Graphics::Dwm::DwmFlush,
    System::{
        Com::CoTaskMemFree,
        Environment::ExpandEnvironmentStringsW,
        Ole::{OleInitialize, OleUninitialize},
        Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_EXPAND_SZ},
        Threading::{INFINITE, WaitForSingleObject},
    },
    UI::{
        Shell::{
            FOLDERID_CommonStartup, FOLDERID_Profile, FOLDERID_Startup, KNOWN_FOLDER_FLAG,
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            SHGetKnownFolderPath, ShellExecuteExW,
        },
        WindowsAndMessaging::{
            FindWindowW, GetSystemMetrics, GetWindowThreadProcessId, IsWindowVisible, SM_CLEANBOOT,
            SMTO_ABORTIFHUNG, SMTO_BLOCK, SW_SHOWNOACTIVATE, SendMessageTimeoutW, WM_NULL,
        },
    },
};
use windows::core::{GUID, HSTRING, PCWSTR, w};

pub fn after_taskbar(started: Instant, limit: Duration, logon: bool) {
    let _ = std::thread::Builder::new()
        .name("startup-apps".into())
        .spawn(move || {
            if !components::part_enabled(bus::Owner::Taskbar) {
                if logon {
                    after_desktop(started, limit);
                }
                return;
            }
            while started.elapsed() < limit {
                if crate::problem::pending() {
                    return;
                }
                if window_ready(w!("Shell_TrayWnd")) {
                    // SAFETY: wait for the compositor after the taskbar starts processing messages.
                    let composed = unsafe { DwmFlush() }.is_ok();
                    log::line(&format!(
                        "taskbar answering in {} ms (DwmFlush: {composed})",
                        started.elapsed().as_millis()
                    ));
                    if logon {
                        after_desktop(started, limit);
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
}

fn after_desktop(started: Instant, limit: Duration) {
    let desktop = components::part_enabled(bus::Owner::Desktop);
    while desktop && !window_ready(w!("Progman")) {
        if started.elapsed() >= limit {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if crate::problem::pending() {
        return;
    }
    // SAFETY: initializes and uninitializes OLE on this thread, as ShellExecuteEx recommends.
    unsafe {
        if let Err(e) = OleInitialize(None) {
            log::line(&format!("startup: OLE: {e}"));
            return;
        }
    }
    match registry::claim() {
        Ok(true) => launch_all(),
        Ok(false) => log::line("startup: apps already handled for this logon"),
        Err(e) => log::line(&format!("startup: could not claim this logon: {e}")),
    }
    unsafe { OleUninitialize() };
}

fn window_ready(class: PCWSTR) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    // SAFETY: query an HWND and send a bounded, data-free message.
    unsafe {
        let Ok(hwnd) = FindWindowW(class, None) else {
            return false;
        };
        let mut owner = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        owner == std::process::id()
            && IsWindowVisible(hwnd).as_bool()
            && SendMessageTimeoutW(
                hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                50,
                None,
            )
            .0 != 0
    }
}

fn launch_all() {
    // SAFETY: system metric for the current boot mode.
    let safe_mode = unsafe { GetSystemMetrics(SM_CLEANBOOT) } != 0;
    let profile = known_folder(&FOLDERID_Profile);
    for (root, subkey, view, once) in [
        (HKEY_LOCAL_MACHINE, "RunOnce", registry::NATIVE, true),
        (HKEY_LOCAL_MACHINE, "Run", registry::NATIVE, false),
        (HKEY_LOCAL_MACHINE, "Run", registry::WOW32, false),
        (HKEY_CURRENT_USER, "Run", registry::NATIVE, false),
        (HKEY_CURRENT_USER, "RunOnce", registry::NATIVE, true),
    ] {
        launch_registry(root, subkey, view, once, safe_mode, profile.as_deref());
    }
    if !safe_mode {
        launch_folder(&FOLDERID_CommonStartup, HKEY_LOCAL_MACHINE);
        launch_folder(&FOLDERID_Startup, HKEY_CURRENT_USER);
    }
}

fn launch_registry(
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
        if !deferred
            && let Some(key) = &writer
            && !consume(key, &value.name)
        {
            continue;
        }
        match launch_command(&command, profile, once && deferred) {
            Ok(process) => {
                log::line(&format!("startup: launched {subkey} {}", value.name));
                if deferred && let Some(key) = writer {
                    consume_after_run(key, value.name, process);
                }
            }
            Err(e) => log::line(&format!("startup: {subkey} {}: {e}", value.name)),
        }
    }
}

fn consume(key: &registry::Key, name: &HSTRING) -> bool {
    match key.delete(name) {
        Ok(()) => true,
        Err(e) => {
            log::line(&format!("startup: could not consume {name}: {e}"));
            false
        }
    }
}

fn consume_after_run(key: registry::Key, name: HSTRING, process: Option<OwnedHandle>) {
    let Some(process) = process else {
        // ShellExecuteEx can hand a command to an already-running app without returning a process.
        consume(&key, &name);
        return;
    };
    let started = std::thread::Builder::new()
        .name("startup-run-once".into())
        .spawn(move || {
            // SAFETY: this thread owns the returned process handle for the whole wait.
            if unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), INFINITE) }
                == WAIT_OBJECT_0
            {
                consume(&key, &name);
            }
        });
    if let Err(e) = started {
        log::line(&format!(
            "startup: could not wait for deferred RunOnce: {e}"
        ));
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the returned NUL-terminated string is copied before freeing it.
    unsafe {
        let name = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0.cast()));
        Some(path)
    }
}

fn launch_folder(id: &GUID, root: HKEY) {
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
        let file = HSTRING::from(path.as_os_str());
        // ShellExecute uses the working directory stored in .lnk files.
        match execute(&file, &HSTRING::new(), None, false) {
            Ok(_) => log::line(&format!(
                "startup: launched shortcut {}",
                entry.file_name().to_string_lossy()
            )),
            Err(e) => log::line(&format!(
                "startup: shortcut {}: {e}",
                entry.file_name().to_string_lossy()
            )),
        }
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
