//! Finds Windows' own toast popup: a visible, uncloaked top-level window of
//! ShellExperienceHost.exe shaped like a toast (`model::is_toast_popup`).

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, PWSTR};

/// A visible window of ShellExperienceHost.
pub struct HostWindow {
    pub class: String,
    pub title: String,
    pub rect: RECT,
    pub scale: f32,
}

impl HostWindow {
    pub fn describe(&self) -> String {
        let r = self.rect;
        format!("'{}' '{}' {}x{} at {},{} scale {}", self.class, self.title, r.right - r.left, r.bottom - r.top, r.left, r.top, self.scale)
    }
}

fn image_is_host(pid: u32) -> bool {
    // SAFETY: a query-only handle, closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { return false };
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    // SAFETY: writes at most `len` characters into the local buffer.
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    // SAFETY: the handle opened above.
    unsafe { let _ = CloseHandle(h); }
    ok && String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase().ends_with("\\shellexperiencehost.exe")
}

fn cloaked(hwnd: HWND) -> bool {
    let mut v = 0u32;
    // SAFETY: reads one u32 attribute into a local.
    let r = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v as *mut _ as _, 4) };
    r.is_ok() && v != 0
}

unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the Vec passed by `host_windows` for the length of EnumWindows.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HostWindow>) };
    // SAFETY: plain queries on a window handle EnumWindows gave.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || cloaked(hwnd) {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !image_is_host(pid) {
            return true.into();
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let mut title = [0u16; 128];
        let m = GetWindowTextW(hwnd, &mut title).max(0) as usize;
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let dpi = GetDpiForWindow(hwnd);
        out.push(HostWindow {
            class: String::from_utf16_lossy(&class[..n]),
            title: String::from_utf16_lossy(&title[..m]),
            rect,
            scale: if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 },
        });
    }
    true.into()
}

/// The visible, uncloaked top-level windows of ShellExperienceHost.exe.
pub fn host_windows() -> Vec<HostWindow> {
    let mut out: Vec<HostWindow> = Vec::new();
    // SAFETY: the callback only touches `out` while EnumWindows runs.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut out as *mut _ as isize)) };
    out
}

/// Whether one of them is Windows' toast popup, with the host's windows described for the log.
pub fn windows_toast_visible() -> (bool, String) {
    let mut work = RECT::default();
    // SAFETY: reads the work area into a local.
    let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), Default::default()) };
    let wins = host_windows();
    let seen = wins.iter().any(|w| {
        let r = w.rect;
        crate::model::is_toast_popup((r.left, r.top, r.right, r.bottom), (work.left, work.top, work.right, work.bottom), w.scale)
    });
    let list = if wins.is_empty() { "none".to_string() } else { wins.iter().map(HostWindow::describe).collect::<Vec<_>>().join("; ") };
    (seen, list)
}
