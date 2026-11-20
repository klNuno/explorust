//! The DLL side: `explorust_inject_hook` runs inside the host, on the UI thread that owns the
//! target window, as a `WH_CALLWNDPROC` hook of that thread only (set by the loader when a dialog
//! window is created, before it shows). It only looks at messages sent on that thread:
//!
//! - `WM_CREATE` of a `DUIViewWndClassName` inside a `#32770`: a common item dialog is being built;
//!   the process takes the dark app mode now, so every control the dialog creates after it is born
//!   dark (no theme change to broadcast afterwards).
//! - `WM_SHOWWINDOW` (shown) of that `#32770`: dark themes on its controls and the frame, before
//!   the window is visible.
//! - the loader's `explorust.inject.style` (sent with `SendNotifyMessageW`): the fallback when the
//!   hook came too late, and Settings' frame.
//!
//! Each phase is timed and reported to the loader. A panic never leaves this function.

use crate::dark;
use crate::target::{self, Kind, Phase};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW};
use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, EnumChildWindows, FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW, HC_ACTION,
    HWND_MESSAGE, PostMessageW, RegisterWindowMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_CREATE,
    WM_SETTINGCHANGE, WM_SHOWWINDOW,
};
use windows::core::{BOOL, PCWSTR, w};

static STYLE: AtomicU32 = AtomicU32::new(0);
/// The process took the dark app mode while a dialog was being built (its controls are born dark).
static DARK_EARLY: AtomicBool = AtomicBool::new(false);
/// The process has the dark app mode.
static DARK: AtomicBool = AtomicBool::new(false);
/// Dialogs already styled in this process.
static STYLED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

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
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_CALLWNDPROC, lParam points at the CWPSTRUCT of the message being sent.
        let m = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
        let (hwnd, msg, wp) = (m.hwnd, m.message, m.wParam.0);
        if msg == WM_CREATE || msg == WM_SHOWWINDOW || (msg != 0 && msg == style_msg()) {
            let _ = std::panic::catch_unwind(|| on_message(hwnd, msg, wp));
        }
    }
    // SAFETY: passes the hook chain on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn class_of(h: HWND) -> ([u16; 64], usize) {
    let mut buf = [0u16; 64];
    // SAFETY: the buffer outlives the call.
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    (buf, n)
}

fn is_class(h: HWND, name: &str) -> bool {
    let (buf, n) = class_of(h);
    name.encode_utf16().eq(buf[..n].iter().copied())
}

fn root(h: HWND) -> HWND {
    // SAFETY: plain query.
    unsafe { GetAncestor(h, GA_ROOT) }
}

fn has_mark(dialog: HWND) -> bool {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        if is_class(h, target::DIALOG_MARK) {
            // SAFETY: lp is the &mut bool below, alive for the whole enumeration.
            unsafe { *(lp.0 as *mut bool) = true };
            return false.into();
        }
        true.into()
    }
    let mut found = false;
    // SAFETY: see `each`.
    unsafe {
        let _ = EnumChildWindows(Some(dialog), Some(each), LPARAM(&mut found as *mut bool as isize));
    }
    found
}

fn on_message(hwnd: HWND, msg: u32, wp: usize) {
    match msg {
        WM_CREATE => {
            if !DARK.load(Ordering::Relaxed) && is_class(hwnd, target::DIALOG_MARK) && is_class(root(hwnd), target::DIALOG_CLASS) {
                if crate::switched_off() {
                    return;
                }
                let t = Instant::now();
                dark::force_dark_app();
                DARK.store(true, Ordering::Relaxed);
                DARK_EARLY.store(true, Ordering::Relaxed);
                report(Phase::DarkApp, t);
            }
        }
        WM_SHOWWINDOW => {
            if wp != 0 && is_class(hwnd, target::DIALOG_CLASS) && root(hwnd) == hwnd && has_mark(hwnd) {
                file_dialog(hwnd);
            }
        }
        _ => match Kind::from_usize(wp) {
            Some(Kind::FileDialog) => file_dialog(hwnd),
            Some(Kind::Settings) => {
                if crate::switched_off() {
                    return;
                }
                let t = Instant::now();
                let _ = dark::frame(hwnd, DWMSBT_TRANSIENTWINDOW);
                report(Phase::Total, t);
            }
            None => {}
        },
    }
}

/// Dark theme on the dialog and every control in it, Mica on its frame. When the dark app mode
/// came after the dialog's controls were built, one theme change broadcast and a repaint follow.
fn file_dialog(hwnd: HWND) {
    {
        let Ok(mut done) = STYLED.lock() else { return };
        if done.contains(&(hwnd.0 as isize)) {
            return;
        }
        if crate::switched_off() {
            return;
        }
        done.retain(|h| {
            // SAFETY: plain query on a handle that may be stale.
            unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(*h as *mut _))).as_bool() }
        });
        done.push(hwnd.0 as isize);
    }
    let t0 = Instant::now();
    let late = !DARK_EARLY.load(Ordering::Relaxed);
    if !DARK.swap(true, Ordering::Relaxed) {
        let t = Instant::now();
        dark::force_dark_app();
        report(Phase::DarkApp, t);
    }
    let t = Instant::now();
    dark::allow_dark_window(hwnd);
    let _ = dark::frame(hwnd, DWMSBT_MAINWINDOW);
    report(Phase::Frame, t);
    let t = Instant::now();
    // SAFETY: the callback only touches the child it is given.
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(child), LPARAM(0));
    }
    report(Phase::Children, t);
    if late {
        let t = Instant::now();
        let mut r = 0usize;
        let s = w!("ImmersiveColorSet");
        // SAFETY: a sent message to our own thread's window with a constant string.
        unsafe {
            let _ = SendMessageTimeoutW(hwnd, WM_SETTINGCHANGE, WPARAM(0), LPARAM(s.0 as isize), SMTO_ABORTIFHUNG, 200, Some(&mut r));
        }
        report(Phase::SettingChange, t);
        let t = Instant::now();
        // SAFETY: plain invalidation.
        unsafe {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
        }
        report(Phase::Redraw, t);
    }
    report(Phase::Total, t0);
}

unsafe extern "system" fn child(h: HWND, _: LPARAM) -> BOOL {
    let _ = std::panic::catch_unwind(|| {
        dark::allow_dark_window(h);
        let (buf, n) = class_of(h);
        let class = String::from_utf16_lossy(&buf[..n]);
        // SetWindowTheme sends the control its own WM_THEMECHANGED.
        let theme = match target::dark_theme_of(&class) {
            Some("DarkMode_CFD") => w!("DarkMode_CFD"),
            Some(_) => w!("DarkMode_Explorer"),
            None => return,
        };
        // SAFETY: a child of the dialog, on this thread.
        unsafe {
            let _ = SetWindowTheme(h, theme, PCWSTR::null());
        }
    });
    true.into()
}

/// Tells the loader how long a phase took on the host's thread.
fn report(phase: Phase, since: Instant) {
    let v = target::pack(phase, since.elapsed().as_micros() as u64);
    // SAFETY: plain window lookups and a posted message.
    unsafe {
        let done = RegisterWindowMessageW(w!("explorust.inject.done"));
        if let Ok(w) = FindWindowExW(Some(HWND_MESSAGE), None, w!("explorust.inject"), PCWSTR::null()) {
            let _ = PostMessageW(Some(w), done, WPARAM(v as usize), LPARAM(std::process::id() as isize));
        }
    }
}
