//! The `Progman` window ("Program Manager", as explorer names it): it covers the virtual screen,
//! stays at the bottom of the Z-order, paints the wallpaper and hosts the desktop view. Its
//! thread also owns the command window and runs the message loop.

use crate::layout::Rect;
use crate::view::{self, Desk};
use crate::wallpaper::{self, Frame, Surface, WM_WALLPAPER};
use crate::{command, log, screen, workerw};
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetSysColorBrush, COLOR_BACKGROUND, HDC, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_ERASE,
    RDW_INVALIDATE, RedrawWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    HWND_BOTTOM, IDC_ARROW, IsWindow, LoadCursorW, MSG, MoveWindow, PostQuitMessage, RegisterClassExW, SPI_SETDESKWALLPAPER,
    SPI_SETWORKAREA, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetWindowPos,
    ShowWindow, TranslateMessage, UnregisterClassW, WA_INACTIVE, WINDOWPOS, WM_ACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYFIRST, WM_KEYLAST, WM_MOUSEACTIVATE, WM_PAINT, WM_PRINTCLIENT, WM_SETFOCUS, WM_SETTINGCHANGE,
    WM_SIZE, WM_SYSCOLORCHANGE, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::{PCWSTR, w};

const CLASS: PCWSTR = w!("Progman");

struct State {
    hwnd: HWND,
    origin: (i32, i32),
    surface: Option<Surface>,
    generation: usize,
    desk: Option<Desk>,
    /// The 24H2 layout: Progman has no GDI content, the DefView is a layered child.
    layered: bool,
    /// The WorkerW child, once a wallpaper app raised the desktop (0x052C).
    workerw: Option<HWND>,
    wallpaper_input: Option<workerw::input::Hook>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

/// The Progman window and its view, for the command window.
pub fn hwnd() -> Option<HWND> {
    with(|s| s.hwnd)
}

pub fn defview() -> Option<HWND> {
    with(|s| s.desk.as_ref().map(|d| d.hwnd)).flatten()
}

pub fn run() {
    // SAFETY: Win32 and COM calls on this part's own thread, balanced before it returns.
    unsafe {
        if let Ok(other) = FindWindowW(CLASS, None) {
            log::line(&format!("another Progman ({:?}) owns the desktop, not started", other.0));
            return;
        }
        let ole = OleInitialize(None);
        if let Err(e) = &ole {
            log::line(&format!("OleInitialize: {e}"));
        }
        match create() {
            Ok(h) => {
                log::line(&format!("Progman {:?}", h.0));
                pump();
            }
            Err(e) => log::line(&format!("Progman not created: {e}")),
        }
        command::destroy();
        if let Some(h) = hwnd().filter(|h| IsWindow(Some(*h)).as_bool()) {
            let _ = DestroyWindow(h);
        }
        STATE.with(|s| s.borrow_mut().take());
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let _ = UnregisterClassW(CLASS, Some(inst.into()));
        if ole.is_ok() {
            OleUninitialize();
        }
        log::line("desktop stopped");
    }
}

unsafe fn create() -> windows_core::Result<HWND> {
    // SAFETY: registers and creates windows owned by this thread.
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
        let v = screen::virtual_screen();
        let layered = workerw::supported();
        let ex = if layered { WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP } else { WS_EX_TOOLWINDOW };
        let hwnd = CreateWindowExW(
            ex,
            CLASS,
            w!("Program Manager"),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            v.x,
            v.y,
            v.w,
            v.h,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                origin: (v.x, v.y),
                surface: None,
                generation: 1,
                desk: None,
                layered,
                workerw: None,
                wallpaper_input: None,
            })
        });
        wallpaper::spawn(hwnd, 1);
        match view::create(hwnd, Rect::new(0, 0, v.w, v.h)) {
            Ok(desk) => {
                let dv = desk.hwnd;
                with(|s| s.desk = Some(desk));
                if layered && !workerw::layer_view(dv, false) {
                    log::line("DefView not layered: the desktop shows no content (needs the Windows 10 manifest)");
                }
                view::apply_work_areas(dv, (v.x, v.y));
            }
            Err(e) => log::line(&format!("desktop view not created: {e}")),
        }
        command::create()?;
        let _ = ShowWindow(hwnd, SW_SHOWNA);
        let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        Ok(hwnd)
    }
}

