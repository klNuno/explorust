//! Snap layouts (Win+Z) for the foreground window, drawn by explorust since Windows' flyout does
//! not answer without explorer: an acrylic flyout under the window's top right corner (where the
//! maximize button is) with one picture per layout; hover a zone to light it, click to snap the
//! window there with SetWindowPos (restored first, DWM's invisible resize borders added back so
//! the visible frame fills the zone). Esc, a click elsewhere or losing the focus closes it.

use crate::snap_layout::{self as S, Flyout, Zone};
use crate::tv_scene::Painter;
use crate::windows_list as wl;
use crate::{activate, log};
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PCWSTR, w};

const CLASS: PCWSTR = w!("explorust.switcher.snap");
const TINT: u32 = 0xCC20_2020;
struct Snap {
    hwnd: HWND,
    shown: bool,
    target: HWND,
    scale: f32,
    layouts: Vec<Vec<Zone>>,
    flyout: Flyout,
    work: RECT,
    hover: Option<(usize, usize)>,
    painter: Option<Painter>,
    acting: bool,
}

thread_local! {
    static SN: RefCell<Option<Snap>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Snap) -> R) -> Option<R> {
    SN.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

pub fn create() -> windows_core::Result<HWND> {
    // SAFETY: a window class and a window owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: CLASS, hCursor: cursor, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
            w!("Snap layouts"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: TINT });
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
        let border = 0x003A_3A3Au32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        SN.with(|c| {
            *c.borrow_mut() = Some(Snap {
                hwnd,
                shown: false,
                target: HWND::default(),
                scale: 1.0,
                layouts: Vec::new(),
                flyout: S::flyout(&[]),
                work: RECT::default(),
                hover: None,
                painter: None,
                acting: false,
            })
        });
        Ok(hwnd)
    }
}

pub fn destroy() {
    if let Some(h) = with(|s| s.hwnd) {
        // SAFETY: our own window.
        unsafe {
            if IsWindow(Some(h)).as_bool() {
                let _ = DestroyWindow(h);
            }
        }
    }
}

fn snappable(h: HWND) -> bool {
    // SAFETY: plain style query.
    let style = unsafe { GetWindowLongW(h, GWL_STYLE) } as u32;
    !h.is_invalid() && style & WS_THICKFRAME.0 != 0 && style & WS_CHILD.0 == 0
}

/// Win+Z: shows the flyout for the foreground window, or closes it.
pub fn toggle() {
    if with(|s| s.shown).unwrap_or(false) {
        hide(true);
        return;
    }
    let t0 = Instant::now();
    // SAFETY: plain query.
    let target = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) };
    if !snappable(target) {
        log::line(&format!("snap: {:?} cannot be resized, no layouts", wl::title(target)));
        return;
    }
    let mut pos = (0, 0);
    let shown = with(|s| {
        s.target = target;
        // SAFETY: monitor and frame queries, then placing our own window.
        unsafe {
            let mon = MonitorFromWindow(target, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            s.work = mi.rcWork;
            let _ = SetWindowPos(s.hwnd, None, mi.rcMonitor.left, mi.rcMonitor.top, 1, 1, SWP_NOACTIVATE | SWP_NOZORDER);
            s.scale = GetDpiForWindow(s.hwnd).max(96) as f32 / 96.0;
            let sc = s.scale;
            s.layouts = S::layouts((s.work.right - s.work.left) as f32 / sc);
            s.flyout = S::flyout(&s.layouts);
            let mut frame = RECT::default();
            let _ = DwmGetWindowAttribute(target, DWMWA_EXTENDED_FRAME_BOUNDS, &mut frame as *mut _ as _, size_of::<RECT>() as u32);
            let (fw, fh) = ((s.flyout.w * sc).round() as i32, (s.flyout.h * sc).round() as i32);
            // Under the maximize button (the second caption button from the right), kept on screen.
            let anchor_x = frame.right - (46.0 * 1.5 * sc) as i32;
            let x = (anchor_x - fw / 2).clamp(s.work.left + 8, s.work.right - fw - 8);
            let y = (frame.top + (32.0 * sc) as i32 + 4).clamp(s.work.top + 8, s.work.bottom - fh - 8);
            let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), x, y, fw, fh, SWP_NOACTIVATE);
            pos = (x, y);
            if let Some(painter) = &s.painter {
                if let Err(e) = painter.resize(fw as u32, fh as u32) {
                    log::line(&format!("snap resize: {e}"));
                }
            } else {
                match Painter::new(s.hwnd, fw as u32, fh as u32) {
                    Ok(p) => s.painter = Some(p),
                    Err(e) => log::line(&format!("snap painter: {e}")),
                }
            }
        }
        s.shown = true;
        s.hover = None;
        draw(s);
        // SAFETY: shows our own window.
        unsafe {
            let _ = ShowWindow(s.hwnd, SW_SHOW);
        }
        s.acting = true;
        log::line(&format!(
            "snap: {} layouts for {:?}, flyout {}x{} DIPs at {},{}, shown in {:.1} ms",
            s.layouts.len(),
            wl::title(target),
            s.flyout.w,
            s.flyout.h,
            pos.0,
            pos.1,
            t0.elapsed().as_secs_f32() * 1000.0
        ));
        s.hwnd
    });
    if let Some(h) = shown {
        let how = activate::activate(h);
        with(|s| s.acting = false);
        log::line(&format!("snap: focus via {how}"));
    }
}

