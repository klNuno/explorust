//! Start the signed-in user's optional symbol warm-up after installation commits.

use crate::proc::Child;
use std::path::Path;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetTokenInformation, TOKEN_LINKED_TOKEN, TOKEN_QUERY, TokenLinkedToken},
    System::Threading::{
        CREATE_NO_WINDOW, CREATE_PROCESS_LOGON_FLAGS, CreateProcessWithTokenW, GetCurrentProcess,
        OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
    },
};
use windows::core::{HSTRING, PWSTR};

const ARG: &str = "--prefetch-symbols";
/// The child owns its download deadline and cleanup. Dropping the handle lets setup finish.
pub fn start(exe: &Path, elevated: bool) -> Result<(), String> {
    let child = if elevated {
        spawn_unelevated(exe)?
    } else {
        Child::spawn(exe, ARG)?
    };
    crate::log::line(&format!("install: optional symbol prefetch started (pid {})", child.pid));
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
