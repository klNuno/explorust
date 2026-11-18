//! Monitors, work areas and the virtual screen, in physical pixels (the thread is per-monitor
//! DPI aware v2, set by the process).

use crate::layout::Rect;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use windows_core::BOOL;

pub struct Monitor {
    pub rect: Rect,
    pub work: Rect,
    pub primary: bool,
}

pub fn rect(r: &RECT) -> Rect {
    Rect::from_edges(r.left, r.top, r.right, r.bottom)
}

pub fn virtual_screen() -> Rect {
    // SAFETY: plain metric queries.
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `Vec<Monitor>` passed by `monitors` for the length of the call.
    let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` is a MONITORINFO with its size set.
    if unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
        out.push(Monitor {
            rect: rect(&mi.rcMonitor),
            work: rect(&mi.rcWork),
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    BOOL(1)
}

pub fn monitors() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only runs during this call, while `out` lives.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut _ as isize)) };
    out
}
