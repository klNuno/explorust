//! The hook itself. A chord is swallowed only when a part answers it, so Win+A (answered by
//! Windows' own quick settings host) and anything unowned pass through untouched. While Alt+Tab is
//! open, the arrows, Enter and Delete drive it as in Windows. Win+L and Ctrl+Alt+Del never reach a
//! hook.

use crate::chords::*;
use bus::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use windows::Win32::Foundation::{CloseHandle, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, GetCurrentThreadId, OpenEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, MSG,
    PostThreadMessageW, SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};
use windows::core::w;

static LOG: OnceLock<fn(&str)> = OnceLock::new();
static WIN: AtomicBool = AtomicBool::new(false);
/// Another key went down while Win was held, so releasing Win does not open Start.
static WIN_USED: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
/// Alt as the hook saw it: LLKHF_ALTDOWN is not set on a key pressed with Ctrl and Alt both held.
static ALT: AtomicBool = AtomicBool::new(false);
/// The switcher is open: Alt up commits, Esc cancels.
static SWITCHING: AtomicBool = AtomicBool::new(false);
/// Alt+Tab took a Tab since Alt went down: the app in front saw Alt down alone, so its Alt up is
/// masked or the app would show its menu or its key tips (Windows' own Alt+Tab leaves none).
static ALT_MASK: AtomicBool = AtomicBool::new(false);
/// A line `altmask` in `%LOCALAPPDATA%\explorust\parts-off.txt` leaves Alt up unmasked after
/// Alt+Tab (the before case of the key tips check).
static ALT_MASK_OFF: AtomicBool = AtomicBool::new(false);
/// The key whose down was swallowed, so its up is swallowed too.
static SWALLOWED: AtomicU32 = AtomicU32::new(0);
/// A part answered a chord (or the tap) since Win went down: Win up is masked.
static WIN_ANSWERED: AtomicBool = AtomicBool::new(false);
/// Win was tapped alone: Start is told once the masked Win up has gone through (see `regrant`).
static TAP_PENDING: AtomicBool = AtomicBool::new(false);
/// The hook thread, for the foreground grant made after the hook returns.
static THREAD: AtomicU32 = AtomicU32::new(0);
/// Thread message: grant the foreground again, now that the injected keys are processed.
const MSG_REGRANT: u32 = 0x8000 + 0x101;

/// Unassigned virtual key, sent before a swallowed Win up so Windows never sees a bare Win tap.
const VK_MASK: u16 = 0xE8;
/// dwExtraInfo of the keys the hook injects, so it lets them pass.
const OWN_INPUT: usize = 0x6578_706C;

pub(crate) fn log_line(msg: &str) {
    log(msg);
}

fn log(msg: &str) {
    if let Some(f) = LOG.get() {
        f(msg);
    }
}

/// Installs the hook on a thread of its own, which then pumps messages for it, and the activation
/// window (`crate::activate`) on another. Returns once both are in place, or the reason the hook is
/// not (a missing activation window is only logged: the parts then activate by themselves).
/// `log` writes one line to the host's log.
pub fn spawn(log_line: fn(&str)) -> Result<(), String> {
    let _ = LOG.set(log_line);
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if std::fs::read_to_string(base.join("explorust").join("parts-off.txt")).is_ok_and(|s| s.lines().any(|l| l.trim() == "altmask")) {
        ALT_MASK_OFF.store(true, Relaxed);
        log("altmask: off (parts-off.txt), Alt up after Alt+Tab reaches the app");
    }
    if let Err(e) = crate::activate::spawn() {
        log(&e);
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || {
            // SAFETY: installs a hook on this thread, which then pumps messages for it.
            let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) };
            if let Err(e) = hook {
                let _ = tx.send(Err(format!("hook not installed: {e}")));
                return;
            }
            // SAFETY: plain query of this thread's id.
            THREAD.store(unsafe { GetCurrentThreadId() }, Relaxed);
            let _ = tx.send(Ok(()));
            let mut msg = MSG::default();
            // SAFETY: a plain message loop on this thread.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                if msg.hwnd.is_invalid() && msg.message == MSG_REGRANT {
                    if TAP_PENDING.swap(false, Relaxed) {
                        post(Command::StartToggle, 0);
                    } else {
                        grant();
                    }
                    continue;
                }
                unsafe { DispatchMessageW(&msg) };
            }
        })
        .map_err(|e| format!("hook thread: {e}"))?;
    rx.recv().map_err(|_| "hook thread ended".to_string())?
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL, lParam points to a KBDLLHOOKSTRUCT for this call.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if kb.dwExtraInfo == OWN_INPUT {
            if !down && matches!(kb.vkCode, VK_LWIN | VK_RWIN | VK_LALT | VK_RALT | VK_ALT) {
                // Input resets who may take the foreground, and a masked Win or Alt up is input:
                // grant it again once this event is through (the Run dialog, Start and the window
                // Alt+Tab picked often come later).
                // SAFETY: posts to the hook thread, which pumps messages.
                let _ = unsafe { PostThreadMessageW(THREAD.load(Relaxed), MSG_REGRANT, WPARAM(0), LPARAM(0)) };
            }
            // SAFETY: our own mask key and Win up go on untouched.
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }
        if let Some(r) = key(kb.vkCode, down, kb.flags.0 & LLKHF_ALTDOWN.0 != 0) {
            return r;
        }
    }
    // SAFETY: passes the event on, as every hook must.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn swallow(vk: u32) -> LRESULT {
    SWALLOWED.store(vk, Relaxed);
    LRESULT(1)
}

