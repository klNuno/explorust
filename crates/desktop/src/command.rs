//! The command window `explorust.desktop` (message-only): ShowDesktop (Win+D, Win+M) and
//! Shutdown from the bus.

use crate::{log, progman, showdesktop};
use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, PostQuitMessage, RegisterClassExW, UnregisterClassW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW,
};
use windows_core::HSTRING;

thread_local! {
    static HWND_: Cell<Option<HWND>> = const { Cell::new(None) };
}

pub fn create() -> windows_core::Result<HWND> {
    let class = HSTRING::from(bus::class_of(bus::Owner::Desktop));
    // SAFETY: registers and creates a message-only window owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: windows_core::PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class,
            &class,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(inst.into()),
            None,
        )?;
        bus::listen(h);
        HWND_.with(|c| c.set(Some(h)));
        Ok(h)
    }
}

pub fn destroy() {
    let class = HSTRING::from(bus::class_of(bus::Owner::Desktop));
    // SAFETY: destroys this thread's window and unregisters its class.
    unsafe {
        if let Some(h) = HWND_.with(|c| c.take()) {
            let _ = DestroyWindow(h);
        }
        if let Ok(inst) = GetModuleHandleW(None) {
            let _ = UnregisterClassW(&class, Some(inst.into()));
        }
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == bus::MSG_COMMAND {
        let r = std::panic::catch_unwind(|| command(wp, lp));
        if r.is_err() {
            log::line("command: panic");
        }
        return LRESULT(0);
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn command(wp: WPARAM, lp: LPARAM) {
    let Some((cmd, arg)) = bus::decode(wp, lp) else { return };
    log::line(&format!("command {cmd:?} {arg}"));
    match cmd {
        bus::Command::ShowDesktop => showdesktop::press(progman::hwnd()),
        bus::Command::Shutdown => {
            // SAFETY: ends this thread's windows and message loop.
            unsafe {
                if let Some(p) = progman::hwnd() {
                    let _ = DestroyWindow(p);
                }
                PostQuitMessage(0);
            }
        }
        _ => {}
    }
}
