//! Which key does what: the Win chords, the keys of an open Alt+Tab and the guard's escape key.
//! Plain logic, tested on any OS.

use bus::{Command, ShellAction};

pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CTRL: u32 = 0x11;
pub const VK_ALT: u32 = 0x12;
pub const VK_ESC: u32 = 0x1B;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_DELETE: u32 = 0x2E;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
pub const VK_F12: u32 = 0x7B;
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCTRL: u32 = 0xA2;
pub const VK_RCTRL: u32 = 0xA3;
pub const VK_LALT: u32 = 0xA4;
pub const VK_RALT: u32 = 0xA5;

/// The command a key sends while Win is held, with its argument. Shell actions (Win+R, Win+I and
/// the like) go to explorust's own window as `Command::ShellAction`.
pub fn win_chord(vk: u32, ctrl: bool, shift: bool) -> Option<(Command, isize)> {
    let action = |a: ShellAction| (Command::ShellAction, a as isize);
    if ctrl {
        return Some(match vk {
            0x44 => (Command::DesktopNew, 0),     // Ctrl+Win+D
            VK_LEFT => (Command::DesktopSwitch, -1), // Ctrl+Win+Left
            VK_RIGHT => (Command::DesktopSwitch, 1), // Ctrl+Win+Right
            0x73 => (Command::DesktopClose, 0),   // Ctrl+Win+F4
            _ => return None,
        });
    }
    if shift {
        return match vk {
            0x53 => Some(action(ShellAction::ScreenClip)), // Win+Shift+S
            _ => None,
        };
    }
    Some(match vk {
        0x53 => (Command::Search, 0),             // S
        VK_TAB => (Command::TaskView, 0),         // Tab
        0x4E => (Command::NotificationCenter, 0), // N
        // Win+A stays with Windows' own quick settings host until the notify part answers it.
        0x58 => (Command::WinX, 0),                             // X
        0x31..=0x39 => (Command::TaskbarNth, (vk - 0x30) as isize), // 1..9
        0x54 => (Command::TaskbarFocus, 0),                     // T
        0x42 => (Command::TrayFocus, 0),                        // B
        0x45 => (Command::FilesOpen, 0),                        // E
        0x44 | 0x4D => (Command::ShowDesktop, 0),               // D, M
        0x5A => (Command::SnapLayouts, 0),                      // Z
        0x56 => (Command::ClipboardHistory, 0),                 // V
        0xBE | 0xBA => (Command::EmojiPanel, 0),                // . and ;
        0x52 => action(ShellAction::RunDialog),                 // R
        0x49 => action(ShellAction::Settings),                  // I
        0x55 => action(ShellAction::EaseOfAccess),              // U
        0x50 => action(ShellAction::Projection),                // P
        0x4B => action(ShellAction::Connect),                   // K
        0x13 => action(ShellAction::About),                     // Pause
        _ => return None,
    })
}

/// What a key does while Alt+Tab is open.
pub fn switcher_key(vk: u32) -> Option<(Command, isize)> {
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

/// Ctrl+Alt+Shift+F12: the guard's escape to plain explorer.
pub fn is_escape(vk: u32, ctrl: bool, alt: bool, shift: bool) -> bool {
    vk == VK_F12 && ctrl && alt && shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_actions_go_to_the_shell() {
        assert_eq!(win_chord(0x52, false, false), Some((Command::ShellAction, ShellAction::RunDialog as isize)));
        assert_eq!(win_chord(0x49, false, false), Some((Command::ShellAction, ShellAction::Settings as isize)));
        assert_eq!(win_chord(0x53, false, true), Some((Command::ShellAction, ShellAction::ScreenClip as isize)));
        for vk in 0..=0xFF {
            for (ctrl, shift) in [(false, false), (true, false), (false, true)] {
                if let Some((Command::ShellAction, a)) = win_chord(vk, ctrl, shift) {
                    assert!(ShellAction::from_isize(a).is_some(), "Win+{vk:#x} sends an unknown action {a}");
                }
            }
        }
    }

    #[test]
    fn chords() {
        assert_eq!(win_chord(0x53, false, false), Some((Command::Search, 0)));
        assert_eq!(win_chord(0x45, false, false), Some((Command::FilesOpen, 0)));
        assert_eq!(win_chord(0x35, false, false), Some((Command::TaskbarNth, 5)));
        assert_eq!(win_chord(VK_LEFT, true, false), Some((Command::DesktopSwitch, -1)));
        assert_eq!(win_chord(0x41, false, false), None, "Win+A stays with Windows");
        assert_eq!(win_chord(0x45, false, true), None);
        assert_eq!(switcher_key(VK_DELETE), Some((Command::SwitcherClose, 0)));
        assert_eq!(switcher_key(0x41), None);
    }

    #[test]
    fn escape_needs_all_three() {
        assert!(is_escape(VK_F12, true, true, true));
        assert!(!is_escape(VK_F12, true, true, false));
        assert!(!is_escape(0x7A, true, true, true));
    }
}
