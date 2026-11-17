//! Clock and date strings formatted by the user's locale, as the taskbar shows them.

use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
use windows_core::PCWSTR;

/// (time, date), e.g. ("23:18", "17/11/2026") for fr-FR.
pub fn now() -> (String, String) {
    unsafe {
        let mut t = [0u16; 64];
        let mut d = [0u16; 64];
        let tn = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut t));
        let dn = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, None, PCWSTR::null(), Some(&mut d), PCWSTR::null());
        let s = |b: &[u16], n: i32| String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize]);
        (s(&t, tn), s(&d, dn))
    }
}

/// Milliseconds until the next minute boundary, for redraw scheduling.
pub fn ms_to_next_minute() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    60_000 - (now.as_millis() % 60_000) as u64
}
