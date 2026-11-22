//! The escape hatch: Ctrl+Alt+Shift+F12, seen by a low-level keyboard hook on its own thread,
//! sets the fallback event at any moment, whatever explorust is doing.

use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThreadId, SetEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F12, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
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

pub struct Hook {
    tid: u32,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        EVENT.store(0, Ordering::Relaxed);
        // SAFETY: the handler creates its queue before reporting its thread ID.
        if unsafe { PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

/// Starts the hook thread; `fallback` remains live until the returned hook is dropped.
pub fn spawn(fallback: HANDLE) -> Option<Hook> {
    EVENT.store(fallback.0 as isize, Ordering::Relaxed);
    let (ready, result) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new().name("hotkey".into()).spawn(move || {
        // SAFETY: a global low-level hook needs this module's handle and a message loop on this thread.
        let module = unsafe { GetModuleHandleW(None) }.ok();
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0) };
        let hook = match hook {
            Ok(hook) => hook,
            Err(e) => {
                crate::log::line(&format!("hotkey hook failed: {e}"));
                return;
            }
        };
        // SAFETY: standard message loop for the hook thread.
        let mut msg = MSG::default();
        let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
        let _ = ready.send(unsafe { GetCurrentThreadId() });
        crate::log::line("hotkey Ctrl+Alt+Shift+F12 armed");
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // SAFETY: this thread installed the hook and owns it.
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    }).ok()?;
    Some(Hook { tid: result.recv().ok()?, thread: Some(thread) })
}

/// True while Shift is held: at logon this skips explorust and starts plain explorer.
pub fn shift_held() -> bool {
    down(VK_SHIFT.0)
}
