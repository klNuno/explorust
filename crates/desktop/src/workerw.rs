//! The raised desktop of Windows 11 24H2 and later, for Wallpaper Engine and Lively.
//!
//! Microsoft's description (quoted in Lively's `WinDesktopCore.cs`): "the top-level Progman window
//! is now created with WS_EX_NOREDIRECTIONBITMAP (so there is no GDI content for that window at
//! all) and the shell DefView child window is a WS_EX_LAYERED child window. When the desktop is
//! raised, we create a child WorkerW window that is z-ordered under the DefView that will render
//! the wallpaper. The DefView window will draw mostly transparent with just the icons and text."
//! An app raises it by sending 0x052C to Progman, then finds the WorkerW with
//! `FindWindowEx(progman, 0, "WorkerW")` and puts its window between the DefView and the WorkerW
//! (Lively's default) or inside the WorkerW.
//!
//! Tree, before and after 0x052C:
//! `Progman "Program Manager"` (WS_EX_NOREDIRECTIONBITMAP) > `SHELLDLL_DefView` (WS_EX_LAYERED) >
//! `SysListView32 "FolderView"`; then `Progman` > [`SHELLDLL_DefView`, `WorkerW`], the WorkerW
//! last (bottom) child.
//!
//! Here the DefView is opaque (alpha 255, the list view paints the wallpaper as its background)
//! until the desktop is raised; then it is color-keyed (the list view background is `KEY`) and the
//! WorkerW paints the wallpaper. Key-colored pixels let the mouse through, so the WorkerW passes
//! the clicks it gets back to the list view.

use crate::{log, progman, screen};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, GetClipBox, InvalidateRect, MapWindowPoints, PAINTSTRUCT, HDC};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, FindWindowExW, GWL_EXSTYLE, GetClientRect, GetWindowLongPtrW, HWND_BOTTOM,
    IDC_ARROW, LWA_ALPHA, LWA_COLORKEY, LoadCursorW, RegisterClassExW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SendMessageW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, WINDOWPOS,
    WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_PRINTCLIENT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_WINDOWPOSCHANGING, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW, CS_DBLCLKS,
    WS_CHILD, WS_EX_LAYERED, WS_VISIBLE,
};
use windows_core::{PCWSTR, w};

/// Sent to Progman by wallpaper apps (wParam 0xD, lParam 1 for Lively and weebp; 0, 0 for older
/// code): raise the desktop, create the WorkerW if it does not exist.
pub const MSG_RAISE: u32 = 0x052C;

/// The list view background once raised: those pixels of the DefView are transparent.
pub const KEY: COLORREF = COLORREF(0x0001_0101);

const CLASS: PCWSTR = w!("WorkerW");

static PAINTS: AtomicU32 = AtomicU32::new(0);

/// Layered child windows need the Windows 8+ manifest; without it the process sees 6.2 and the
/// classic layout (an opaque Progman with GDI content) is kept.
pub fn supported() -> bool {
    let mut v = OSVERSIONINFOW { dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: `v` has its size set.
    #[allow(deprecated)]
    let ok = unsafe { GetVersionExW(&mut v) }.is_ok();
    ok && (v.dwMajorVersion, v.dwMinorVersion) >= (6, 3)
}

/// Makes the DefView a layered child: opaque, or color-keyed once raised.
pub fn layer_view(dv: HWND, raised: bool) -> bool {
    // SAFETY: style and layering of the view window this thread created.
    unsafe {
        let ex = GetWindowLongPtrW(dv, GWL_EXSTYLE);
        if ex & WS_EX_LAYERED.0 as isize == 0 {
            SetWindowLongPtrW(dv, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        }
        let ok = if raised {
            SetLayeredWindowAttributes(dv, KEY, 0, LWA_COLORKEY)
        } else {
            SetLayeredWindowAttributes(dv, COLORREF(0), 255, LWA_ALPHA)
        }
        .is_ok();
        let layered = GetWindowLongPtrW(dv, GWL_EXSTYLE) & WS_EX_LAYERED.0 as isize != 0;
        log::line(&format!("DefView layered {layered} (attributes {ok}), raised {raised}"));
        layered && ok
    }
}

/// Creates the WorkerW as the bottom child of Progman, covering its client area.
pub fn create(progman: HWND) -> windows_core::Result<HWND> {
    // SAFETY: registers and creates a child window owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut rc = Default::default();
        GetClientRect(progman, &mut rc)?;
        let h = CreateWindowExW(
            WS_EX_LAYERED,
            CLASS,
            None,
            // No WS_CLIPSIBLINGS: the DefView above covers it all and would clip every GDI paint.
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            rc.right,
            rc.bottom,
            Some(progman),
            None,
            Some(inst.into()),
            None,
        )?;
        SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA)?;
        let _ = SetWindowPos(h, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        Ok(h)
    }
}

/// Repaints the list view from scratch (its icon backgrounds included).
pub fn repaint_list() {
    if let Some(lv) = list_view() {
        // SAFETY: invalidates a window of this thread.
        let _ = unsafe { InvalidateRect(Some(lv), None, true) };
    }
}

fn list_view() -> Option<HWND> {
    let dv = progman::defview()?;
    // SAFETY: a child lookup in this thread's view window.
    unsafe { FindWindowExW(Some(dv), None, w!("SysListView32"), None) }.ok()
}

fn paint(hdc: HDC, hwnd: HWND) {
    let mut rc = Default::default();
    // SAFETY: reads this window's client rectangle.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    progman::paint_wallpaper(hdc, screen::rect(&rc));
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match std::panic::catch_unwind(|| handle(hwnd, msg, wp, lp)) {
        Ok(Some(l)) => l,
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_WINDOWPOSCHANGING => {
            // SAFETY: for this message lParam points to the WINDOWPOS being applied.
            let pos = unsafe { &mut *(lp.0 as *mut WINDOWPOS) };
            if pos.flags & SWP_NOZORDER == Default::default() {
                pos.hwndInsertAfter = HWND_BOTTOM;
            }
            None
        }
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            paint(HDC(wp.0 as _), hwnd);
            Some(LRESULT(1))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: a paint cycle of this window.
            unsafe {
                let dc = BeginPaint(hwnd, &mut ps);
                let mut clip = Default::default();
                GetClipBox(dc, &mut clip);
                if PAINTS.fetch_add(1, Ordering::Relaxed) < 3 {
                    log::line(&format!("WorkerW paint {:?} clip {:?}", screen::rect(&ps.rcPaint), screen::rect(&clip)));
                }
                progman::paint_wallpaper(dc, screen::rect(&ps.rcPaint));
                let _ = EndPaint(hwnd, &ps);
            }
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let lv = list_view()?;
            // SAFETY: same-thread message to the list view; wheel points are in screen pixels.
            Some(unsafe { SendMessageW(lv, msg, Some(wp), Some(lp)) })
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP
        | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK | WM_XBUTTONDOWN | WM_XBUTTONUP
        | WM_XBUTTONDBLCLK => {
            // A click on a transparent pixel of the view: give it back to the list view.
            let lv = list_view()?;
            let mut pt = [POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }];
            // SAFETY: maps one point between two windows of this thread, then sends the message.
            unsafe {
                MapWindowPoints(Some(hwnd), Some(lv), &mut pt);
                let l = ((pt[0].y as u16 as isize) << 16) | pt[0].x as u16 as isize;
                Some(SendMessageW(lv, msg, Some(wp), Some(LPARAM(l))))
            }
        }
        _ => None,
    }
}