unsafe fn pump() {
    let mut msg = MSG::default();
    // SAFETY: a plain message loop; keys go to the view first (F2, Delete, Ctrl+C...).
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message) {
                let v = with(|s| s.desk.as_ref().map(|d| d.view.clone())).flatten();
                if let Some(v) = v {
                    if v.TranslateAccelerator(&msg).0 == 0 {
                        continue;
                    }
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The list view background (Progman's erase and print): the wallpaper, or the key color once
/// the WorkerW paints the wallpaper under the view.
fn paint_into(hdc: HDC, r: Rect) {
    if with(|s| s.workerw.is_some()).unwrap_or(false) {
        let rc = windows::Win32::Foundation::RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() };
        // SAFETY: a brush created, used and freed here.
        unsafe {
            let b = CreateSolidBrush(workerw::KEY);
            FillRect(hdc, &rc, b);
            let _ = DeleteObject(b.into());
        }
        return;
    }
    paint_wallpaper(hdc, r);
}

/// Draws the wallpaper (client coordinates of Progman) into `hdc`.
pub fn paint_wallpaper(hdc: HDC, r: Rect) {
    let drawn = with(|s| s.surface.as_ref().map(|sf| sf.blit(hdc, r)).is_some()).unwrap_or(false);
    if !drawn {
        let rc = windows::Win32::Foundation::RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() };
        // SAFETY: fills the given DC with the system background brush (not owned, not freed).
        unsafe { FillRect(hdc, &rc, GetSysColorBrush(COLOR_BACKGROUND)) };
    }
}

/// The whole screen changed (resolution, monitors): cover the virtual screen again.
fn relayout(hwnd: HWND, why: &str) {
    let v = screen::virtual_screen();
    log::line(&format!("{why}: virtual screen {v:?}"));
    let generation = with(|s| {
        s.origin = (v.x, v.y);
        s.generation += 1;
        s.generation
    });
    // SAFETY: moves this thread's window.
    let _ = unsafe { MoveWindow(hwnd, v.x, v.y, v.w, v.h, true) };
    if let Some(g) = generation {
        wallpaper::spawn(hwnd, g);
    }
    if let Some(dv) = defview() {
        view::apply_work_areas(dv, (v.x, v.y));
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let r = std::panic::catch_unwind(|| handle(hwnd, msg, wp, lp));
    match r {
        Ok(Some(l)) => l,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        Err(_) => {
            log::line(&format!("Progman: panic on message {msg:#x}"));
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

fn forward(msg: u32, wp: WPARAM, lp: LPARAM) {
    if let Some(dv) = defview() {
        // SAFETY: a settings message passed on to the view window of this thread.
        unsafe { SendMessageW(dv, msg, Some(wp), Some(lp)) };
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
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 != WA_INACTIVE {
                // SAFETY: z-order change of this thread's window.
                let _ = unsafe {
                    SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)
                };
            }
            None
        }
        WM_SETFOCUS => {
            if let Some(dv) = defview() {
                // SAFETY: focus moves to the view window of this thread.
                let _ = unsafe { SetFocus(Some(dv)) };
            }
            Some(LRESULT(0))
        }
        WM_ERASEBKGND => {
            paint_into(HDC(wp.0 as _), client(hwnd));
            Some(LRESULT(1))
        }
        WM_PRINTCLIENT => {
            paint_into(HDC(wp.0 as _), client(hwnd));
            Some(LRESULT(0))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: a paint cycle of this window.
            unsafe {
                let dc = BeginPaint(hwnd, &mut ps);
                paint_into(dc, screen::rect(&ps.rcPaint));
                let _ = EndPaint(hwnd, &ps);
            }
            Some(LRESULT(0))
        }
        WM_SIZE => {
            let (w, h) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
            for child in [defview(), with(|s| s.workerw).flatten()].into_iter().flatten() {
                // SAFETY: resizes a child window of this thread.
                let _ = unsafe { MoveWindow(child, 0, 0, w, h, true) };
            }
            None
        }
        workerw::MSG_RAISE => {
            raise(hwnd, wp, lp);
            Some(LRESULT(0))
        }
        WM_MOUSEACTIVATE => with(|s| s.wallpaper_input.as_ref()?.right_click_activation(lp)).flatten(),
        workerw::input::MSG_RIGHT => {
            if let (Some(view), Some(worker)) = (defview(), with(|s| s.wallpaper_input.as_ref().and(s.workerw)).flatten()) {
                workerw::input::right_click(hwnd, view, worker, lp);
            }
            Some(LRESULT(0))
        }
        WM_WALLPAPER => {
            // SAFETY: lParam is the Box<Frame> posted by `wallpaper::spawn`; taken once here.
            let frame = unsafe { Box::from_raw(lp.0 as *mut Frame) };
            let current = with(|s| s.generation == wp.0).unwrap_or(false);
            if current {
                let sf = Surface::new(&frame);
                log::line(&format!("wallpaper shown: {}", sf.is_some()));
                with(|s| s.surface = sf);
                // SAFETY: repaints this window and the view over it.
                let _ = unsafe { RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN) };
                // The layered children have surfaces of their own: invalidate them by name.
                if let Some(w) = with(|s| s.workerw).flatten() {
                    // SAFETY: invalidates a child window of this thread.
                    let _ = unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(w), None, true) };
                }
                workerw::repaint_list();
            }
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            let action = wp.0 as u32;
            if action == SPI_SETDESKWALLPAPER.0 {
                log::line("SPI_SETDESKWALLPAPER: reloading the wallpaper");
                if let Some(g) = with(|s| {
                    s.generation += 1;
                    s.generation
                }) {
                    wallpaper::spawn(hwnd, g);
                }
            }
            if action == SPI_SETWORKAREA.0 {
                if let (Some(dv), Some(o)) = (defview(), with(|s| s.origin)) {
                    view::apply_work_areas(dv, o);
                }
            }
            forward(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_SYSCOLORCHANGE => {
            log::line("WM_SYSCOLORCHANGE: reloading the wallpaper");
            if let Some(g) = with(|s| {
                s.generation += 1;
                s.generation
            }) {
                wallpaper::spawn(hwnd, g);
            }
            forward(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_DISPLAYCHANGE => {
            relayout(hwnd, "WM_DISPLAYCHANGE");
            forward(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            // Alt+F4 on the desktop: explorer shows the shut down dialog; not ours yet.
            log::line("Progman: WM_CLOSE ignored");
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            drop(with(|s| s.wallpaper_input.take()));
            let desk = with(|s| s.desk.take()).flatten();
            if let Some(d) = desk {
                d.destroy();
            }
            with(|s| s.surface = None);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            relayout(hwnd, &format!("WM_DPICHANGED {}", wp.0 & 0xFFFF));
            forward(msg, wp, lp);
            Some(LRESULT(0))
        }
        m if (0x400..0x8000).contains(&m) => {
            // What Windows and apps send to Progman (explorer's private messages): logged once each.
            if SEEN.with(|s| s.borrow_mut().insert(m)) {
                log::line(&format!("Progman got message {m:#x} ({:#x}, {:#x})", wp.0, lp.0));
            }
            None
        }
        _ => None,
    }
}

thread_local! {
    static SEEN: RefCell<std::collections::HashSet<u32>> = RefCell::new(Default::default());
}

/// 0x052C from a wallpaper app: the WorkerW goes under the view and the view turns transparent.
fn raise(hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    let (layered, existing) = with(|s| (s.layered, s.workerw)).unwrap_or((false, None));
    if let Some(w) = existing {
        log::line(&format!("0x052C ({:#x}, {:#x}): already raised, WorkerW {:?}", wp.0, lp.0, w.0));
        return;
    }
    if !layered {
        log::line(&format!("0x052C ({:#x}, {:#x}) ignored: no layered layout", wp.0, lp.0));
        return;
    }
    match workerw::create(hwnd) {
        Ok(w) => {
            with(|s| s.workerw = Some(w));
            if let Some(dv) = defview() {
                if workerw::layer_view(dv, true) {
                    match workerw::input::Hook::new(hwnd, dv, w) {
                        Ok(hook) => with(|s| s.wallpaper_input = Some(hook)),
                        Err(error) => {
                            log::line(&format!("wallpaper input observation unavailable: {error}"));
                            None
                        }
                    };
                }
            }
            log::line(&format!("0x052C ({:#x}, {:#x}): raised, WorkerW {:?}", wp.0, lp.0, w.0));
            // SAFETY: repaints this window and its children (view background now the key color).
            let _ = unsafe { RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN) };
            workerw::repaint_list();
        }
        Err(e) => log::line(&format!("0x052C: WorkerW not created: {e}")),
    }
}

fn client(hwnd: HWND) -> Rect {
    let mut rc = windows::Win32::Foundation::RECT::default();
    // SAFETY: reads this window's client rectangle.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc) };
    screen::rect(&rc)
}
