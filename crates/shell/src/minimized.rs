//! Minimized windows: explorer adds `ARW_HIDE` to the session's minimized metrics when its tray
//! starts, so a minimized window leaves the screen instead of showing as a small title bar above
//! the taskbar. The change is for the session only (no `SPIF_UPDATEINIFILE`), and the bit is
//! cleared again on a clean exit, like explorer does.

use crate::log;
use windows::Win32::UI::WindowsAndMessaging::{
    ARW_HIDE, MINIMIZEDMETRICS, MINIMIZEDMETRICS_ARRANGE, SPI_GETMINIMIZEDMETRICS, SPI_SETMINIMIZEDMETRICS,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

fn get() -> Option<MINIMIZEDMETRICS> {
    let mut mm = MINIMIZEDMETRICS { cbSize: size_of::<MINIMIZEDMETRICS>() as u32, ..Default::default() };
    // SAFETY: the struct and its size, as the call documents.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETMINIMIZEDMETRICS,
            mm.cbSize,
            Some((&raw mut mm).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok().then_some(mm)
}

fn set_hidden(hidden: bool) {
    let Some(mut mm) = get() else {
        log::line("minimized metrics: SPI_GETMINIMIZEDMETRICS failed");
        return;
    };
    let arrange = if hidden { mm.iArrange.0 | ARW_HIDE } else { mm.iArrange.0 & !ARW_HIDE };
    if arrange == mm.iArrange.0 {
        return;
    }
    mm.iArrange = MINIMIZEDMETRICS_ARRANGE(arrange);
    // SAFETY: the struct and its size; no ini update, the change lasts for the session only.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_SETMINIMIZEDMETRICS,
            mm.cbSize,
            Some((&raw mut mm).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    log::line(&format!("minimized metrics: ARW_HIDE {} ({:?})", if hidden { "set" } else { "cleared" }, ok));
}

/// At start: minimized windows leave the screen.
pub fn hide() {
    set_hidden(true);
}

/// At a clean exit: back to Windows' default with no shell.
pub fn restore() {
    set_hidden(false);
}
