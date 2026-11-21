//! explorust-keys.exe: the installed shell's keyboard hook, in a process of its own so that only
//! this exe carries UI access (`uiAccess="true"`, signed, in Program Files). explorust starts it
//! with `--shell <pid>`; it ends when that explorust ends or sets `Local\explorust-keys-quit-<pid>`.
//! It starts no process: shell actions are posted to explorust. It also answers `Command::Activate`
//! over a named pipe, so the parts bring windows forward over elevated ones. Log: `%LOCALAPPDATA%\explorust\hotkeys.log`.
//!
//! Exit codes: 0 explorust ended or asked it to stop, 2 bad arguments, 3 another instance kept the
//! session's mutex, 4 the hook was refused.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    std::process::exit(win::main());
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("explorust-keys runs on Windows only");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
mod win {
    use std::io::Write;
    use std::path::PathBuf;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUIAccess};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, INFINITE, OpenEventW, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE,
        SYNCHRONIZATION_SYNCHRONIZE, WaitForMultipleObjects, WaitForSingleObject,
    };
    use windows::core::HSTRING;

    pub fn line(msg: &str) {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join("explorust");
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: GetLocalTime only writes the returned struct.
        let t = unsafe { GetLocalTime() };
        let text = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}] {msg}\r\n",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds,
            std::process::id()
        );
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("hotkeys.log")) {
            let _ = f.write_all(text.as_bytes());
        }
    }

    fn ui_access() -> bool {
        // SAFETY: queries this process's own token into a u32 and closes it.
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut v = 0u32;
            let mut len = 0u32;
            let ok = GetTokenInformation(token, TokenUIAccess, Some((&mut v as *mut u32).cast()), 4, &mut len).is_ok();
            let _ = CloseHandle(token);
            ok && v != 0
        }
    }

    pub fn main() -> i32 {
        let mut args = std::env::args().skip(1);
        let shell = match (args.next().as_deref(), args.next().and_then(|p| p.parse::<u32>().ok())) {
            (Some("--shell"), Some(pid)) => pid,
            _ => {
                line("usage: explorust-keys --shell <explorust pid>");
                return 2;
            }
        };
        // SAFETY: handles opened here live until the process ends.
        unsafe {
            // One hook per session: a restarted explorust may start this before the old one is gone.
            let Ok(mutex) = CreateMutexW(None, false, &HSTRING::from(r"Local\explorust-keys")) else {
                line("mutex not created");
                return 3;
            };
            let w = WaitForSingleObject(mutex, 3000);
            if w != WAIT_OBJECT_0 && w != WAIT_ABANDONED {
                line("another explorust-keys holds the session: exiting");
                return 3;
            }
            let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, shell) else {
                line(&format!("explorust pid {shell} not found: exiting"));
                return 0;
            };
            let quit = OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(format!(r"Local\explorust-keys-quit-{shell}")));
            if let Err(e) = keys::spawn_for_shell(shell, line) {
                line(&format!("{e}: exiting"));
                return 4;
            }
            line(&format!("hotkeys armed for explorust pid {shell} (UIAccess={})", ui_access() as u8));
            let mut handles = vec![process];
            handles.extend(quit.ok());
            let r = WaitForMultipleObjects(&handles, false, INFINITE);
            let why = if r == WAIT_OBJECT_0 { "explorust ended" } else { "explorust asked it to stop" };
            line(&format!("{why}: exiting"));
        }
        0
    }
}
