//! Append-only log in `%LOCALAPPDATA%\explorust\menu.log`, as the shell's log.

use std::io::Write;
use std::path::PathBuf;
use windows::Win32::System::SystemInformation::GetLocalTime;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("explorust")
}

pub fn line(msg: &str) {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: GetLocalTime only writes the returned struct.
    let t = unsafe { GetLocalTime() };
    let text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}] {msg}\r\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds,
        std::process::id()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("menu.log")) {
        let _ = f.write_all(text.as_bytes());
    }
}
