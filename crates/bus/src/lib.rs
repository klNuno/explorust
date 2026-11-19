//! The command bus between explorust's parts: the hotkey hook (and anything else) posts a
//! `Command` to the window class of the component that owns it. It works the same inside the
//! explorust process and across processes, so a component's dev exe in the guest gets the real
//! hotkeys of the running shell.
//!
//! A component creates its message window with `class_of(owner)` as class name and calls
//! `listen(hwnd)` once; its window procedure turns `MSG_COMMAND` (wParam = `Command as u32`,
//! lParam = an optional argument) into the action.

/// The message every component window accepts: `WM_APP + 0x100`.
pub const MSG_COMMAND: u32 = 0x8000 + 0x100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Command {
    /// Win tapped alone, or a click on the Start button.
    StartToggle = 1,
    /// Win+S: Start opened on its search box.
    Search = 2,
    /// Win+Tab.
    TaskView = 3,
    /// Win+N.
    NotificationCenter = 4,
    /// Win+A (only when explorust owns quick settings; the UWP host answers it otherwise).
    QuickSettings = 5,
    /// Win+X.
    WinX = 6,
    /// Alt+Tab pressed or Tab pressed again while Alt is held (lParam 1 when Shift is held).
    SwitcherNext = 7,
    /// Alt released: switch to the selected window.
    SwitcherCommit = 8,
    /// Esc while the switcher is open.
    SwitcherCancel = 9,
    /// Win+1..9 (lParam = 1..9): start or switch to the nth taskbar button.
    TaskbarNth = 10,
    /// Win+T: focus the taskbar buttons.
    TaskbarFocus = 11,
    /// Win+B: focus the tray.
    TrayFocus = 12,
    /// Win+E: a new File Explorer window (lParam 0) or a folder handed over by the shell.
    FilesOpen = 13,
    /// Win+D: show the desktop, or restore what it hid.
    ShowDesktop = 14,
    /// Ask a component to close its windows and end its thread (reload, quit).
    Shutdown = 15,
    /// Ctrl+Win+D.
    DesktopNew = 16,
    /// Ctrl+Win+Left (lParam -1) and Ctrl+Win+Right (lParam 1).
    DesktopSwitch = 17,
    /// Ctrl+Win+F4.
    DesktopClose = 18,
    /// Win+Z: snap layouts for the foreground window.
    SnapLayouts = 19,
    /// Win+V.
    ClipboardHistory = 20,
    /// Win+. and Win+;.
    EmojiPanel = 21,
    /// An arrow key while the switcher is open: lParam 0 left, 1 right, 2 up, 3 down.
    SwitcherMove = 22,
    /// Delete while the switcher is open: close the selected window.
    SwitcherClose = 23,
}

impl Command {
    pub fn from_u32(v: u32) -> Option<Command> {
        use Command::*;
        Some(match v {
            1 => StartToggle,
            2 => Search,
            3 => TaskView,
            4 => NotificationCenter,
            5 => QuickSettings,
            6 => WinX,
            7 => SwitcherNext,
            8 => SwitcherCommit,
            9 => SwitcherCancel,
            10 => TaskbarNth,
            11 => TaskbarFocus,
            12 => TrayFocus,
            13 => FilesOpen,
            14 => ShowDesktop,
            15 => Shutdown,
            16 => DesktopNew,
            17 => DesktopSwitch,
            18 => DesktopClose,
            19 => SnapLayouts,
            20 => ClipboardHistory,
            21 => EmojiPanel,
            22 => SwitcherMove,
            23 => SwitcherClose,
            _ => return None,
        })
    }

    /// The component that answers this command.
    pub fn owner(self) -> Owner {
        use Command::*;
        match self {
            StartToggle | Search => Owner::Start,
            TaskView | SwitcherNext | SwitcherCommit | SwitcherCancel | SwitcherMove | SwitcherClose
            | DesktopNew | DesktopSwitch | DesktopClose | SnapLayouts => Owner::Switcher,
            NotificationCenter | QuickSettings | ClipboardHistory | EmojiPanel => Owner::Notify,
            WinX | TaskbarNth | TaskbarFocus | TrayFocus => Owner::Taskbar,
            FilesOpen => Owner::Files,
            ShowDesktop => Owner::Desktop,
            // Sent to each owner with `post_to`.
            Shutdown => Owner::Taskbar,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Taskbar,
    Desktop,
    Start,
    Switcher,
    Notify,
    Files,
}

impl Owner {
    pub const ALL: [Owner; 6] =
        [Owner::Taskbar, Owner::Desktop, Owner::Start, Owner::Switcher, Owner::Notify, Owner::Files];

    pub fn name(self) -> &'static str {
        match self {
            Owner::Taskbar => "taskbar",
            Owner::Desktop => "desktop",
            Owner::Start => "start",
            Owner::Switcher => "switcher",
            Owner::Notify => "notify",
            Owner::Files => "files",
        }
    }
}

/// The class name of an owner's command window: `explorust.<name>`, a message-only or hidden window.
/// The taskbar also owns `Shell_TrayWnd`; that one stays Windows' name for tray icons and AppBars.
pub fn class_of(owner: Owner) -> String {
    format!("explorust.{}", owner.name())
}

#[cfg(target_os = "windows")]
mod win {
    use super::*;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, FindWindowExW, HWND_MESSAGE, MSGFLT_ALLOW, PostMessageW,
    };
    use windows::core::HSTRING;

    /// The command window of `owner`, top-level or message-only, in any process of the session.
    pub fn target(owner: Owner) -> Option<HWND> {
        let class = HSTRING::from(class_of(owner));
        // SAFETY: plain window lookups by class name.
        unsafe {
            FindWindowExW(Some(HWND_MESSAGE), None, &class, None)
                .or_else(|_| FindWindowExW(None, None, &class, None))
                .ok()
                .filter(|h| !h.is_invalid())
        }
    }

    /// Posts `cmd` to its owner. False when no component answers it (the key then passes through).
    pub fn post(cmd: Command, arg: isize) -> bool {
        post_to(cmd.owner(), cmd, arg)
    }

    /// Posts `cmd` to one owner, whoever answers it by default (used for `Shutdown`).
    pub fn post_to(owner: Owner, cmd: Command, arg: isize) -> bool {
        let Some(h) = target(owner) else { return false };
        // SAFETY: posting to a window found above; a stale handle only makes the call fail.
        unsafe { PostMessageW(Some(h), MSG_COMMAND, WPARAM(cmd as usize), LPARAM(arg)) }.is_ok()
    }

    /// Lets a lower-integrity process (the shell, when a dev exe runs elevated) post commands here.
    pub fn listen(hwnd: HWND) {
        // SAFETY: changes the message filter of a window this thread owns.
        let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, MSG_COMMAND, MSGFLT_ALLOW, None) };
    }

    /// Decodes a `MSG_COMMAND` message: `(command, argument)`.
    pub fn decode(wparam: WPARAM, lparam: LPARAM) -> Option<(Command, isize)> {
        Command::from_u32(wparam.0 as u32).map(|c| (c, lparam.0))
    }
}

#[cfg(target_os = "windows")]
pub use win::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_round_trips() {
        for v in 1..=23 {
            let c = Command::from_u32(v).unwrap();
            assert_eq!(c as u32, v);
        }
        assert!(Command::from_u32(0).is_none());
        assert!(Command::from_u32(24).is_none());
    }
}
