//! Child processes and waits, as thin wrappers over CreateProcessW and WaitForMultipleObjects.

use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW, EVENT_MODIFY_STATE, GetExitCodeProcess, INFINITE,
    OpenEventW, OpenProcess, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, STARTUPINFOW, SetEvent, TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};

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
        unsafe {
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
        }
        .map_err(|e| format!("CreateProcess {}: {e}", exe.display()))?;
        // SAFETY: the thread handle is not needed.
        let _ = unsafe { CloseHandle(pi.hThread) };
        Ok(Child { handle: pi.hProcess, pid: pi.dwProcessId })
    }

    /// Takes over a running process (an explorust whose guard died) as if this guard had started it.
    pub fn open(pid: u32) -> Result<Child, String> {
        let access = PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: Windows validates the pid and the requested rights.
        let handle = unsafe { OpenProcess(access, false, pid) }.map_err(|e| format!("open pid {pid}: {e}"))?;
        Ok(Child { handle, pid })
    }

    pub fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        // SAFETY: valid process handle owned by self.
        let _ = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        code
    }

    /// Kills the process and waits until it is gone, so the shell window it owned is released.
    pub fn kill(&self) {
        // SAFETY: valid process handle owned by self.
        unsafe {
            let _ = TerminateProcess(self.handle, 1);
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
        // SAFETY: opens and sets an event another process created.
        unsafe {
            let h = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name)).map_err(|e| format!("{name}: {e}"))?;
            let r = SetEvent(h).map_err(|e| e.to_string());
            let _ = CloseHandle(h);
            r
        }
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
