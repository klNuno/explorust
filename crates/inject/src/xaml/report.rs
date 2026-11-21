//! The TAP's log: lines go to the loader's message-only window in explorust by `WM_COPYDATA` (the
//! loader writes them to `inject.log`), since a packaged host's AppData writes may be redirected.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, HWND_MESSAGE, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA};
use windows::core::{HSTRING, PCWSTR};

/// `COPYDATASTRUCT::dwData` of a TAP log batch: UTF-8 lines.
pub const COPYDATA_TAG: usize = 0x5841_4d4c;

/// Sends lines (joined by `\n`) to the loader; dropped when explorust's loader is not there.
pub fn send(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let text = lines.join("\n");
    // SAFETY: a window lookup and a synchronous send whose buffer outlives the call.
    unsafe {
        let Ok(to) = FindWindowExW(Some(HWND_MESSAGE), None, &HSTRING::from(crate::WINDOW_CLASS), PCWSTR::null()) else { return };
        let cds = COPYDATASTRUCT { dwData: COPYDATA_TAG, cbData: text.len() as u32, lpData: text.as_ptr() as *mut _ };
        let _ = SendMessageTimeoutW(
            to,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            500,
            None,
        );
    }
}

pub fn line(msg: String) {
    send(&[msg]);
}
