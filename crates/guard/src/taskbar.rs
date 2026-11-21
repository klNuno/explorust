//! Who owns the session's taskbar (`Shell_TrayWnd`) and whether it answers. The guard's taskbar
//! watchdog asks explorust's; the fallback and the uninstall wait for explorer's before they say
//! Windows is back.

use crate::policy::{ANSWER_WITHIN, Probe};
use crate::proc::Child;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
};
use windows::core::{PWSTR, w};

/// Every `Shell_TrayWnd` with its process id.
fn trays() -> Vec<(HWND, u32)> {
    let mut out = Vec::new();
    let mut after: Option<HWND> = None;
    // SAFETY: window lookups; a stale handle only fails them.
    unsafe {
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), None) {
            if h.is_invalid() {
                break;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            out.push((h, pid));
            after = Some(h);
        }
    }
    out
}

fn answers(h: HWND, within: Duration) -> bool {
    let mut out = 0usize;
    // SAFETY: a bounded send of a message every window accepts.
    let r = unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, within.as_millis() as u32, Some(&mut out)) };
    r.0 != 0
}

/// The file name of a process's image, lowercase.
fn image_name(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed; the buffer has the length given.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(path.rsplit('\\').next().unwrap_or_default().to_ascii_lowercase())
    }
}

/// Asks explorust's (`pid`) taskbar window for an answer.
pub fn probe(pid: u32) -> Probe {
    match trays().into_iter().find(|(_, p)| *p == pid) {
        None => Probe::Missing,
        Some((h, _)) if answers(h, ANSWER_WITHIN) => Probe::Answered,
        Some(_) => Probe::NoAnswer,
    }
}

/// The pid of an explorer.exe whose taskbar answers, if any.
pub fn explorer() -> Option<u32> {
    trays()
        .into_iter()
        .find(|(h, pid)| image_name(*pid).as_deref() == Some("explorer.exe") && answers(*h, Duration::from_secs(2)))
        .map(|(_, pid)| pid)
}

/// True when some taskbar window exists, whoever owns it.
pub fn any() -> bool {
    !trays().is_empty()
}

pub enum Start {
    /// explorer's taskbar answers; the pid owns it (usually the child, sometimes an explorer that
    /// was already running and claimed the shell).
    Up(u32),
    /// The child ended and no explorer taskbar came within a few seconds.
    Exited(u32),
    /// Still no explorer taskbar after `limit`.
    Timeout,
}

/// Waits for a freshly started explorer to own an answering taskbar.
pub fn wait_explorer(child: &Child, limit: Duration) -> Start {
    let t0 = Instant::now();
    let mut ended: Option<Instant> = None;
    loop {
        if let Some(pid) = explorer() {
            return Start::Up(pid);
        }
        if ended.is_none() && child.ended() {
            ended = Some(Instant::now());
        }
        if let Some(at) = ended
            && at.elapsed() >= Duration::from_secs(5)
        {
            return Start::Exited(child.exit_code());
        }
        if t0.elapsed() >= limit {
            return Start::Timeout;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Waits up to `limit` for any explorer taskbar.
pub fn wait_any_explorer(limit: Duration) -> Option<u32> {
    let t0 = Instant::now();
    loop {
        if let Some(pid) = explorer() {
            return Some(pid);
        }
        if t0.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
