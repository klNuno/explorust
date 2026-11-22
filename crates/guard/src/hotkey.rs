//! The escape hatch: Ctrl+Alt+Shift+F12, seen by a low-level keyboard hook on its own thread,
//! sets the fallback event at any moment, whatever explorust is doing.

use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F12, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

static EVENT: AtomicIsize = AtomicIsize::new(0);
/// Ctrl, Alt and Shift as the hook saw them, one bit each. The async key state lags behind a hook
/// for injected input, so the hook keeps its own.
static MODS: AtomicU8 = AtomicU8::new(0);
const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;

fn modifier(vk: u32) -> u8 {
    match vk {
        0x11 | 0xA2 | 0xA3 => CTRL,
        0x12 | 0xA4 | 0xA5 => ALT,
        0x10 | 0xA0 | 0xA1 => SHIFT,
        _ => 0,
    }
}

fn down(vk: u16) -> bool {
    // SAFETY: reads the async key state only.
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL, lparam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let m = modifier(k.vkCode);
        if m != 0 {
            if down { MODS.fetch_or(m, Ordering::Relaxed) } else { MODS.fetch_and(!m, Ordering::Relaxed) };
        } else if down && k.vkCode == VK_F12.0 as u32 && MODS.load(Ordering::Relaxed) == CTRL | ALT | SHIFT {
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
        crate::log::line("hotkey Ctrl+Alt+Shift+F12 armed");
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
