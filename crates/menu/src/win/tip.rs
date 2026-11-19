//! The top row's tooltips (Cut, Copy, Rename, Share, Delete): a tracking tooltip of comctl32 in
//! the dark theme, shown under the hovered button after the hover delay.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    SetWindowTheme, TOOLTIPS_CLASSW, TTF_ABSOLUTE, TTF_TRACK, TTM_ADDTOOLW, TTM_TRACKACTIVATE, TTM_TRACKPOSITION, TTM_UPDATETIPTEXTW,
    TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PWSTR, w};

pub struct Tip {
    hwnd: HWND,
    owner: HWND,
    text: Vec<u16>,
    pub shown: bool,
}

impl Tip {
    pub fn new(owner: HWND) -> Option<Self> {
        // SAFETY: a tooltip window owned by our root popup, with one tracking tool.
        unsafe {
            let inst = GetModuleHandleW(None).ok()?;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                TOOLTIPS_CLASSW,
                w!(""),
                WINDOW_STYLE(WS_POPUP.0 | TTS_NOPREFIX | TTS_ALWAYSTIP),
                0,
                0,
                0,
                0,
                Some(owner),
                None,
                Some(inst.into()),
                None,
            )
            .ok()?;
            let _ = SetWindowTheme(hwnd, w!("DarkMode_Explorer"), None);
            let mut tip = Tip { hwnd, owner, text: vec![0], shown: false };
            let ti = tip.info();
            SendMessageW(hwnd, TTM_ADDTOOLW, Some(WPARAM(0)), Some(LPARAM(&ti as *const _ as isize)));
            Some(tip)
        }
    }

    fn info(&mut self) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_TRACK | TTF_ABSOLUTE,
            hwnd: self.owner,
            uId: 1,
            lpszText: PWSTR(self.text.as_mut_ptr()),
            ..Default::default()
        }
    }

    /// Shows `text` with its top centre at screen point (x, y).
    pub fn show(&mut self, text: &str, x: i32, y: i32) {
        self.text = text.encode_utf16().chain([0]).collect();
        let ti = self.info();
        // SAFETY: messages to our tooltip; the info and its text outlive each call.
        unsafe {
            SendMessageW(self.hwnd, TTM_UPDATETIPTEXTW, Some(WPARAM(0)), Some(LPARAM(&ti as *const _ as isize)));
            SendMessageW(self.hwnd, TTM_TRACKACTIVATE, Some(WPARAM(1)), Some(LPARAM(&ti as *const _ as isize)));
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            let x = x - (r.right - r.left) / 2;
            let pos = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
            SendMessageW(self.hwnd, TTM_TRACKPOSITION, Some(WPARAM(0)), Some(LPARAM(pos as isize)));
        }
        self.shown = true;
    }

    pub fn hide(&mut self) {
        if !self.shown {
            return;
        }
        let ti = self.info();
        // SAFETY: deactivates our tooltip's tool.
        unsafe {
            SendMessageW(self.hwnd, TTM_TRACKACTIVATE, Some(WPARAM(0)), Some(LPARAM(&ti as *const _ as isize)));
        }
        self.shown = false;
    }
}
