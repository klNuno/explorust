//! The command bus between explorust's parts: the hotkey hook (and anything else) posts a
//! `Command` to the window class of the component that owns it. It works the same inside the
//! explorust process and across processes, so a component's dev exe in the guest gets the real
//! hotkeys of the running shell.
//!
//! Keys commands use a named pipe checked against the shell and helper processes. Other
//! components create a message window with `class_of(owner)` as class name and call
//! `listen(hwnd)` once; its window procedure turns `MSG_COMMAND` (wParam = `Command as u32`,
//! lParam = an optional argument) into the action.

/// The message every component window accepts: `WM_APP + 0x100`.
pub const MSG_COMMAND: u32 = 0x8000 + 0x100;

#[cfg(any(target_os = "windows", test))]
pub mod keys_pipe;

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
    /// Win+R, Win+I and the other one-line shell actions (lParam = `ShellAction as isize`). They run
    /// in explorust itself, so what they start gets its plain token, never the hook exe's UI access.
    ShellAction = 24,
    /// Bring a window to the foreground (lParam = its HWND). Answered by explorust-keys.exe, whose UI
    /// access lets it activate any window, a UWP frame included, over an elevated foreground window.
    Activate = 25,
    /// Minimize a window without activating it (lParam = its HWND), for Win+D over a window
    /// of a process above explorust's integrity. Answered by explorust-keys.exe (UI access).
    Minimize = 26,
    /// Restore a window without activating it and raise it to the top (lParam = its HWND), the
    /// second Win+D press for the same windows.
    Restore = 27,
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
            24 => ShellAction,
            25 => Activate,
            26 => Minimize,
            27 => Restore,
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
            ShellAction => Owner::Shell,
            Activate | Minimize | Restore => Owner::Keys,
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
    /// explorust's own command window, which runs the shell actions.
    Shell,
    /// The keyboard hook host (explorust-keys.exe, or explorust when the hook runs in-process).
    Keys,
}

impl Owner {
    /// The parts (the shell's own window is not one).
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
            Owner::Shell => "shell",
            Owner::Keys => "keys",
        }
    }
}

/// The argument of `Command::ShellAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub enum ShellAction {
    /// Win+R: Windows' own Run dialog.
    RunDialog = 0,
    /// Win+I.
    Settings = 1,
    /// Win+U.
    EaseOfAccess = 2,
    /// Win+P.
    Projection = 3,
    /// Win+K.
    Connect = 4,
    /// Win+Pause.
    About = 5,
    /// Win+Shift+S.
    ScreenClip = 6,
}

impl ShellAction {
    pub fn from_isize(v: isize) -> Option<ShellAction> {
        use ShellAction::*;
        Some(match v {
            0 => RunDialog,
            1 => Settings,
            2 => EaseOfAccess,
            3 => Projection,
            4 => Connect,
            5 => About,
            6 => ScreenClip,
            _ => return None,
        })
    }

    /// The URI handed to ShellExecute; None for the Run dialog.
    pub fn uri(self) -> Option<&'static str> {
        use ShellAction::*;
        Some(match self {
            RunDialog => return None,
            Settings => "ms-settings:",
            EaseOfAccess => "ms-settings:easeofaccess",
            Projection => "ms-settings-displays-topology:projection",
            Connect => "ms-settings-connectabledevices:devicediscovery",
            About => "ms-settings:about",
            ScreenClip => "ms-screenclip:",
        })
    }
}

