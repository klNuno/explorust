//! Append-only log in `%LOCALAPPDATA%\explorust\taskman.log` (time, pid, line).

use std::io::Write;
use std::path::PathBuf;
use windows::Win32::System::SystemInformation::GetLocalTime;

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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("taskman.log")) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Milliseconds since this process was created (the start of "start to first frame").
pub fn since_start() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let now = GetSystemTimePreciseAsFileTime();
        let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64;
        (f(now) - f(c)) / 10_000.0
    }
}

static MARKS: std::sync::Mutex<Vec<(&'static str, f64)>> = std::sync::Mutex::new(Vec::new());

/// Notes a startup step at its time since process start, for `timeline`.
pub fn mark(what: &'static str) {
    let t = since_start();
    MARKS.lock().unwrap_or_else(|p| p.into_inner()).push((what, t));
}

/// The startup steps noted so far, `step ms` in order of time.
pub fn timeline() -> String {
    let mut m = MARKS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    m.sort_by(|a, b| a.1.total_cmp(&b.1));
    m.iter().map(|(w, t)| format!("{w} {t:.1}")).collect::<Vec<_>>().join(", ")
}
