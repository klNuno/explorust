//! The work area of the primary monitor: explorer reserves the bar's strip, and without explorer
//! nobody does. Set on start, put back on drop (shutdown, panic unwinding).

use super::log;
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SPI_SETWORKAREA, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

pub struct WorkArea {
    /// What to put back: the work area found at start, or the whole monitor when that was
    /// already our strip (a previous bar killed before it could restore it).
    old: RECT,
}

fn get() -> RECT {
    let mut r = RECT::default();
    // SAFETY: writes a RECT into a local.
    let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    r
}

pub fn set(r: RECT) {
    let mut r = r;
    // SAFETY: passes a RECT for the call; SPIF_SENDCHANGE tells windows to fit maximized windows.
    let res = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut r as *mut _ as _), SPIF_SENDCHANGE) };
    log::line(&format!("work area {},{} {}x{}: {}", r.left, r.top, r.right - r.left, r.bottom - r.top, if res.is_ok() { "set" } else { "refused" }));
}

/// Sets the work area to `want` unless it already is.
pub fn apply_rect(want: RECT) {
    let cur = get();
    if (cur.left, cur.top, cur.right, cur.bottom) != (want.left, want.top, want.right, want.bottom) {
        set(want);
    }
}

impl WorkArea {
    /// Reserves `bar_h` px at the bottom of `monitor`.
    pub fn reserve(monitor: RECT, bar_h: i32) -> WorkArea {
        let found = get();
        let ours = RECT { bottom: monitor.bottom - bar_h, ..monitor };
        let old = if (found.left, found.top, found.right, found.bottom) == (ours.left, ours.top, ours.right, ours.bottom) { monitor } else { found };
        let wa = WorkArea { old };
        wa.apply(monitor, bar_h);
        wa
    }

    pub fn apply(&self, monitor: RECT, bar_h: i32) {
        let want = RECT { bottom: monitor.bottom - bar_h, ..monitor };
        let cur = get();
        if (cur.left, cur.top, cur.right, cur.bottom) != (want.left, want.top, want.right, want.bottom) {
            set(want);
        }
    }
}

impl Drop for WorkArea {
    fn drop(&mut self) {
        log::line("restoring the work area");
        set(self.old);
    }
}
