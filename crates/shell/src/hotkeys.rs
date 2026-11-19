//! Windows' shell hotkeys, which explorer owns and which do nothing without it: Win alone, Win+S,
//! Win+Tab, Win+N, Win+X, Win+1..9 and the rest, plus Alt+Tab. A low-level keyboard hook on its own
//! thread turns them into bus commands. A chord is swallowed only when a part answers it, so Win+A
//! (answered by Windows' own quick settings host) and anything unowned pass through untouched.
//! While Alt+Tab is open, the arrows, Enter and Delete drive it as in Windows.
//! Ctrl+Win+D/Left/Right/F4 drive virtual desktops; Win+R, Win+I, Win+Shift+S and a few more are
//! one-line shell actions. Win+L and Ctrl+Alt+Del never reach a hook.

use crate::{actions, log};
use bus::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, MSG,
    SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

static WIN: AtomicBool = AtomicBool::new(false);
/// Another key went down while Win was held, so releasing Win does not open Start.
static WIN_USED: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
/// The switcher is open: Alt up commits, Esc cancels.
static SWITCHING: AtomicBool = AtomicBool::new(false);
/// The key whose down was swallowed, so its up is swallowed too.
static SWALLOWED: AtomicU32 = AtomicU32::new(0);
/// A part answered a chord (or the tap) since Win went down: Win up is masked.
static WIN_ANSWERED: AtomicBool = AtomicBool::new(false);

const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_DELETE: u32 = 0x2E;
const VK_SHIFT: u32 = 0x10;
const VK_CTRL: u32 = 0x11;
const VK_LCTRL: u32 = 0xA2;
const VK_RCTRL: u32 = 0xA3;
const VK_ALT: u32 = 0x12;
const VK_ESC: u32 = 0x1B;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LALT: u32 = 0xA4;
const VK_RALT: u32 = 0xA5;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
/// Unassigned virtual key, sent before a swallowed Win up so Windows never sees a bare Win tap.
const VK_MASK: u16 = 0xE8;
/// dwExtraInfo of the keys explorust injects, so its own hook lets them pass.
const OWN_INPUT: usize = 0x6578_706C;

pub fn spawn() {
    let _ = std::thread::Builder::new().name("hotkeys".into()).spawn(|| {
        // SAFETY: installs a hook on this thread, which then pumps messages for it.
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) };
        if let Err(e) = hook {
            log::line(&format!("hotkeys: hook not installed: {e}"));
            return;
        }
        log::line("hotkeys armed");
        let mut msg = MSG::default();
        // SAFETY: a plain message loop on this thread.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe { DispatchMessageW(&msg) };
        }
    });
}

/// What a key does with Win held: a bus command with its argument, or a shell action.
enum WinChord {
    Bus(Command, isize),
    Action(actions::Action),
}

fn win_chord(vk: u32, ctrl: bool, shift: bool) -> Option<WinChord> {
    use WinChord::*;
    if ctrl {
        return Some(match vk {
            0x44 => Bus(Command::DesktopNew, 0),        // Ctrl+Win+D
            0x25 => Bus(Command::DesktopSwitch, -1),    // Ctrl+Win+Left
            0x27 => Bus(Command::DesktopSwitch, 1),     // Ctrl+Win+Right
            0x73 => Bus(Command::DesktopClose, 0),      // Ctrl+Win+F4
            _ => return None,
        });
    }
    if shift {
        return match vk {
            0x53 => Some(Action(actions::Action::Uri("ms-screenclip:"))), // Win+Shift+S
            _ => None,
        };
    }
    Some(match vk {
        0x53 => Bus(Command::Search, 0),                // S
        VK_TAB => Bus(Command::TaskView, 0),            // Tab
        0x4E => Bus(Command::NotificationCenter, 0),    // N
        // Win+A stays with Windows' own quick settings host until the notify part answers it.
        0x58 => Bus(Command::WinX, 0),                  // X
        0x31..=0x39 => Bus(Command::TaskbarNth, (vk - 0x30) as isize), // 1..9
        0x54 => Bus(Command::TaskbarFocus, 0),          // T
        0x42 => Bus(Command::TrayFocus, 0),             // B
        0x45 => Bus(Command::FilesOpen, 0),             // E
        0x44 | 0x4D => Bus(Command::ShowDesktop, 0),    // D, M
        0x5A => Bus(Command::SnapLayouts, 0),           // Z
        0x56 => Bus(Command::ClipboardHistory, 0),      // V
        0xBE | 0xBA => Bus(Command::EmojiPanel, 0),     // . and ;
        0x52 => Action(actions::Action::RunDialog),     // R
        0x49 => Action(actions::Action::Uri("ms-settings:")), // I
        0x55 => Action(actions::Action::Uri("ms-settings:easeofaccess")), // U
        0x50 => Action(actions::Action::Uri("ms-settings-displays-topology:projection")), // P
        0x4B => Action(actions::Action::Uri("ms-settings-connectabledevices:devicediscovery")), // K
        0x13 => Action(actions::Action::Uri("ms-settings:about")), // Pause
        _ => return None,
    })
}

