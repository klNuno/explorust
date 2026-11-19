//! The Files part on its UI thread: the command window `explorust.files` (bus commands and the
//! `WM_COPYDATA` handover), a hidden window kept ready, and the message loop.

mod chrome;
mod frame;
mod gfx;
mod log;
mod menus;
mod shell;
mod tab;

use crate::handover;
use shell::Pidl;
use std::cell::Cell;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::HSTRING;

const TIMER_SPARE: usize = 1;
/// The next ready window is made this long after a window opened, off its first frames.
const SPARE_DELAY_MS: u32 = 800;

thread_local! {
    static SPARE: Cell<Option<HWND>> = const { Cell::new(None) };
    static OPENED: Cell<i32> = const { Cell::new(0) };
}

/// Dark menus and controls for the process; the dev exe calls it before `run`, the shell should.
pub fn dark_mode_for_process() {
    shell::dark_process();
}

/// The user's UI language tag, such as `fr-FR`.
pub fn ui_language() -> String {
    let id = unsafe { GetUserDefaultUILanguage() };
    match id & 0x3ff {
        0x0c => "fr-FR".into(),
        _ => "en-US".into(),
    }
}

fn next_position() -> (i32, i32) {
    let n = OPENED.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    (60 + 28 * (n % 6), 30 + 28 * (n % 6))
}

/// Makes the hidden window the next open shows at once.
fn prepare_spare() {
    if SPARE.with(|s| s.get()).is_some() {
        return;
    }
    let t0 = Instant::now();
    let Some(home) = shell::home() else { return };
    let (x, y) = next_position();
    match frame::create(&home, x, y) {
        Ok(h) => {
            SPARE.with(|s| s.set(Some(h)));
            log::line(&format!("spare window ready in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0));
        }
        Err(e) => log::line(&format!("spare window failed: {e}")),
    }
}

/// Opens a window on `path` (Home when `None`). `t0` is when the request arrived.
fn open(cmd: HWND, path: Option<&str>, t0: Instant, queued_ms: u32) {
    let place = match path {
        Some(p) => match Pidl::parse(p) {
            Some(pidl) => Some(pidl),
            None => {
                log::line(&format!("open {p:?}: not found"));
                return;
            }
        },
        None => None,
    };
    let spare = SPARE.with(|s| s.take());
    let mut later = None;
    let (hwnd, from) = match spare {
        Some(h) => {
            if let Some(p) = place {
                // Navigate after the first frame; the view stays hidden until it arrives.
                frame::hide_view(h);
                later = Some(p);
            }
            (h, "ready window")
        }
        None => {
            let Some(target) = place.clone().or_else(shell::home) else { return };
            let (x, y) = next_position();
            match frame::create(&target, x, y) {
                Ok(h) => (h, "new window"),
                Err(e) => {
                    log::line(&format!("open failed: {e}"));
                    return;
                }
            }
        }
    };
    frame::set_opened(hwnd, t0);
    let before = t0.elapsed().as_secs_f64() * 1000.0;
    let (show_ms, draw_ms, activate_ms) = frame::show(hwnd);
    let presented = before + show_ms + draw_ms;
    let _ = unsafe { DwmFlush() };
    let composed = t0.elapsed().as_secs_f64() * 1000.0;
    log::line(&format!(
        "open {}: first frame presented {presented:.1} ms after the command ({from}, queued {queued_ms} ms; \
         prepare {before:.1}, ShowWindow {show_ms:.1}, draw and Present {draw_ms:.1}), activated after it in \
         {activate_ms:.1} ms, composed by {composed:.1} ms",
        path.unwrap_or("Home")
    ));
    if let Some(p) = later {
        frame::browse(hwnd, &p);
    }
    unsafe {
        SetTimer(Some(cmd), TIMER_SPARE, SPARE_DELAY_MS, None);
    }
}

unsafe extern "system" fn cmd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            bus::MSG_COMMAND => {
                let t0 = Instant::now();
                let queued = GetTickCount().wrapping_sub(GetMessageTime() as u32);
                match bus::decode(wp, lp) {
                    Some((bus::Command::FilesOpen, arg)) => {
                        if arg != 0 {
                            log::line(&format!("FilesOpen lParam {arg}: no path protocol yet, opening Home"));
                        }
                        open(hwnd, None, t0, queued);
                    }
                    Some((bus::Command::Shutdown, _)) => {
                        log::line("shutdown");
                        frame::close_all();
                        if let Some(s) = SPARE.with(|s| s.take()) {
                            let _ = SendMessageW(s, WM_CLOSE, None, None);
                        }
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COPYDATA => {
                let t0 = Instant::now();
                let cds = &*(lp.0 as *const COPYDATASTRUCT);
                if cds.dwData != handover::MAGIC || cds.lpData.is_null() {
                    return LRESULT(0);
                }
                let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                let Some(path) = handover::decode(bytes) else { return LRESULT(0) };
                // Reply first: the sender waits in SendMessage.
                let _ = ReplyMessage(LRESULT(1));
                open(hwnd, Some(&path), t0, 0);
                LRESULT(1)
            }
            WM_TIMER if wp.0 == TIMER_SPARE => {
                let _ = KillTimer(Some(hwnd), TIMER_SPARE);
                prepare_spare();
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

pub fn run() {
    let t0 = Instant::now();
    if let Err(e) = unsafe { OleInitialize(None) } {
        log::line(&format!("OleInitialize failed: {e}"));
        return;
    }
    let strings = crate::text::for_language(&ui_language());
    let sh = match gfx::Shared::new() {
        Ok(s) => s,
        Err(e) => {
            log::line(&format!("graphics failed: {e}"));
            unsafe { OleUninitialize() };
            return;
        }
    };
    let cmd = unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let name = HSTRING::from(bus::class_of(bus::Owner::Files));
        let class = WNDCLASSW {
            lpfnWndProc: Some(cmd_proc),
            hInstance: inst.into(),
            lpszClassName: windows_core::PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
        match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            &name,
            &name,
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(inst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::line(&format!("command window failed: {e}"));
                OleUninitialize();
                return;
            }
        }
    };
    bus::listen(cmd);
    unsafe {
        let _ = ChangeWindowMessageFilterEx(cmd, WM_COPYDATA, MSGFLT_ALLOW, None);
    }
    frame::init(sh, strings, cmd);
    log::line(&format!("files: listening, devices in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0));
    prepare_spare();
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if frame::accelerator(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if frame::count() > 0 {
            frame::close_all();
        }
        OleUninitialize();
    }
    log::line("files: stopped");
}