fn draw(s: &mut Snap) {
    if let Some(p) = s.painter.as_mut()
        && let Err(e) = p.snap(s.scale, &s.flyout.zones, s.hover)
    {
        log::line(&format!("snap draw: {e}"));
    }
}

/// Hides the flyout; `refocus` gives the focus back to the target window.
fn hide(refocus: bool) {
    let t = with(|s| {
        if !s.shown {
            return None;
        }
        s.shown = false;
        s.acting = true;
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(s.hwnd, SW_HIDE);
        }
        if let Some(painter) = &s.painter && let Err(e) = painter.resize(1, 1) {
            log::line(&format!("hidden snap resize: {e}"));
        }
        Some(s.target)
    })
    .flatten();
    if let (Some(t), true) = (t, refocus) {
        activate::activate(t);
    }
    with(|s| s.acting = false);
}

/// Snaps `h` so its visible frame fills `zone` of the work area.
fn snap_to(h: HWND, zone: Zone, work: RECT) -> String {
    let (l, t, r, b) = S::zone_px(zone, (work.left, work.top, work.right, work.bottom));
    // SAFETY: window placement calls on another process's top-level window.
    unsafe {
        if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let mut win = RECT::default();
        let _ = GetWindowRect(h, &mut win);
        let mut frame = win;
        let _ = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut frame as *mut _ as _, size_of::<RECT>() as u32);
        // The invisible resize borders around the visible frame.
        let (ml, mt, mr, mb) = (frame.left - win.left, frame.top - win.top, win.right - frame.right, win.bottom - frame.bottom);
        let _ = SetWindowPos(h, None, l - ml, t - mt, (r - l) + ml + mr, (b - t) + mt + mb, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut frame as *mut _ as _, size_of::<RECT>() as u32);
        format!("frame {},{} {}x{} for zone {},{} {}x{}", frame.left, frame.top, frame.right - frame.left, frame.bottom - frame.top, l, t, r - l, b - t)
    }
}

fn dips(lp: LPARAM, s: f32) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            with(|s| {
                let (x, y) = dips(lp, s.scale);
                let h = s.flyout.hit(x, y);
                if h != s.hover {
                    s.hover = h;
                    draw(s);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let pick = with(|s| {
                let (x, y) = dips(lp, s.scale);
                s.flyout.hit(x, y).map(|(l, z)| (s.target, s.layouts[l][z], s.work, l, z))
            })
            .flatten();
            if let Some((target, zone, work, l, z)) = pick {
                hide(false);
                let t = Instant::now();
                let how = snap_to(target, zone, work);
                let fg = activate::activate(target);
                log::line(&format!(
                    "snap: {:?} to layout {} zone {} ({:?}): {how} in {:.1} ms, focus via {fg}",
                    wl::title(target),
                    l + 1,
                    z + 1,
                    zone,
                    t.elapsed().as_secs_f32() * 1000.0
                ));
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            hide(true);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if with(|s| s.shown && !s.acting).unwrap_or(false) {
                hide(false);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            // SAFETY: nothing to paint by GDI; the frame comes from the swap chain.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
