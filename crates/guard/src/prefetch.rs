//! Warm the signed-in user's symbol cache before offering to sign out.

use crate::proc::Child;
use std::path::Path;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    Security::{GetTokenInformation, TOKEN_LINKED_TOKEN, TOKEN_QUERY, TokenLinkedToken},
    System::Threading::{
        CREATE_NO_WINDOW, CREATE_PROCESS_LOGON_FLAGS, CreateProcessWithTokenW, GetCurrentProcess,
        OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    },
};
use windows::core::{HSTRING, PWSTR};

const ARG: &str = "--prefetch-symbols";
const LIMIT_MS: u32 = 35_000;

/// Called on the existing installation worker. An offline server or unavailable user token is
/// nonfatal: the shell can populate the cache at logon.
pub fn run(exe: &Path, elevated: bool) -> Result<(), String> {
    let child = if elevated {
        spawn_unelevated(exe)?
    } else {
        Child::spawn(exe, ARG)?
    };
    // SAFETY: child owns the handle for this wait.
    if unsafe { WaitForSingleObject(child.handle, LIMIT_MS) } != WAIT_OBJECT_0 {
        // SAFETY: this is only the prefetch child. Do not signal the live shell's quit event.
        let _ = unsafe { TerminateProcess(child.handle, 1) };
        return Err("download exceeded the install time limit".into());
    }
    let code = child.exit_code();
    if code != 0 {
        return Err(format!("prefetch exited {code}"));
    }
    Ok(())
}

fn spawn_unelevated(exe: &Path) -> Result<Child, String> {
    let name = HSTRING::from(exe.as_os_str());
    let cwd = HSTRING::from(
        exe.parent()
            .ok_or("shell executable has no directory")?
            .as_os_str(),
    );
    let mut cmd: Vec<u16> = format!("\"{}\" {ARG}", exe.display())
        .encode_utf16()
        .chain([0])
        .collect();
    let mut linked = TOKEN_LINKED_TOKEN::default();
    let mut len = 0;
    let mut token = HANDLE::default();
    // SAFETY: the token output and size are valid, and both token handles are closed here.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| e.to_string())?;
        let result = GetTokenInformation(
            token,
            TokenLinkedToken,
            Some((&mut linked as *mut TOKEN_LINKED_TOKEN).cast()),
            size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        result.map_err(|e| format!("signed-in user's unelevated token: {e}"))?;
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        let result = CreateProcessWithTokenW(
            linked.LinkedToken,
            CREATE_PROCESS_LOGON_FLAGS(0),
            &name,
            Some(PWSTR(cmd.as_mut_ptr())),
            CREATE_NO_WINDOW,
            None,
            &cwd,
            &startup,
            &mut process,
        );
        let _ = CloseHandle(linked.LinkedToken);
        result.map_err(|e| format!("start prefetch as the signed-in user: {e}"))?;
        let _ = CloseHandle(process.hThread);
        Ok(Child {
            handle: process.hProcess,
            pid: process.dwProcessId,
        })
    }
}
