//! The DLL side: `explorust_inject_hook` runs inside the host, on the UI thread that owns the
//! target window, as a `WH_GETMESSAGE` hook of that thread only. It does nothing but pass the
//! message on, except for the loader's `explorust.inject.style` message, which styles the window.
//! A panic never leaves this function.

use crate::dark;
use crate::target::{self, Kind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW};
use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EnumChildWindows, FindWindowExW, GetClassNameW, HC_ACTION, HWND_MESSAGE, MSG, PM_REMOVE,
    PostMessageW, RegisterWindowMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};
use windows::core::{BOOL, PCWSTR, w};

static STYLE: AtomicU32 = AtomicU32::new(0);

fn style_msg() -> u32 {
    let m = STYLE.load(Ordering::Relaxed);
    if m != 0 {
        return m;
    }
    // SAFETY: a constant string.
    let m = unsafe { RegisterWindowMessageW(w!("explorust.inject.style")) };
    STYLE.store(m, Ordering::Relaxed);
    m
}

#[unsafe(no_mangle)]
pub extern "system" fn explorust_inject_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 == PM_REMOVE.0 as usize && lparam.0 != 0 {
        // SAFETY: for WH_GETMESSAGE, lParam points at the MSG being retrieved.
        let msg = unsafe { &*(lparam.0 as *const MSG) };
        if msg.message != 0 && msg.message == style_msg() {
            let (hwnd, kind) = (msg.hwnd, msg.wParam.0);
            let _ = std::panic::catch_unwind(|| on_style(hwnd, kind));
        }
    }
    // SAFETY: passes the hook chain on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn on_style(hwnd: HWND, kind: usize) {
    if crate::switched_off() {
        return;
    }
    let t = Instant::now();
    match Kind::from_usize(kind) {
        Some(Kind::FileDialog) => file_dialog(hwnd),
        Some(Kind::Settings) => {
            let _ = dark::frame(hwnd, DWMSBT_TRANSIENTWINDOW);
        }
        None => return,
    }
    report(t.elapsed().as_micros() as usize);
}

/// Dark theme on the dialog and every control in it, Mica on its frame, then one repaint.
fn file_dialog(hwnd: HWND) {
    dark::force_dark_app();
    dark::allow_dark_window(hwnd);
    let _ = dark::frame(hwnd, DWMSBT_MAINWINDOW);
    // SAFETY: the callback only touches the child it is given.
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(child), LPARAM(0));
        let mut r = 0usize;
        let s = w!("ImmersiveColorSet");
        let _ = SendMessageTimeoutW(hwnd, WM_SETTINGCHANGE, WPARAM(0), LPARAM(s.0 as isize), SMTO_ABORTIFHUNG, 200, Some(&mut r));
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

unsafe extern "system" fn child(h: HWND, _: LPARAM) -> BOOL {
    let _ = std::panic::catch_unwind(|| {
        dark::allow_dark_window(h);
        let mut buf = [0u16; 64];
        // SAFETY: the buffer outlives both calls; same-thread child windows answer at once.
        unsafe {
            let n = GetClassNameW(h, &mut buf) as usize;
            let class = String::from_utf16_lossy(&buf[..n]);
            // SetWindowTheme sends the control its own WM_THEMECHANGED.
            let theme = match target::dark_theme_of(&class) {
                Some("DarkMode_CFD") => w!("DarkMode_CFD"),
                Some(_) => w!("DarkMode_Explorer"),
                None => return,
            };
            let _ = SetWindowTheme(h, theme, PCWSTR::null());
        }
    });
    true.into()
}

/// Tells the loader how long the styling took on the host's thread.
fn report(us: usize) {
    // SAFETY: plain window lookups and a posted message.
    unsafe {
        let done = RegisterWindowMessageW(w!("explorust.inject.done"));
        if let Ok(w) = FindWindowExW(Some(HWND_MESSAGE), None, w!("explorust.inject"), PCWSTR::null()) {
            let _ = PostMessageW(Some(w), done, WPARAM(us), LPARAM(std::process::id() as isize));
        }
    }
}
