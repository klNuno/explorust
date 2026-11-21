//! Append-only log in `%LOCALAPPDATA%\explorust\inject.log` (the loader's side; the DLL reports to
//! the loader instead, since a packaged host's AppData writes may be redirected).

use std::io::Write;
use std::path::PathBuf;
use windows::Win32::System::SystemInformation::GetLocalTime;

pub fn line(msg: &str) {
    lines("", std::iter::once(msg));
}

/// Several lines with one timestamp, each after `prefix`, in one append.
pub fn lines<'a>(prefix: &str, msgs: impl Iterator<Item = &'a str>) {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("explorust");
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: GetLocalTime only writes the returned struct.
    let t = unsafe { GetLocalTime() };
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}] {prefix}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds,
        std::process::id()
    );
    let mut text = String::new();
    for m in msgs {
        text.push_str(&stamp);
        text.push_str(m);
        text.push_str("\r\n");
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("inject.log")) {
        let _ = f.write_all(text.as_bytes());
    }
}