/// Some(result) swallows the key.
fn key(vk: u32, down: bool, alt: bool) -> Option<LRESULT> {
    if !down && SWALLOWED.load(Relaxed) == vk {
        SWALLOWED.store(0, Relaxed);
        return Some(LRESULT(1));
    }
    match vk {
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => SHIFT.store(down, Relaxed),
        VK_CTRL | VK_LCTRL | VK_RCTRL => CTRL.store(down, Relaxed),
        VK_ALT | VK_LALT | VK_RALT => {
            ALT.store(down, Relaxed);
            if down {
                ALT_MASK.store(false, Relaxed);
            }
        }
        VK_LWIN | VK_RWIN => {
            if down {
                if !WIN.swap(true, Relaxed) {
                    WIN_USED.store(false, Relaxed);
                    WIN_ANSWERED.store(false, Relaxed);
                }
            } else {
                WIN.store(false, Relaxed);
                let answered = if WIN_USED.swap(true, Relaxed) {
                    WIN_ANSWERED.swap(false, Relaxed)
                } else {
                    // Start is told after the mask keys, so their input does not undo its grant.
                    let start = bus::target(Command::StartToggle.owner()).is_some();
                    TAP_PENDING.store(start, Relaxed);
                    start
                };
                // Windows opens its own Start or search when it sees Win go down and up alone,
                // which is what a swallowed chord leaves. When explorust answered, this Win up is
                // swallowed and sent again after an unassigned key, as AutoHotkey does.
                if answered && release_masked(vk, true) {
                    return Some(LRESULT(1));
                }
            }
            return None;
        }
        _ => {}
    }
    match vk {
        VK_ALT | VK_LALT | VK_RALT if !down => {
            // Sent first, so the commit's foreground grant comes after this input (see `hook`).
            let mask = ALT_MASK.swap(false, Relaxed) && release_masked(vk, vk == VK_RALT);
            if SWITCHING.swap(false, Relaxed) {
                post(Command::SwitcherCommit, 0);
            }
            return mask.then_some(LRESULT(1));
        }
        _ => {}
    }
    if !down {
        return None;
    }
    if is_escape(vk, CTRL.load(Relaxed), alt || ALT.load(Relaxed), SHIFT.load(Relaxed)) && escape() {
        return Some(swallow(vk));
    }
    if WIN.load(Relaxed) {
        WIN_USED.store(true, Relaxed);
        let (cmd, arg) = win_chord(vk, CTRL.load(Relaxed), SHIFT.load(Relaxed), alt || ALT.load(Relaxed))?;
        let handled = post(cmd, arg);
        if handled {
            WIN_ANSWERED.store(true, Relaxed);
        } else {
            log(&format!("{cmd:?} has no owner, Win+{vk:#x} passed through"));
        }
        return handled.then(|| swallow(vk));
    }
    if vk == VK_TAB && alt && post(Command::SwitcherNext, SHIFT.load(Relaxed) as isize) {
        SWITCHING.store(true, Relaxed);
        ALT_MASK.store(!ALT_MASK_OFF.load(Relaxed), Relaxed);
        return Some(swallow(vk));
    }
    if !SWITCHING.load(Relaxed) {
        return None;
    }
    let (cmd, arg) = switcher_key(vk)?;
    if matches!(cmd, Command::SwitcherCancel | Command::SwitcherCommit) {
        SWITCHING.store(false, Relaxed);
    }
    post(cmd, arg);
    Some(swallow(vk))
}

/// Sets the guard's fallback event (the guard then ends explorust and starts explorer). False when
/// no guard runs, so the key goes on to whoever else wants it.
fn escape() -> bool {
    // SAFETY: opens, sets and closes an event the guard created.
    let set = unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, w!(r"Local\explorust-guard-fallback")) {
            Ok(h) => {
                let r = SetEvent(h);
                let _ = CloseHandle(h);
                r.map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    };
    match &set {
        Ok(()) => log("Ctrl+Alt+Shift+F12: guard fallback requested"),
        Err(e) => log(&format!("Ctrl+Alt+Shift+F12: no guard ({e})")),
    }
    set.is_ok()
}

/// Sends the mask key down and up, then `key` up (Win, or Alt after Alt+Tab), marked as the
/// hook's own input. Win and right Alt are extended keys. False when Windows refused the input (a
/// hook without UI access under an elevated window): the real key up must then go through.
fn release_masked(released: u32, extended: bool) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    };
    let key = |vk: u16, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), dwFlags: flags, dwExtraInfo: OWN_INPUT, ..Default::default() } },
    };
    let inputs = [
        key(VK_MASK, KEYBD_EVENT_FLAGS(0)),
        key(VK_MASK, KEYEVENTF_KEYUP),
        key(released as u16, if extended { KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY } else { KEYEVENTF_KEYUP }),
    ];
    // SAFETY: SendInput reads the three INPUTs from a live array.
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        log(&format!("masked key up for {released:#x} not sent ({sent} of 3): the real one goes through"));
        return false;
    }
    true
}

/// Posts a command, first letting its part take the foreground: the key went to the hook, not to
/// the part, so Windows would otherwise refuse the window the part brings up.
fn post(cmd: Command, arg: isize) -> bool {
    grant();
    bus::post(cmd, arg)
}

/// Lets any process take the foreground until the next input.
fn grant() {
    // SAFETY: plain call.
    if let Err(e) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
        log(&format!("AllowSetForegroundWindow: {e}"));
    }
}
