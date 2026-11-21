//! Win+D and Win+M on real windows: lists what a press hides (the windows Alt+Tab would show),
//! minimizes them, and on the next press restores them bottom first so the Z-order comes back.

use crate::log;
use crate::toggle::{Action, Toggle};
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOP,
    IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow, ShowWindowAsync,
    WS_EX_TOOLWINDOW,
};
use windows_core::BOOL;

thread_local! {
    static TOGGLE: RefCell<Toggle> = RefCell::new(Toggle::default());
}

const SKIP: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

fn class(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer outlives the call.
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn hideable(h: HWND) -> bool {
    // SAFETY: read-only queries on a window handle from EnumWindows.
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return false;
        }
        if GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4);
        if cloaked != 0 {
            return false;
        }
        let mut rc = RECT::default();
        if GetWindowRect(h, &mut rc).is_err() || rc.right <= rc.left || rc.bottom <= rc.top {
            return false;
        }
    }
    let c = class(h);
    !SKIP.contains(&c.as_str()) && !c.starts_with("explorust.")
}

unsafe extern "system" fn each(h: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the Vec passed by `shown` for the length of EnumWindows.
    let out = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    if hideable(h) {
        out.push(h.0 as isize);
    }
    BOOL(1)
}

/// The windows a press hides, top of the Z-order first (EnumWindows' order).
fn shown() -> Vec<isize> {
    let mut out: Vec<isize> = Vec::new();
    // SAFETY: the callback only runs during the call, while `out` lives.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut out as *mut _ as isize)) };
    out
}

fn hwnd(id: isize) -> HWND {
    HWND(id as _)
}

pub fn press(progman: Option<HWND>) {
    let list = shown();
    let action = TOGGLE.with(|t| {
        t.borrow_mut().press(&list, |id| unsafe {
            // SAFETY: plain queries on a remembered handle (it may be gone).
            IsWindow(Some(hwnd(id))).as_bool() && IsIconic(hwnd(id)).as_bool()
        })
    });
    match action {
        Action::Minimize(ws) => {
            let mut keys = 0;
            for &id in &ws {
                // An elevated window ignores this process (UIPI): the keys host minimizes it.
                if bus::above_me(hwnd(id)) && bus::post(bus::Command::Minimize, id) {
                    keys += 1;
                    continue;
                }
                // SAFETY: async show requests do not wait on a hung window.
                let _ = unsafe { ShowWindowAsync(hwnd(id), SW_SHOWMINNOACTIVE) };
            }
            let fg = progman.map(|p| unsafe { SetForegroundWindow(p) }.as_bool());
            log::line(&format!("show desktop: minimized {} windows ({keys} through explorust-keys), desktop focused {fg:?}", ws.len()));
        }
        Action::Restore(ws) => {
            // With an elevated window, the helper raises everything bottom first and then gives
            // the foreground to the top window in the same batch, even when that window is ordinary.
            let via_keys = ws.iter().any(|&id| bus::above_me(hwnd(id)));
            if via_keys && bus::keys_pipe::restore_batch(&ws) {
                log::line(&format!("show desktop: queued {} restores and top activation through explorust-keys", ws.len()));
                return;
            }
            if via_keys {
                log::line("show desktop: keys batch unavailable, restoring locally");
            }
            for &id in &ws {
                let h = hwnd(id);
                // SAFETY: restore without activation, then raise it above the ones restored before.
                unsafe {
                    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                    if IsHungAppWindow(h).as_bool() {
                        let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                        let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, flags | SWP_ASYNCWINDOWPOS);
                    } else {
                        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                        let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, flags);
                    }
                }
            }
            let fg = ws.last().map(|&t| unsafe { SetForegroundWindow(hwnd(t)) }.as_bool());
            log::line(&format!("show desktop: restored {} windows locally, top focused {fg:?}", ws.len()));
        }
        Action::Nothing => log::line("show desktop: nothing to do"),
    }
}