/// Window property a part sets on its visible window (`Shell_TrayWnd`) once its first frame is
/// drawn. An auto-hidden taskbar starts off screen and hidden, so visibility cannot tell the shell
/// that the bar is up.
pub const PAINTED_PROP: &str = "explorust.painted";

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
        if owner == Owner::Keys {
            return keys_pipe::encode(cmd, arg).is_some_and(|record| keys_pipe::post(&record));
        }
        if cmd.owner() == Owner::Keys {
            return false;
        }
        let Some(h) = target(owner) else { return false };
        // SAFETY: posting to a window found above; a stale handle only makes the call fail.
        unsafe { PostMessageW(Some(h), MSG_COMMAND, WPARAM(cmd as usize), LPARAM(arg)) }.is_ok()
    }

    /// Lets a lower-integrity process (the shell, when a dev exe runs elevated) post commands here.
    pub fn listen(hwnd: HWND) {
        // SAFETY: changes the message filter of a window this thread owns.
        let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, MSG_COMMAND, MSGFLT_ALLOW, None) };
    }

    /// The mandatory integrity level of a process token (its label's last sub-authority).
    fn integrity(process: windows::Win32::Foundation::HANDLE) -> Option<u32> {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
        };
        use windows::Win32::System::Threading::OpenProcessToken;
        // SAFETY: queries a token into a local buffer and closes it; the label's SID points into it.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
            let mut buf = [0u64; 16];
            let mut len = 0u32;
            let ok = GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), 128, &mut len).is_ok();
            let _ = CloseHandle(token);
            if !ok {
                return None;
            }
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let sid = label.Label.Sid;
            let n = *GetSidSubAuthorityCount(sid);
            Some(*GetSidSubAuthority(sid, n.saturating_sub(1) as u32))
        }
    }

    /// True when `h` belongs to a process above this one's integrity level, or one whose level
    /// cannot be read: Windows (UIPI) drops what this process sends it, so only the keys host,
    /// which has UI access, can move it.
    pub fn above_me(h: HWND) -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
        let mut pid = 0u32;
        // SAFETY: plain queries; the process handle is closed.
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return false;
            }
            let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
            let theirs = integrity(p);
            let _ = CloseHandle(p);
            match (theirs, integrity(GetCurrentProcess())) {
                (Some(t), Some(m)) => t > m,
                _ => true,
            }
        }
    }

    /// Decodes a `MSG_COMMAND` message: `(command, argument)`.
    pub fn decode(wparam: WPARAM, lparam: LPARAM) -> Option<(Command, isize)> {
        Command::from_u32(wparam.0 as u32).map(|c| (c, lparam.0))
    }
}

/// twinui's shell hook window inside explorust (published by uwp-host once it registered it, 0 when
/// none). Under explorer, Windows sends the taskman window (the taskbar) codes it sends no other
/// shell hook window (0x24 and 0x26 presentation readiness among them) and explorer hands them on
/// to twinui; explorust's taskbar does the same through this slot.
pub mod shell_hook_sink {
    use std::sync::atomic::{AtomicIsize, Ordering};
    static SINK: AtomicIsize = AtomicIsize::new(0);
    pub fn set(hwnd: isize) {
        SINK.store(hwnd, Ordering::Release);
    }
    pub fn get() -> Option<isize> {
        Some(SINK.load(Ordering::Acquire)).filter(|h| *h != 0)
    }
    /// Codes Windows already broadcasts to every shell hook window (seen on twinui's window under
    /// explorust, 2026-11-20): they must not reach twinui twice. GETMINRECT (5) is a sent message
    /// with a pointer, never posted on. Handing on only 0x24 and 0x26 is not enough: the center
    /// then never shows (g1, 2026-11-20).
    pub fn taskman_only(code: usize) -> bool {
        !matches!(code, 0x1 | 0x2 | 0x3 | 0x4 | 0x5 | 0x6 | 0x10 | 0x35 | 0x8004 | 0x8006)
    }
}

#[cfg(target_os = "windows")]
pub use win::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_round_trips() {
        for v in 1..=27 {
            let c = Command::from_u32(v).unwrap();
            assert_eq!(c as u32, v);
        }
        assert!(Command::from_u32(0).is_none());
        assert!(Command::from_u32(28).is_none());
        assert_eq!(Command::Activate.owner(), Owner::Keys);
        assert_eq!(Command::Minimize.owner(), Owner::Keys);
        assert_eq!(Command::Restore.owner(), Owner::Keys);
        assert_eq!(class_of(Owner::Keys), "explorust.keys");
    }

    #[test]
    fn shell_actions_round_trip() {
        for v in 0..=6 {
            assert_eq!(ShellAction::from_isize(v).unwrap() as isize, v);
        }
        assert!(ShellAction::from_isize(7).is_none());
        assert_eq!(ShellAction::RunDialog.uri(), None);
        assert_eq!(Command::ShellAction.owner(), Owner::Shell);
        assert_eq!(class_of(Owner::Shell), "explorust.shell");
    }
}
