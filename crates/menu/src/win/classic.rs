//! The classic menu (`Show more options`, Shift+right-click, Shift+F10): the same HMENU tracked by
//! `TrackPopupMenuEx`, owned by a hidden window that forwards the owner-draw and popup messages to
//! the handler, and the command invocation shared with the Windows 11 menu.

use super::log;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::{CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, IContextMenu, IContextMenu2, IContextMenu3};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, Interface, PCSTR, PCWSTR, w};

const CMIC_MASK_UNICODE: u32 = 0x4000;
const CMIC_MASK_PTINVOKE: u32 = 0x2000_0000;
const CMIC_MASK_SHIFT_DOWN: u32 = 0x1000_0000;
const CMIC_MASK_CONTROL_DOWN: u32 = 0x4000_0000;

thread_local! {
    static OWNER: RefCell<Option<HWND>> = const { RefCell::new(None) };
    /// The handler that gets the forwarded menu messages while its menu shows.
    pub static CURRENT: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

/// uxtheme 135 SetPreferredAppMode(ForceDark), 136 FlushMenuThemes, 133 AllowDarkModeForWindow:
/// the dark classic menu Windows' own apps get.
fn dark(hwnd: HWND) {
    // SAFETY: exports looked up by ordinal, skipped when this build lacks them.
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let set: unsafe extern "system" fn(i32) -> i32 = core::mem::transmute(f);
            set(2);
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(133 as *const u8)) {
            let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = core::mem::transmute(f);
            let _ = allow(hwnd, true.into());
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = core::mem::transmute(f);
            flush();
        }
    }
}

/// The hidden owner window of this thread's classic menus, made once.
pub fn owner() -> HWND {
    if let Some(h) = OWNER.with(|o| *o.borrow()) {
        return h;
    }
    // SAFETY: a window class and a hidden window owned by this thread.
    let h = unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(owner_proc), hInstance: inst.into(), lpszClassName: w!("explorust.menu.owner"), ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("explorust.menu.owner"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(inst.into()),
            None,
        )
        .unwrap_or_default()
    };
    dark(h);
    OWNER.with(|o| *o.borrow_mut() = Some(h));
    h
}

unsafe extern "system" fn owner_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR | WM_MENUSELECT) {
        let cm = CURRENT.with(|c| c.borrow().clone());
        if let Some(cm) = cm {
            // SAFETY: forwards the message as Explorer does; the handler writes the result.
            unsafe {
                if let Ok(c3) = cm.cast::<IContextMenu3>() {
                    let mut r = LRESULT(0);
                    if c3.HandleMenuMsg2(msg, wp, lp, Some(&mut r)).is_ok() {
                        return r;
                    }
                } else if let Ok(c2) = cm.cast::<IContextMenu2>() {
                    if c2.HandleMenuMsg(msg, wp, lp).is_ok() {
                        return LRESULT(if msg == WM_MENUCHAR { 0 } else { 1 });
                    }
                }
            }
        }
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

/// Tracks the classic menu at a screen point; returns the chosen command id, 0 when dismissed.
pub fn track(cm: &IContextMenu, menu: HMENU, x: i32, y: i32) -> u32 {
    let h = owner();
    CURRENT.with(|c| *c.borrow_mut() = Some(cm.clone()));
    // SAFETY: a menu this thread owns, tracked modally by the menu manager.
    let id = unsafe {
        let _ = SetForegroundWindow(h);
        let id = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN).0, x, y, h, None);
        let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        id.0 as u32
    };
    CURRENT.with(|c| c.borrow_mut().take());
    id
}

/// Runs command `id` (ids start at `first`) with the point and the modifier keys, as Explorer does.
pub fn invoke(cm: &IContextMenu, owner: HWND, id: u32, first: u32, at: POINT) {
    let offset = (id - first) as usize;
    // SAFETY: modifier state of this thread's input.
    let (shift, ctrl) = unsafe { (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_CONTROL.0 as i32) < 0) };
    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    if shift {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if ctrl {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd: owner,
        lpVerb: PCSTR(offset as *const u8),
        lpVerbW: PCWSTR(offset as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: at,
        ..Default::default()
    };
    let verb = super::read::verb(cm, id - first);
    let t = std::time::Instant::now();
    CURRENT.with(|c| *c.borrow_mut() = Some(cm.clone()));
    // SAFETY: the info struct outlives the call; the verb is MAKEINTRESOURCE(offset).
    let r = unsafe { cm.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO) };
    CURRENT.with(|c| c.borrow_mut().take());
    log::line(&format!(
        "invoke id {id} verb '{}': {:?} in {:.1} ms",
        verb,
        r.err().map(|e| e.code()),
        t.elapsed().as_secs_f64() * 1000.0
    ));
}
