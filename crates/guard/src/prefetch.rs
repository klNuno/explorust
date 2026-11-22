//! Launch from the interactive shell's token, including the optional symbol warm-up after install.

use crate::{account, proc::Child};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{
        GetTokenInformation, TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    },
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
            OpenProcessToken, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROCESS_CREATE_PROCESS,
            PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOEXW,
            UpdateProcThreadAttribute,
        },
    },
    UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId},
};
use windows::core::{HSTRING, PWSTR};

const ARG: &str = "--prefetch-symbols";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn warm_up_uses_a_trusted_persistent_directory() {
        let shell = PathBuf::from(r"C:\Program Files\explorust\explorust.exe");
        assert_eq!(
            working_directory(&shell).unwrap(),
            Path::new(r"C:\Program Files\explorust")
        );
        assert!(working_directory(Path::new("explorust.exe")).is_err());
    }

    #[test]
    #[ignore = "run elevated in the configured interactive Windows guest"]
    fn starts_installed_prefetch_with_the_signed_in_users_token() {
        assert!(crate::install::elevated());
        let child = spawn_unelevated(&crate::install::dir().join("explorust.exe")).unwrap();
        let token = process_token(child.handle).unwrap();
        let token = HANDLE(token.as_raw_handle());
        assert!(!token_elevated(token).unwrap());
        assert_eq!(
            account::token_sid(token).unwrap(),
            account::own_sid().unwrap()
        );
        println!(
            "Prefetch pid {} uses the signed-in user's unelevated token",
            child.pid
        );
    }
}

fn working_directory(exe: &Path) -> Result<&Path, String> {
    exe.parent()
        .filter(|parent| parent.is_absolute())
        .ok_or_else(|| "prefetch needs an absolute installed executable".into())
}

/// The child owns its download deadline and cleanup. Dropping the handle lets setup finish.
pub fn start(exe: &Path, elevated: bool) -> Result<(), String> {
    let child = if elevated {
        spawn_unelevated(exe)?
    } else {
        Child::spawn(exe, ARG)?
    };
    crate::log::line(&format!(
        "install: optional symbol prefetch started (pid {})",
        child.pid
    ));
    Ok(())
}

fn spawn_unelevated(exe: &Path) -> Result<Child, String> {
    let mut pid = 0;
    // SAFETY: query this session's registered shell.
    if unsafe { GetWindowThreadProcessId(GetShellWindow(), Some(&mut pid)) } == 0 {
        return Err("could not find the signed-in user's desktop shell".into());
    }
    spawn_as_shell(pid, exe, ARG)
}

pub fn spawn_as_shell(pid: u32, exe: &Path, args: &str) -> Result<Child, String> {
    let working_directory = working_directory(exe)?;
    let name = HSTRING::from(exe.as_os_str());
    let cwd = HSTRING::from(working_directory.as_os_str());
    let mut cmd: Vec<u16> = format!("\"{}\" {args}", exe.display())
        .encode_utf16()
        .chain([0])
        .collect();
    let parent = desktop_shell(pid)?;
    let parent = HANDLE(parent.as_raw_handle());
    let token = process_token(parent)?;
    let token = HANDLE(token.as_raw_handle());
    if token_elevated(token)?
        || !account::own_sid()
            .is_some_and(|sid| account::token_sid(token).as_deref() == Some(sid.as_str()))
    {
        return Err("the desktop shell is not running as the signed-in unelevated user".into());
    }
    // Windows inherits the specified parent's primary token. TokenLinkedToken from this
    // installer can be an identification-only impersonation token, unsuitable for starting
    // a process. Supply the interactive user's environment and the installed executable's
    // directory explicitly, since the installer may be running from an admin-only staging dir.
    // SAFETY: the shell, token, parent attribute storage and strings outlive CreateProcessW.
    unsafe {
        let mut bytes = 0;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut bytes);
        if bytes == 0 {
            return Err("could not size the desktop process attributes".into());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut bytes)
            .map_err(|e| format!("initialize desktop process attributes: {e}"))?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attrs;
        let mut environment = std::ptr::null_mut();
        let mut process = PROCESS_INFORMATION::default();
        let result = (|| {
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
                Some((&parent as *const HANDLE).cast()),
                size_of::<HANDLE>(),
                None,
                None,
            )
            .map_err(|e| format!("set the desktop shell as parent: {e}"))?;
            CreateEnvironmentBlock(&mut environment, Some(token), false)
                .map_err(|e| format!("signed-in user's environment: {e}"))?;
            CreateProcessW(
                &name,
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                Some(environment.cast_const()),
                &cwd,
                &startup.StartupInfo,
                &mut process,
            )
            .map_err(|e| format!("start {} as the signed-in user: {e}", exe.display()))
        })();
        if !environment.is_null() {
            let _ = DestroyEnvironmentBlock(environment);
        }
        DeleteProcThreadAttributeList(attrs);
        result?;
        let _ = CloseHandle(process.hThread);
        Ok(Child {
            handle: process.hProcess,
            pid: process.dwProcessId,
        })
    }
}

fn desktop_shell(pid: u32) -> Result<OwnedHandle, String> {
    // SAFETY: take ownership of the opened handle.
    unsafe {
        let handle = OpenProcess(
            PROCESS_CREATE_PROCESS | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .map_err(|e| format!("open desktop shell pid {pid}: {e}"))?;
        Ok(OwnedHandle::from_raw_handle(handle.0))
    }
}

fn process_token(process: HANDLE) -> Result<OwnedHandle, String> {
    let mut raw = HANDLE::default();
    // SAFETY: the token output is valid, and we take ownership of the returned handle.
    unsafe {
        OpenProcessToken(process, TOKEN_QUERY | TOKEN_DUPLICATE, &mut raw)
            .map_err(|e| format!("open process token: {e}"))?;
        Ok(OwnedHandle::from_raw_handle(raw.0))
    }
}

fn token_elevated(token: HANDLE) -> Result<bool, String> {
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0;
    // SAFETY: elevation is returned into a correctly sized buffer.
    unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .map_err(|e| format!("process token elevation: {e}"))?;
    Ok(elevation.TokenIsElevated != 0)
}
