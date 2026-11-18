//! Windows' shell hotkeys, which explorer owns and which do nothing without it: Win alone, Win+S,
//! Win+Tab, Win+N, Win+X, Win+1..9 and the rest, plus Alt+Tab. A low-level keyboard hook on its own
//! thread turns them into bus commands. A chord is swallowed only when a part answers it, so Win+A
//! (answered by Windows' own quick settings host) and anything unowned pass through untouched.
//! Ctrl+Win+D/Left/Right/F4 drive virtual desktops; Win+R, Win+I, Win+Shift+S and a few more are
//! one-line shell actions. Win+L and Ctrl+Alt+Del never reach a hook.

use crate::{actions, log};
use bus::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, MSG,
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

const VK_TAB: u32 = 0x09;
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
        0x41 => Bus(Command::QuickSettings, 0),         // A
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
                }
            } else {
                WIN.store(false, Relaxed);
                if !WIN_USED.swap(true, Relaxed) {
                    bus::post(Command::StartToggle, 0);
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
            WinChord::Bus(cmd, arg) => bus::post(cmd, arg),
            WinChord::Action(a) => actions::send(a),
        };
        return handled.then(|| swallow(vk));
    }
    if vk == VK_TAB && alt && bus::post(Command::SwitcherNext, SHIFT.load(Relaxed) as isize) {
        SWITCHING.store(true, Relaxed);
        return Some(swallow(vk));
    }
    if vk == VK_ESC && SWITCHING.swap(false, Relaxed) {
        bus::post(Command::SwitcherCancel, 0);
        return Some(swallow(vk));
    }
    None
}
