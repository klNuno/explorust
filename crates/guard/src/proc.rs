//! Child processes and waits, as thin wrappers over CreateProcessW and WaitForMultipleObjects.
//! An explorust.exe built with `uiAccess="true"` cannot come from CreateProcess in this
//! medium-integrity guard (ERROR_ELEVATION_REQUIRED): ShellExecuteEx hands it to the AppInfo
//! service, which checks the signature and the folder and starts it with UI access.

use std::path::Path;
use crate::log;
use windows::Win32::Foundation::{CloseHandle, ERROR_ELEVATION_REQUIRED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW, EVENT_MODIFY_STATE, GetExitCodeProcess, GetProcessId,
    INFINITE,
    OpenEventW, OpenProcess, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, ResetEvent, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// Set by explorust's `--quit`: the clean way out when this guard may not terminate it.
const QUIT_EVENT: &str = r"Local\explorust-quit";

pub struct Child {
    pub handle: HANDLE,
    pub pid: u32,
}

impl Child {
    pub fn spawn(exe: &Path, args: &str) -> Result<Child, String> {
        let mut cmd: Vec<u16> = format!("\"{}\" {args}", exe.display()).encode_utf16().chain(Some(0)).collect();
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        let app = HSTRING::from(exe.as_os_str());
        let cwd = exe.parent().map(|p| HSTRING::from(p.as_os_str()));
        // SAFETY: cmd is a mutable NUL-terminated buffer as CreateProcessW requires.
        let made = unsafe {
            CreateProcessW(
                &app,
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT,
                None,
                cwd.as_ref().map(|c| windows::core::PCWSTR(c.as_ptr())).unwrap_or(windows::core::PCWSTR::null()),
                &si,
                &mut pi,
            )
        };
        match made {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_ELEVATION_REQUIRED.to_hresult() => return Self::shell_execute(exe, args),
            Err(e) => return Err(format!("CreateProcess {}: {e}", exe.display())),
        }
        // SAFETY: the thread handle is not needed.
        let _ = unsafe { CloseHandle(pi.hThread) };
        Ok(Child { handle: pi.hProcess, pid: pi.dwProcessId })
    }

    /// The uiAccess path: ShellExecuteEx asks the AppInfo service, which starts the exe with UI
    /// access when it is signed by a trusted certificate and lives in a secure folder.
    fn shell_execute(exe: &Path, args: &str) -> Result<Child, String> {
        let file = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(args);
        let cwd = exe.parent().map(|p| HSTRING::from(p.as_os_str()));
        let verb = HSTRING::from("open");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            lpDirectory: cwd.as_ref().map(|c| PCWSTR(c.as_ptr())).unwrap_or(PCWSTR::null()),
            nShow: 1,
            ..Default::default()
        };
        // SAFETY: every string outlives the call; hProcess is ours to close.
        unsafe { ShellExecuteExW(&mut info) }.map_err(|e| format!("ShellExecuteEx {}: {e}", exe.display()))?;
        if info.hProcess.is_invalid() {
            return Err(format!("ShellExecuteEx {}: no process handle", exe.display()));
        }
        // SAFETY: a valid process handle.
        let pid = unsafe { GetProcessId(info.hProcess) };
        log::line(&format!("CreateProcess needs elevation (uiAccess): started through ShellExecuteEx, pid {pid}"));
        Ok(Child { handle: info.hProcess, pid })
    }

    /// Takes over a running process (an explorust whose guard died) as if this guard had started it.
    pub fn open(pid: u32) -> Result<Child, String> {
        let access = PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: Windows validates the pid and the requested rights.
        let handle = unsafe { OpenProcess(access, false, pid) }.map_err(|e| format!("open pid {pid}: {e}"))?;
        Ok(Child { handle, pid })
    }

    /// True once the process has exited.
    pub fn ended(&self) -> bool {
        // SAFETY: valid process handle owned by self.
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0 }
    }

    pub fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        // SAFETY: valid process handle owned by self.
        let _ = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        code
    }

    /// Kills the process and waits until it is gone, so the shell window it owned is released.
    /// A uiAccess explorust runs above this guard's integrity level, so TerminateProcess can be
    /// refused: then explorust is asked to quit through its own event.
    pub fn kill(&self) {
        // SAFETY: valid process handle owned by self.
        unsafe {
            if let Err(e) = TerminateProcess(self.handle, 1) {
                let asked = Event::set_existing(QUIT_EVENT);
                log::line(&format!("terminate pid {}: {e}; quit event: {asked:?}", self.pid));
            }
            WaitForSingleObject(self.handle, 10_000);
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: closes the handle this struct owns.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

pub struct Event(pub HANDLE);

impl Event {
    /// A manual-reset event, named so another process can set it.
    pub fn named(name: &str) -> Result<Event, String> {
        // SAFETY: plain event creation.
        unsafe { CreateEventW(None, true, false, &HSTRING::from(name)) }
            .map(Event)
            .map_err(|e| format!("CreateEvent {name}: {e}"))
    }

    pub fn set_existing(name: &str) -> Result<(), String> {
        Self::open_for_signal(name)?.signal()
    }

    pub fn open_for_signal(name: &str) -> Result<Event, String> {
        // SAFETY: the returned event owns its handle and closes it on drop.
        unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name)) }
            .map(Event)
            .map_err(|e| format!("{name}: {e}"))
    }

    pub fn signal(&self) -> Result<(), String> {
        // SAFETY: the handle is valid while self lives.
        unsafe { SetEvent(self.0) }.map_err(|e| e.to_string())
    }

    pub fn reset(&self) -> Result<(), String> {
        // SAFETY: the handle is valid while self lives.
        unsafe { ResetEvent(self.0) }.map_err(|e| e.to_string())
    }

    pub fn is_signalled(name: &str) -> bool {
        // SAFETY: the returned handle, when present, is closed by the owner.
        unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) }
            .is_ok_and(|handle| {
                let event = Self(handle);
                // SAFETY: the event handle is live and the wait returns immediately.
                unsafe { WaitForSingleObject(event.0, 0) == WAIT_OBJECT_0 }
            })
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: closes the handle this struct owns.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Index of the first signaled handle, or None on timeout. `ms` None waits forever.
pub fn wait_any(handles: &[HANDLE], ms: Option<u32>) -> Option<usize> {
    // SAFETY: every handle is valid for the duration of the call.
    let r = unsafe { WaitForMultipleObjects(handles, false, ms.unwrap_or(INFINITE)) };
    if r == WAIT_TIMEOUT {
        return None;
    }
    let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    (i < handles.len()).then_some(i)
}

/// Runs a console program with no window and waits at most `limit` for it, so a child that never
/// ends (PowerShell's Appx module, a service install) cannot freeze an install or an uninstall.
/// Ok((success, stdout, stderr)); Err when it does not start or is killed at the limit.
pub fn run_bounded(cmd: &mut std::process::Command, limit: std::time::Duration) -> Result<(bool, String, String), String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    let what = format!("{:?}", cmd.get_program());
    let mut child = cmd
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what}: {e}"))?;
    // Each pipe is read on its own thread and handed over through a channel, so a grandchild that
    // keeps a pipe open cannot hold this call past its limit.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            let _ = tx.send(text);
        });
        rx
    };
    let out = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let t0 = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(std::time::Duration::from_millis(200)),
            Ok(None) => {
                let _ = child.kill();
                return Err(format!("{what} still running after {} s: stopped", limit.as_secs()));
            }
            Err(e) => return Err(format!("{what}: {e}")),
        }
    };
    let rest = std::time::Duration::from_secs(5);
    let out = out.recv_timeout(rest).unwrap_or_default();
    let err = err.recv_timeout(rest).unwrap_or_default();
    Ok((status.success(), out.trim().to_string(), err.trim().to_string()))
}
