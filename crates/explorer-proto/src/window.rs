//! Win32 window: frameless overlapped window (DWM keeps the rounded corners, border and
//! shadow), system acrylic backdrop like MicaBar sets, never activated, closes itself.

use crate::scene::Scene;
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::FOLDERID_Downloads;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::w;

pub struct Opts {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub secs: u32,
    pub select: Option<usize>,
    pub hover: Option<usize>,
}

const TIMER_QUIT: usize = 1;

thread_local! {
    static SCENE: RefCell<Option<Scene>> = const { RefCell::new(None) };
}

fn backdrop(hwnd: HWND) {
    unsafe {
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let kind = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as _, 4);
    }
}

pub fn run(o: Opts) -> windows_core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        // Reparse the file system path so the parent chain runs through This PC, as
        // Explorer's breadcrumb does, not through the user's profile folder.
        let known = crate::shell::known(&FOLDERID_Downloads)?;
        let fs = known.GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)?.to_string().unwrap_or_default();
        let folder = crate::shell::parse(&fs).unwrap_or(known);
        let listing = crate::shell::list(&folder)?;
        let folder_path = folder.GetDisplayName(windows::Win32::UI::Shell::SIGDN_DESKTOPABSOLUTEPARSING)?;
        let folder_path = folder_path.to_string().unwrap_or_default();
        let nav = crate::shell::nav();
        shelldata::bench::log(&format!(
            "E list_ms={:.1} count={} nav={} at_ms={:.1}",
            listing.list_ms,
            listing.entries.len(),
            nav.len(),
            shelldata::bench::ms_since_process_start()
        ));
        let inst = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("ShellacExplorerProto"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("ShellacExplorerProto"),
            w!("shellac explorer"),
            WS_OVERLAPPEDWINDOW,
            o.x,
            o.y,
            o.w,
            o.h,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        // WM_NCCALCSIZE at creation carries wParam 0; recompute the frame so the whole
        // window becomes client area.
        SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)?;
        backdrop(hwnd);
        let mut scene = Scene::new(hwnd, o.w as u32, o.h as u32, listing, &folder_path, nav)?;
        scene.selected = o.select;
        scene.hovered = o.hover;
        SCENE.with(|s| *s.borrow_mut() = Some(scene));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        paint();
        shelldata::bench::log(&format!("E first_frame_ms={:.1}", shelldata::bench::ms_since_process_start()));
        SetTimer(Some(hwnd), TIMER_QUIT, o.secs * 1000, None);
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            shelldata::bench::log(&format!("E private_ws_kib_5s={}", shelldata::bench::private_ws_kib()));
        });
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
        if let Some(scene) = s.borrow().as_ref() {
            let _ = scene.draw();
        }
    });
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCALCSIZE if wp.0 != 0 => LRESULT(0),
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE | WM_LBUTTONDOWN => {
                let (x, y) = point(lp);
                let changed = SCENE.with(|s| {
                    let mut s = s.borrow_mut();
                    let Some(sc) = s.as_mut() else { return false };
                    let row = sc.row_at(x, y);
                    let before = (sc.hovered, sc.selected);
                    sc.hovered = row;
                    if msg == WM_LBUTTONDOWN {
                        sc.selected = row;
                    }
                    before != (sc.hovered, sc.selected)
                });
                if changed {
                    paint();
                }
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_QUIT => {
                let _ = DestroyWindow(hwnd);
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
