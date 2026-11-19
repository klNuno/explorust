//! explorust: the host's file log, `%LOCALAPPDATA%\explorust\uwp-host.log` (timestamp, pid,
//! thread, line), beside the shell's log. Every phase writes here instead of stdout, so a host
//! that stops working after a Windows update can be diagnosed from this file alone. The file is
//! rotated to `uwp-host.old.log` when it passes 2 MB at the first line of a process.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Once;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("explorust")
}

#[cfg(target_os = "windows")]
fn stamp() -> String {
    // SAFETY: GetLocalTime only writes the returned struct.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(not(target_os = "windows"))]
fn stamp() -> String {
    format!("{:?}", std::time::SystemTime::now())
}

pub(crate) fn line(msg: &str) {
    static ROTATE: Once = Once::new();
    let dir = dir();
    let path = dir.join("uwp-host.log");
    ROTATE.call_once(|| {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&path, dir.join("uwp-host.old.log"));
        }
    });
    let thread = std::thread::current();
    let text = format!(
        "{} [{}:{}] {msg}\r\n",
        stamp(),
        std::process::id(),
        thread.name().unwrap_or("?")
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// `26300.9974`: the Windows build and update revision, read from the registry.
#[cfg(target_os = "windows")]
pub(crate) fn windows_build() -> String {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let mut build = [0u16; 16];
    let mut size = (build.len() * 2) as u32;
    let mut ubr = 0u32;
    let mut ubr_size = 4u32;
    // SAFETY: each output buffer is valid for the size passed with it.
    unsafe {
        let _ = RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("CurrentBuildNumber"), RRF_RT_REG_SZ, None,
            Some(build.as_mut_ptr().cast()), Some(&mut size));
        let _ = RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("UBR"), RRF_RT_REG_DWORD, None,
            Some((&mut ubr as *mut u32).cast()), Some(&mut ubr_size));
    }
    let end = build.iter().position(|&c| c == 0).unwrap_or(build.len());
    format!("{}.{ubr}", String::from_utf16_lossy(&build[..end]))
}

/// `hlog!("phase=... {x}")`: one formatted line in the host log.
macro_rules! hlog {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
