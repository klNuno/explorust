//! explorust-setup.exe: the one file a user downloads. Windows asks for administrator rights when
//! it opens (manifest), then it unpacks the shell's files into a temporary folder and hands over to
//! the guard's `--setup` (question, install into Program Files, Apps entry, Start menu shortcuts,
//! sign-out offer). The temporary folder is deleted afterwards. It is a new folder under
//! `%SystemRoot%\Temp` that only administrators and SYSTEM may change, created in one call with that
//! protection: the user's own non-elevated programs cannot swap a file the elevated guard then
//! copies into Program Files (the LocalSystem index service among them).

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
    use std::path::PathBuf;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};

    /// Full control for Administrators and SYSTEM only, not inherited from the parent.
    const DACL: &str = "D:P(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)";

    /// Creates a fresh staging folder with `DACL`; fails rather than reuse an existing path.
    fn staging() -> Result<PathBuf, String> {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Temp");
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let dir = root.join(format!("explorust-setup-{}-{nanos:08x}", std::process::id()));
        // SAFETY: the descriptor is freed after CreateDirectoryW; the path outlives the call.
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(DACL), SDDL_REVISION_1, &mut sd, None)
                .map_err(|e| format!("security descriptor: {e}"))?;
            let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
            let r = CreateDirectoryW(&HSTRING::from(dir.as_os_str()), Some(&sa));
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            r.map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(dir)
    }

    fn fail(msg: &str) -> u32 {
        // SAFETY: plain strings that outlive the call.
        unsafe { MessageBoxW(None, &HSTRING::from(msg), w!("explorust"), MB_OK | MB_ICONERROR) };
        1
    }

    pub fn run() -> u32 {
        if !FILES.iter().any(|(n, _)| *n == "explorust-guard.exe") {
            return fail("This installer was built without its files (EXPLORUST_PAYLOAD).");
        }
        let dir = match staging() {
            Ok(d) => d,
            Err(e) => return fail(&e),
        };
        for (name, bytes) in FILES {
            // create_new: nothing in the fresh folder may be replaced, only written once.
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(name))
                .and_then(|mut f| std::io::Write::write_all(&mut f, bytes));
            if let Err(e) = written {
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
