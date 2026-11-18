//! Win32 window: no activation, topmost tool window, hover tracking and timers.

use crate::scene::Scene;
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::w;

pub struct Opts {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub secs: u32,
    pub bench: bool,
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_ANIM: usize = 1;
const TIMER_CLOCK: usize = 2;
const TIMER_QUIT: usize = 3;

thread_local! {
    static SCENE: RefCell<Option<Scene>> = const { RefCell::new(None) };
}

pub fn run(o: Opts) -> windows_core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let inst = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("ExplorustTaskbarProto"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("ExplorustTaskbarProto"),
            w!("explorust taskbar A"),
            WS_POPUP,
            o.x,
            o.y,
            o.w,
            shelldata::layout::BAR_H as i32,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: shelldata::layout::TINT_ABGR });
        let scene = Scene::new(hwnd, o.w as u32)?;
        SCENE.with(|s| *s.borrow_mut() = Some(scene));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        paint();
        shelldata::bench::log(&format!("A first_frame_ms={:.1}", shelldata::bench::ms_since_process_start()));
        SetTimer(Some(hwnd), TIMER_CLOCK, shelldata::clock::ms_to_next_minute() as u32, None);
        SetTimer(Some(hwnd), TIMER_QUIT, o.secs * 1000, None);
        if o.bench {
            let h = hwnd.0 as isize;
            shelldata::bench::spawn_idle_bench("A", move || {
                let _ = PostMessageW(Some(HWND(h as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            });
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

fn paint() {
    SCENE.with(|s| {
        if let Some(scene) = s.borrow_mut().as_mut() {
            let _ = scene.draw();
        }
    });
}

fn with_scene<R>(f: impl FnOnce(&mut Scene) -> R) -> Option<R> {
    SCENE.with(|s| s.borrow_mut().as_mut().map(f))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                let x = (lp.0 & 0xffff) as i16 as f32;
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                if with_scene(|s| s.set_hover_x(Some(x))).unwrap_or(false) {
                    SetTimer(Some(hwnd), TIMER_ANIM, 16, None);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if with_scene(|s| s.set_hover_x(None)).unwrap_or(false) {
                    SetTimer(Some(hwnd), TIMER_ANIM, 16, None);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_ANIM => {
                        if !with_scene(|s| s.step()).unwrap_or(false) {
                            let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                        }
                        paint();
                    }
                    TIMER_CLOCK => {
                        with_scene(|s| s.refresh_clock());
                        paint();
                        SetTimer(Some(hwnd), TIMER_CLOCK, shelldata::clock::ms_to_next_minute() as u32, None);
                    }
                    TIMER_QUIT => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = Default::default();
                windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
                paint();
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
