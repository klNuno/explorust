//! One-line shell actions (Win+R, Win+I and the other `bus::ShellAction`s). The hook posts
//! `Command::ShellAction` to explorust's own message window `explorust.shell`, on a thread of its
//! own, so they run in this plain-token process wherever the hook lives (in-process in a dev build,
//! in the UI-access `explorust-keys.exe` once installed) and what they start inherits no UI access.

use crate::log;
use bus::{Command, ShellAction};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, RegisterClassW, SW_SHOWNORMAL,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
};
use windows::core::{HSTRING, PCSTR, PCWSTR, w};

/// Creates the command window on its own thread and returns once it exists, so the hook started
/// next finds it.
pub fn spawn() {
    let (tx, rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let spawned = std::thread::Builder::new().name("actions".into()).spawn(move || {
        // SAFETY: COM for this thread, needed by the Run dialog and ShellExecute.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let Some(hwnd) = create() else {
            let _ = tx.send(false);
            return;
        };
        bus::listen(hwnd);
        let _ = tx.send(true);
        let mut msg = MSG::default();
        // SAFETY: a plain message loop on this thread.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
    if spawned.is_err() || !rx.recv().unwrap_or(false) {
        log::line("actions: command window not created, Win+R and Win+I pass through");
    }
}

fn create() -> Option<HWND> {
    let class = HSTRING::from(bus::class_of(bus::Owner::Shell));
    // SAFETY: registers a class whose name outlives the window, then makes a message-only window.
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: module.into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        match CreateWindowExW(WINDOW_EX_STYLE(0), &class, None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(module.into()), None) {
            Ok(h) => Some(h),
            Err(e) => {
                log::line(&format!("actions: CreateWindowEx: {e}"));
                None
            }
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == bus::MSG_COMMAND {
        if let Some((Command::ShellAction, arg)) = bus::decode(wparam, lparam) {
            match ShellAction::from_isize(arg) {
                Some(a) => {
                    log::line(&format!("action {a:?}"));
                    run(a);
                }
                None => log::line(&format!("action {arg}: unknown")),
            }
        }
        return LRESULT(0);
    }
    // SAFETY: default handling for every other message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn run(a: ShellAction) {
    match a.uri() {
        // SAFETY: plain ShellExecute of a URI.
        Some(uri) => unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(uri), None, None, SW_SHOWNORMAL);
        },
        None => run_dialog(),
    }
}

type RunFileDlg = unsafe extern "system" fn(isize, isize, isize, isize, isize, u32) -> i32;

fn run_dialog() {
    // SAFETY: shell32 exports RunFileDlg as ordinal 61 since Windows 95; called with no owner,
    // default icon, directory, title and prompt. It is modal and pumps this thread's messages.
    unsafe {
        let Ok(shell32) = LoadLibraryW(w!("shell32.dll")) else { return };
        let Some(f) = GetProcAddress(shell32, PCSTR(61 as *const u8)) else {
            log::line("RunFileDlg not found");
            return;
        };
        let f: RunFileDlg = std::mem::transmute(f);
        f(0, 0, 0, 0, 0, 0);
    }
}
