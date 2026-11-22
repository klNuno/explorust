//! Keep wallpaper input intact while giving the desktop its blank-area right click.

use super::list_view;
use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{LVIS_SELECTED, LVITEMW, LVM_SETITEMSTATE};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const MSG_RIGHT: u32 = WM_APP + 0x25;

#[derive(Clone, Copy)]
struct Target {
    progman: HWND,
    view: HWND,
    worker: HWND,
}

thread_local! {
    static TARGET: Cell<Option<Target>> = const { Cell::new(None) };
    static DOWN_ON_WALLPAPER: Cell<bool> = const { Cell::new(false) };
}

pub struct Hook(HHOOK);

impl Hook {
    pub fn new(progman: HWND, view: HWND, worker: HWND) -> windows_core::Result<Self> {
        let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), Some(GetModuleHandleW(None)?.into()), 0)? };
        TARGET.set(Some(Target { progman, view, worker }));
        Ok(Self(hook))
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        TARGET.set(None);
        DOWN_ON_WALLPAPER.set(false);
        let _ = unsafe { UnhookWindowsHookEx(self.0) };
    }
}

unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let msg = wp.0 as u32;
    if code == HC_ACTION as i32 && matches!(msg, WM_RBUTTONDOWN | WM_RBUTTONUP)
        && let Some(target) = TARGET.get()
    {
        // SAFETY: the low-level hook receives MSLLHOOKSTRUCT in screen coordinates. No COM,
        // send to a foreign thread, or menu operations are performed in the hook.
        let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
        if msg == WM_RBUTTONDOWN {
            DOWN_ON_WALLPAPER.set(on_wallpaper(target, pt));
        }
        if msg == WM_RBUTTONUP && DOWN_ON_WALLPAPER.replace(false) && on_wallpaper(target, pt) {
            let _ = unsafe { PostMessageW(Some(target.progman), MSG_RIGHT, WPARAM(0), pack(pt)) };
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

fn on_wallpaper(target: Target, pt: POINT) -> bool {
    unsafe {
        let hit = WindowFromPoint(pt);
        // WorkerW itself already forwards mouse messages. Wallpaper apps either parent their
        // window to WorkerW or place a sibling behind DefView directly under Progman.
        hit != target.worker && !hit.is_invalid()
            && (IsChild(target.worker, hit).as_bool()
                || (GetAncestor(hit, GA_ROOT) == target.progman && hit != target.progman
                    && hit != target.view && !IsChild(target.view, hit).as_bool()))
    }
}

pub fn right_click(lp: LPARAM) {
    if !TARGET.get().is_some_and(|t| on_wallpaper(t, unpack(lp))) {
        return;
    }
    let Some(list) = list_view() else { return };
    let state = LVITEMW { stateMask: LVIS_SELECTED, ..Default::default() };
    // A synthetic button-down can start the list view's mouse tracking after the physical
    // button-up has already gone to the wallpaper. Open its normal context menu directly.
    unsafe {
        SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(usize::MAX)), Some(LPARAM(&state as *const _ as isize)));
        let _ = PostMessageW(Some(list), WM_CONTEXTMENU, WPARAM(list.0 as usize), lp);
    }
}

pub fn unpack(lp: LPARAM) -> POINT {
    POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }
}

pub fn pack(pt: POINT) -> LPARAM {
    LPARAM((pt.y as u16 as isize) << 16 | pt.x as u16 as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_negative_desktop_coordinates() {
        for pt in [POINT { x: -1920, y: 675 }, POINT { x: 1024, y: -720 }] {
            assert_eq!(unpack(pack(pt)), pt);
        }
    }
}
