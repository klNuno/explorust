//! explorust-setup.exe: the one file a user downloads. Windows asks for administrator rights when
//! it opens (manifest), then it unpacks the shell's files into a temporary folder and hands over to
//! the guard's `--setup` (question, install into Program Files, Apps entry, Start menu shortcuts,
//! sign-out offer). The temporary folder is deleted afterwards.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

#[cfg(target_os = "windows")]
fn main() {
    std::process::exit(win::run() as i32);
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("explorust-setup runs on Windows; payload of {} files", FILES.len());
}

#[cfg(target_os = "windows")]
mod win {
    use super::FILES;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};

    fn fail(msg: &str) -> u32 {
        // SAFETY: plain strings that outlive the call.
        unsafe { MessageBoxW(None, &HSTRING::from(msg), w!("explorust"), MB_OK | MB_ICONERROR) };
        1
    }

    pub fn run() -> u32 {
        if !FILES.iter().any(|(n, _)| *n == "explorust-guard.exe") {
            return fail("This installer was built without its files (EXPLORUST_PAYLOAD).");
        }
        let dir = std::env::temp_dir().join(format!("explorust-setup-{}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return fail(&format!("{}: {e}", dir.display()));
        }
        for (name, bytes) in FILES {
            if let Err(e) = std::fs::write(dir.join(name), bytes) {
                let _ = std::fs::remove_dir_all(&dir);
                return fail(&format!("{name}: {e}"));
            }
        }
        // This process is elevated, so the guard is too.
        let code = match std::process::Command::new(dir.join("explorust-guard.exe")).arg("--setup").current_dir(&dir).status() {
            Ok(s) => s.code().unwrap_or(1) as u32,
            Err(e) => fail(&format!("explorust-guard.exe: {e}")),
        };
        // The guard has exited; a scanner may still hold a file for a moment.
        for _ in 0..10 {
            if std::fs::remove_dir_all(&dir).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        code
    }
}