fn swallow(vk: u32) -> LRESULT {
    SWALLOWED.store(vk, Relaxed);
    LRESULT(1)
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL, lParam points to a KBDLLHOOKSTRUCT for this call.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if kb.dwExtraInfo == OWN_INPUT {
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

/// Some(result) swallows the key.
fn key(vk: u32, down: bool, alt: bool) -> Option<LRESULT> {
    if !down && SWALLOWED.load(Relaxed) == vk {
        SWALLOWED.store(0, Relaxed);
        return Some(LRESULT(1));
    }
    match vk {
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => SHIFT.store(down, Relaxed),
        VK_CTRL | VK_LCTRL | VK_RCTRL => CTRL.store(down, Relaxed),
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
                    post(Command::StartToggle, 0)
                };
                // Windows opens its own Start or search when it sees Win go down and up alone,
                // which is what a swallowed chord leaves. When explorust answered, this Win up is
                // swallowed and sent again after an unassigned key, as AutoHotkey does.
                if answered {
                    release_win_masked(vk);
                    return Some(LRESULT(1));
                }
            }
            return None;
        }
        VK_ALT | VK_LALT | VK_RALT if !down && SWITCHING.swap(false, Relaxed) => {
            bus::post(Command::SwitcherCommit, 0);
            return None;
        }
        _ => {}
    }
    if !down {
        return None;
    }
    if WIN.load(Relaxed) {
        WIN_USED.store(true, Relaxed);
        let handled = match win_chord(vk, CTRL.load(Relaxed), SHIFT.load(Relaxed))? {
            WinChord::Bus(cmd, arg) => {
                let ok = post(cmd, arg);
                if !ok {
                    log::line(&format!("hotkeys: {cmd:?} has no owner, Win+{vk:#x} passed through"));
                }
                ok
            }
            WinChord::Action(a) => actions::send(a),
        };
        if handled {
            WIN_ANSWERED.store(true, Relaxed);
        }
        return handled.then(|| swallow(vk));
    }
    if vk == VK_TAB && alt && post(Command::SwitcherNext, SHIFT.load(Relaxed) as isize) {
        SWITCHING.store(true, Relaxed);
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

/// What a key does while Alt+Tab is open.
fn switcher_key(vk: u32) -> Option<(Command, isize)> {
    Some(match vk {
        VK_ESC => (Command::SwitcherCancel, 0),
        VK_RETURN => (Command::SwitcherCommit, 0),
        VK_LEFT => (Command::SwitcherMove, 0),
        VK_RIGHT => (Command::SwitcherMove, 1),
        VK_UP => (Command::SwitcherMove, 2),
        VK_DOWN => (Command::SwitcherMove, 3),
        VK_DELETE => (Command::SwitcherClose, 0),
        _ => return None,
    })
}

/// Sends the mask key down and up, then `win` up, marked as explorust's own input.
fn release_win_masked(win: u32) {
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
        key(win as u16, KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY),
    ];
    // SAFETY: SendInput reads the three INPUTs from a live array.
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// Posts a command, first letting its part take the foreground: the key went to the hook, not to
/// the part, so Windows would otherwise refuse the window the part brings up.
fn post(cmd: Command, arg: isize) -> bool {
    // SAFETY: plain call; it only widens who may set the foreground until the next input.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    bus::post(cmd, arg)
}
