//! The escape hatch: Ctrl+Alt+Shift+F12, seen by a low-level keyboard hook on its own thread,
//! sets the fallback event at any moment, whatever explorust is doing.

use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_F12, VK_MENU, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

static EVENT: AtomicIsize = AtomicIsize::new(0);

fn down(vk: u16) -> bool {
    // SAFETY: reads the async key state only.
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        // SAFETY: for WH_KEYBOARD_LL, lparam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if k.vkCode == VK_F12.0 as u32 && down(VK_CONTROL.0) && down(VK_MENU.0) && down(VK_SHIFT.0) {
            let ev = EVENT.load(Ordering::Relaxed);
            if ev != 0 {
                crate::log::line("hotkey Ctrl+Alt+Shift+F12: fallback requested");
                // SAFETY: the event handle lives for the whole process.
                let _ = unsafe { SetEvent(HANDLE(ev as *mut _)) };
                return LRESULT(1);
            }
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Starts the hook thread; `fallback` is set when the combination is pressed.
pub fn spawn(fallback: HANDLE) {
    EVENT.store(fallback.0 as isize, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("hotkey".into()).spawn(|| {
        // SAFETY: a global low-level hook needs this module's handle and a message loop on this thread.
        let module = unsafe { GetModuleHandleW(None) }.ok();
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0) };
        if let Err(e) = hook {
            crate::log::line(&format!("hotkey hook failed: {e}"));
            return;
        }
        let mut msg = MSG::default();
        // SAFETY: standard message loop for the hook thread.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

/// True while Shift is held: at logon this skips explorust and starts plain explorer.
pub fn shift_held() -> bool {
    down(VK_SHIFT.0)
}
