//! The escape hatch: Ctrl+Alt+Shift+F12 sets the fallback event at any moment, whatever explorust
//! is doing. It is a hotkey registered with Windows (`RegisterHotKey`) on a thread of its own, never
//! a keyboard hook: the guard is told about that one combination and sees no other key, and Windows
//! delivers it over elevated windows too. Once explorer has the session the hotkey is released
//! (`release`), so a guard that only keeps explorer running holds nothing tied to the keyboard.

use std::sync::Mutex;
use std::sync::mpsc::sync_channel;
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HANDLE, LPARAM, WPARAM};
use windows::Win32::System::Threading::{GetCurrentThreadId, SetEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_F12,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER};

/// The id of the hotkey on its thread.
const ID: i32 = 1;
const MODS: HOT_KEY_MODIFIERS = HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_NOREPEAT.0);

struct Armed {
    thread: u32,
    join: JoinHandle<()>,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

/// Registers the hotkey on its own thread; `fallback` is set when the combination is pressed.
/// Returns once the hotkey is registered or Windows refused it (another program owns the
/// combination; explorust's keys helper and `--fallback` still reach the event then).
pub fn spawn(fallback: HANDLE) {
    let event = fallback.0 as isize;
    let (tx, rx) = sync_channel(1);
    let join = std::thread::Builder::new().name("hotkey".into()).spawn(move || {
        // SAFETY: PeekMessage creates this thread's message queue before its id is handed out, so
        // `release` can always post WM_QUIT to it.
        let mut msg = MSG::default();
        let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };
        // SAFETY: a thread hotkey (no window): WM_HOTKEY comes to this thread's queue.
        if let Err(e) = unsafe { RegisterHotKey(None, ID, MODS, VK_F12.0 as u32) } {
            crate::log::line(&format!("hotkey Ctrl+Alt+Shift+F12 not registered: {e}"));
            let _ = tx.send(None);
            return;
        }
        crate::log::line("hotkey Ctrl+Alt+Shift+F12 armed");
        // SAFETY: plain query of this thread's id.
        let _ = tx.send(Some(unsafe { GetCurrentThreadId() }));
        // SAFETY: a thread message loop; WM_QUIT from `release` ends it.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            if msg.message == WM_HOTKEY && msg.wParam.0 == ID as usize {
                crate::log::line("hotkey Ctrl+Alt+Shift+F12: fallback requested");
                // SAFETY: the event handle lives for the whole process.
                let _ = unsafe { SetEvent(HANDLE(event as *mut _)) };
            }
        }
        // SAFETY: drops the registration this thread made.
        let _ = unsafe { UnregisterHotKey(None, ID) };
        crate::log::line("hotkey Ctrl+Alt+Shift+F12 released");
    });
    let Ok(join) = join else {
        crate::log::line("hotkey thread did not start");
        return;
    };
    match rx.recv() {
        Ok(Some(thread)) => *ARMED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Armed { thread, join }),
        _ => {
            let _ = join.join();
        }
    }
}

/// Unregisters the hotkey and ends its thread; nothing to do when it is not armed.
pub fn release() {
    let Some(armed) = ARMED.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    // SAFETY: the thread's queue exists (made before its id was sent) until it has seen WM_QUIT.
    if unsafe { PostThreadMessageW(armed.thread, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok() {
        let _ = armed.join.join();
    }
}

/// True while Shift is held: at logon this skips explorust and starts plain explorer.
pub fn shift_held() -> bool {
    // SAFETY: reads the async key state of one key only.
    (unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Event;
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_MENU,
    };

    /// True when this thread could register Ctrl+Alt+Shift+F12, i.e. nobody holds it.
    fn combination_free() -> bool {
        // SAFETY: registers and at once unregisters a thread hotkey of this test thread.
        unsafe {
            let free = RegisterHotKey(None, 0x7E57, MODS, VK_F12.0 as u32).is_ok();
            if free {
                let _ = UnregisterHotKey(None, 0x7E57);
            }
            free
        }
    }

    fn press_escape() {
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() },
            },
        };
        let keys = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_F12];
        let mut inputs: Vec<INPUT> = keys.iter().map(|&k| key(k, false)).collect();
        inputs.extend(keys.iter().rev().map(|&k| key(k, true)));
        // SAFETY: the inputs are fully initialized keyboard events.
        unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    }

    /// The escape is a registered hotkey (it holds the combination, so no other key reaches the
    /// guard), it sets the fallback event when pressed, and `release` gives the combination back.
    /// Needs an interactive desktop: run it in the guest with `os win run --desktop`.
    #[test]
    #[ignore = "needs an interactive Windows desktop"]
    fn escape_is_a_registered_hotkey_released_after_fallback() {
        assert!(combination_free(), "another program holds Ctrl+Alt+Shift+F12");
        let fallback = Event::named(&format!(r"Local\explorust-hotkey-test-{}", std::process::id())).unwrap();
        spawn(fallback.0);
        assert!(!combination_free(), "the guard did not register Ctrl+Alt+Shift+F12 with Windows");
        press_escape();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !fallback.signalled() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(fallback.signalled(), "Ctrl+Alt+Shift+F12 did not set the fallback event");
        release();
        assert!(combination_free(), "the hotkey stayed registered after release");
    }
}
