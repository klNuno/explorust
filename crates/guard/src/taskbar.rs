//! Who owns the session's taskbar (`Shell_TrayWnd`) and whether it answers. The guard's taskbar
//! watchdog asks explorust's; the fallback and the uninstall wait for explorer's before they say
//! Windows is back.

use crate::policy::{ANSWER_WITHIN, Probe};
use crate::proc::Child;
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetShellWindow, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
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

pub fn answers(h: HWND, within: Duration) -> bool {
    let mut out = 0usize;
    // SAFETY: a bounded send of a message every window accepts.
    let r = unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, within.as_millis() as u32, Some(&mut out)) };
    r.0 != 0
}

pub fn image_path(pid: u32) -> Option<String> {
    // SAFETY: the query handle is closed after it is used.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let image = process_image_path(handle);
    let _ = unsafe { CloseHandle(handle) };
    image
}

pub(crate) fn process_image_path(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the caller holds the process handle, and the buffer has the stated size.
    unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
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
    explorer_except(None)
}

pub fn explorer_except(retiring: Option<u32>) -> Option<u32> {
    trays()
        .into_iter()
        .find(|(h, pid)| {
            retiring != Some(*pid)
                && image_path(*pid).is_some_and(|path| path.rsplit('\\').next().is_some_and(|name| name.eq_ignore_ascii_case("explorer.exe")))
                && answers(*h, Duration::from_secs(2))
        })
        .map(|(_, pid)| pid)
}

pub fn explorer_window() -> Option<(HWND, u32)> {
    explorer_window_except(None)
}

pub fn explorer_window_except(excluded: Option<u32>) -> Option<(HWND, u32)> {
    trays().into_iter().find(|(_, pid)| Some(*pid) != excluded && is_explorer(*pid))
}

pub fn explorer_window_for(pid: u32) -> Option<HWND> {
    tray_for(pid).filter(|_| is_explorer(pid))
}

pub fn is_explorer(pid: u32) -> bool {
    image_path(pid).is_some_and(|path| path.eq_ignore_ascii_case(&crate::supervise::explorer_path().to_string_lossy()))
}

/// The installed shell's taskbar owner. A dev shell launched elsewhere would reload its own exe.
pub fn explorust(exe: &Path) -> Option<u32> {
    let expected = exe.to_string_lossy();
    trays().into_iter().find_map(|(_, pid)| {
        image_path(pid).filter(|path| path.eq_ignore_ascii_case(&expected)).map(|_| pid)
    })
}

/// A registered desktop can have an owner before its taskbar appears.
fn desktop_window_owner() -> Option<u32> {
    let mut pid = 0;
    // SAFETY: only query the PID of the desktop window Windows currently publishes.
    unsafe { GetWindowThreadProcessId(GetShellWindow(), Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

pub fn tray_for(pid: u32) -> Option<HWND> {
    trays().into_iter().find_map(|(window, owner)| (owner == pid).then_some(window))
}

pub fn owns_tray(pid: u32) -> bool {
    tray_for(pid).is_some()
}

/// True when some taskbar window exists, whoever owns it.
pub fn any() -> bool {
    !trays().is_empty()
}

pub fn desktop_owner() -> Option<u32> {
    desktop_window_owner().or_else(|| trays().first().map(|&(_, pid)| pid))
}

pub fn explorer_file_operations() -> bool {
    // SAFETY: this is only a lookup on the current desktop.
    unsafe { FindWindowExW(None, None, w!("OperationStatusWindow"), None).is_ok() }
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
