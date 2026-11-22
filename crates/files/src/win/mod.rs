//! The Files part on its UI thread: the command window `explorust.files` (bus commands and the
//! `WM_COPYDATA` handover), a hidden window kept ready, and the message loop.

mod chrome;
mod clipboard;
mod details;
mod frame;
mod gfx;
mod inbox;
mod log;
mod menus;
mod move_progress;
mod operations;
mod search;
mod share;
mod shell;
mod shellwindows;
mod tab;
mod views;

use crate::handover;
use std::cell::RefCell;
use std::sync::Arc;
use views::{Destination, Open, Windows};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::HSTRING;

const TIMER_SPARE: usize = 1;
/// The next ready window is made this long after a window opened, off its first frames.
const SPARE_DELAY_MS: u32 = 800;

thread_local! {
    static VIEWS: RefCell<Option<Arc<Windows>>> = const { RefCell::new(None) };
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

fn request_open(hwnd: HWND, destination: Destination, requested: Instant, queued_ms: u32) {
    VIEWS.with(|views| {
        if let Some(views) = views.borrow().as_ref() {
            views.open(Open { destination, position: None, requested, queued_ms });
        }
    });
    let _ = unsafe { SetTimer(Some(hwnd), TIMER_SPARE, SPARE_DELAY_MS, None) };
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
                        request_open(hwnd, Destination::Home, t0, queued);
                    }
                    Some((bus::Command::Shutdown, _)) => {
                        log::line("shutdown");
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COPYDATA => {
                let t0 = Instant::now();
                let cds = &*(lp.0 as *const COPYDATASTRUCT);
                let select = cds.dwData == handover::MAGIC_SELECT;
                if (cds.dwData != handover::MAGIC && !select) || cds.lpData.is_null() {
                    return LRESULT(0);
                }
                let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                let Some(path) = handover::decode(bytes) else { return LRESULT(0) };
                // Reply first: the sender waits in SendMessage.
                let _ = ReplyMessage(LRESULT(1));
                request_open(hwnd, Destination::Path(path, select), t0, 0);
                LRESULT(1)
            }
            clipboard::MSG_COMMAND => {
                clipboard::execute(wp.0);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_SPARE => {
                let _ = KillTimer(Some(hwnd), TIMER_SPARE);
                VIEWS.with(|views| {
                    if let Some(views) = views.borrow().as_ref() { views.prewarm() }
                });
                LRESULT(0)
            }
            WM_DESTROY => {
                VIEWS.with(|views| {
                    if let Some(windows) = views.borrow_mut().take() { windows.shutdown() }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn run_with_initial(initial: Option<&str>) {
    let ole = unsafe { windows::Win32::System::Ole::OleInitialize(None) };
    if let Err(error) = ole {
        log::line(&format!("Files command OLE initialization failed: {error}"));
        return;
    }
    let t0 = Instant::now();
    let cmd = unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        // A direct-path test can run beside the real Files part. Sharing its command class would
        // make the shell's watchdog find this window instead of the one inside explorust.exe.
        let name = if initial.is_some() {
            HSTRING::from("explorust.FilesTestCommands")
        } else {
            HSTRING::from(bus::class_of(bus::Owner::Files))
        };
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
                windows::Win32::System::Ole::OleUninitialize();
                return;
            }
        }
    };
    bus::listen(cmd);
    unsafe {
        let _ = ChangeWindowMessageFilterEx(cmd, WM_COPYDATA, MSGFLT_ALLOW, None);
    }
    let windows = Windows::new(cmd);
    VIEWS.with(|views| *views.borrow_mut() = Some(windows.clone()));
    log::line(&format!("files: listening in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0));
    windows.prewarm();
    if let Some(path) = initial {
        request_open(cmd, Destination::Path(path.to_owned(), false), Instant::now(), 0);
    }
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.hwnd.is_invalid() && msg.message == clipboard::MSG_RETIRE {
                clipboard::retire(msg.wParam.0 != 0);
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if IsWindow(Some(cmd)).as_bool() {
            let _ = DestroyWindow(cmd);
        }
    }
    clipboard::release();
    unsafe { windows::Win32::System::Ole::OleUninitialize() };
    log::line("files: stopped");
}
