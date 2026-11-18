//! The `Progman` window ("Program Manager", as explorer names it): it covers the virtual screen,
//! stays at the bottom of the Z-order, paints the wallpaper and hosts the desktop view. Its
//! thread also owns the command window and runs the message loop.

use crate::layout::Rect;
use crate::view::{self, Desk};
use crate::wallpaper::{self, Frame, Surface, WM_WALLPAPER};
use crate::{command, log, screen};
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetSysColorBrush, COLOR_BACKGROUND, HDC, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_ERASE,
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
    WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_KEYFIRST, WM_KEYLAST, WM_PAINT, WM_PRINTCLIENT, WM_SETFOCUS, WM_SETTINGCHANGE,
    WM_SIZE, WM_SYSCOLORCHANGE, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows_core::{PCWSTR, w};

const CLASS: PCWSTR = w!("Progman");

struct State {
    hwnd: HWND,
    origin: (i32, i32),
    surface: Option<Surface>,
    generation: usize,
    desk: Option<Desk>,
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
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
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
            *s.borrow_mut() = Some(State { hwnd, origin: (v.x, v.y), surface: None, generation: 1, desk: None })
        });
        wallpaper::spawn(hwnd, 1);
        match view::create(hwnd, Rect::new(0, 0, v.w, v.h)) {
            Ok(desk) => {
                let dv = desk.hwnd;
                with(|s| s.desk = Some(desk));
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

fn paint_into(hdc: HDC, r: Rect) {
    let drawn = with(|s| s.surface.as_ref().map(|sf| sf.blit(hdc, r)).is_some()).unwrap_or(false);
    if !drawn {
        let rc = windows::Win32::Foundation::RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() };
        // SAFETY: fills the given DC with the system background brush (not owned, not freed).
        unsafe { FillRect(hdc, &rc, GetSysColorBrush(COLOR_BACKGROUND)) };
    }
}

/// The whole screen changed (resolution, monitors): cover the virtual screen again.
fn relayout(hwnd: HWND) {
    let v = screen::virtual_screen();
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
            if let Some(dv) = defview() {
                let (w, h) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
                // SAFETY: resizes the child view window.
                let _ = unsafe { MoveWindow(dv, 0, 0, w, h, true) };
            }
            None
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
            }
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            let action = wp.0 as u32;
            if action == SPI_SETDESKWALLPAPER.0 {
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
            relayout(hwnd);
            forward(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            // Alt+F4 on the desktop: explorer shows the shut down dialog; not ours yet.
            log::line("Progman: WM_CLOSE ignored");
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            let desk = with(|s| s.desk.take()).flatten();
            if let Some(d) = desk {
                d.destroy();
            }
            with(|s| s.surface = None);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn client(hwnd: HWND) -> Rect {
    let mut rc = windows::Win32::Foundation::RECT::default();
    // SAFETY: reads this window's client rectangle.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc) };
    screen::rect(&rc)
}
