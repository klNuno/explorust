//! Monitors covered by the foreground application. Shell surfaces and maximized framed windows
//! leave AppBars and notification delivery enabled.

use super::appbars::to_rect;
use crate::appbar::Rect;
use crate::settings::{fullscreen_cover, maximized_framed};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, HDC, HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromRect,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::BOOL;

const SHELL_HOSTS: [&str; 6] = [
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
    "textinputhost.exe",
    "lockapp.exe",
];

pub fn monitor_at(bounds: &RECT) -> HMONITOR {
    // SAFETY: reads the supplied screen coordinates.
    unsafe { MonitorFromRect(bounds, MONITOR_DEFAULTTONULL) }
}

pub fn covered_monitors(fg: HWND) -> Vec<HMONITOR> {
    let mut monitors = Vec::new();
    if fg.is_invalid() {
        return monitors;
    }
    let mut class = [0u16; 64];
    // SAFETY: reads the class and bounds of a window into local buffers.
    let n = unsafe { GetClassNameW(fg, &mut class) };
    let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    if matches!(
        class.as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "explorust.taskbar.popup"
    ) {
        return monitors;
    }
    let mut bounds = RECT::default();
    unsafe {
        if !IsWindowVisible(fg).as_bool()
            || IsIconic(fg).as_bool()
            || GetWindowRect(fg, &mut bounds).is_err()
        {
            return monitors;
        }
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        if maximized_framed(
            IsZoomed(fg).as_bool(),
            style & WS_CAPTION.0 == WS_CAPTION.0,
            style & WS_THICKFRAME.0 != 0,
        ) {
            return monitors;
        }
        let mut query = Coverage {
            window: to_rect(bounds),
            monitors: &mut monitors,
        };
        // Pass no clip rectangle so the callback sees each monitor's full desktop bounds.
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_covered),
            LPARAM(&mut query as *mut Coverage<'_> as isize),
        );
    }
    if !monitors.is_empty() && !is_application(fg) {
        monitors.clear();
    }
    monitors
}

struct Coverage<'a> {
    window: Rect,
    monitors: &'a mut Vec<HMONITOR>,
}

unsafe extern "system" fn collect_covered(
    monitor: HMONITOR,
    _: HDC,
    bounds: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: EnumDisplayMonitors passes a monitor rect and the live context from covered_monitors.
    let query = unsafe { &mut *(data.0 as *mut Coverage<'_>) };
    if fullscreen_cover(query.window, to_rect(unsafe { *bounds }), false) {
        query.monitors.push(monitor);
    }
    BOOL(1)
}

fn is_application(fg: HWND) -> bool {
    let mut pid = 0;
    // SAFETY: reads the owner PID into a local.
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    let exe = shelldata::items::process_path(pid).unwrap_or_default();
    let file = exe
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&exe)
        .to_lowercase();
    !SHELL_HOSTS.contains(&file.as_str())
}
