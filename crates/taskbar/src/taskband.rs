//! What apps tell the taskbar through `ITaskbarList`..`ITaskbarList4` (`CLSID_TaskbarList`).
//!
//! The object runs in the app's process (explorerframe.dll, `CTaskbarList`). Each call finds
//! `FindWindowW("Shell_TrayWnd")`, reads its window property `TaskbandHWND`, caches that window
//! (re-checked with `IsWindow`) and sends it one message. Read from explorerframe.dll of 26300
//! (`.agents/results/taskbar-4.md` gives the addresses):
//!
//! | method | message | wParam | lParam |
//! |---|---|---|---|
//! | AddTab, DeleteTab | `SHELLHOOK` (sent) | 1 or 2 (HSHELL_WINDOWCREATED or DESTROYED) | hwnd |
//! | ActivateTab | `SHELLHOOK` 4, then 0x432 | 0 | hwnd |
//! | SetActiveAlt | 0x432 (sent) | 0 | hwnd |
//! | MarkFullscreenWindow | 0x43C (sent) | fullscreen | hwnd |
//! | SetProgressValue | 0x440 (posted) | hwnd | 0..=0xFFFE |
//! | SetProgressState | 0x441 (posted) | hwnd | TBPFLAG |
//! | RegisterTab, UnregisterTab | 0x443, 0x444 (posted) | tab | MDI frame |
//! | SetTabOrder, SetTabActive | 0x447, 0x448 (posted) | tab | insert before |
//! | ThumbBarAddButtons, ThumbBarUpdateButtons | 0x44C, 0x44D (sent, 3 s) | hwnd | shared block |
//! | ThumbBarSetImageList | 0x44E (sent, 3 s) | hwnd | shared block |
//! | SetOverlayIcon | 0x44F (sent, 3 s) | hwnd | HICON (a copy the app destroys after) |
//! | SetThumbnailTooltip | 0x450 (sent, 3 s) | hwnd | shared string |
//! | SetThumbnailClip | 0x451 (sent, 3 s) | hwnd | shared RECT |
//! | overlay description | 0x455 (sent, 3 s) | hwnd | shared string |
//! | SetTabProperties | 0x457 (posted) | hwnd | STPFLAG |
//!
//! "Shared" is an `SHAllocShared` handle made for the taskband's process, read with
//! `SHLockShared`; the app frees it once the message returns.

pub const MSG_ACTIVE_ALT: u32 = 0x432;
pub const MSG_FULLSCREEN: u32 = 0x43C;
pub const MSG_PROGRESS_VALUE: u32 = 0x440;
pub const MSG_PROGRESS_STATE: u32 = 0x441;
pub const MSG_REGISTER_TAB: u32 = 0x443;
pub const MSG_UNREGISTER_TAB: u32 = 0x444;
pub const MSG_TAB_ORDER: u32 = 0x447;
pub const MSG_TAB_ACTIVE: u32 = 0x448;
pub const MSG_THUMB_ADD: u32 = 0x44C;
pub const MSG_THUMB_UPDATE: u32 = 0x44D;
pub const MSG_THUMB_IMAGES: u32 = 0x44E;
pub const MSG_OVERLAY: u32 = 0x44F;
pub const MSG_TOOLTIP: u32 = 0x450;
pub const MSG_CLIP: u32 = 0x451;
pub const MSG_OVERLAY_TEXT: u32 = 0x455;
pub const MSG_TAB_PROPERTIES: u32 = 0x457;

/// Every message above, for the UIPI filter of the taskband window.
pub const ALL: [u32; 16] = [
    MSG_ACTIVE_ALT, MSG_FULLSCREEN, MSG_PROGRESS_VALUE, MSG_PROGRESS_STATE, MSG_REGISTER_TAB, MSG_UNREGISTER_TAB,
    MSG_TAB_ORDER, MSG_TAB_ACTIVE, MSG_THUMB_ADD, MSG_THUMB_UPDATE, MSG_THUMB_IMAGES, MSG_OVERLAY, MSG_TOOLTIP,
    MSG_CLIP, MSG_OVERLAY_TEXT, MSG_TAB_PROPERTIES,
];

/// The scale of SetProgressValue's lParam: `completed * 0xFFFC / total + 1`, 0xFFFE when done.
pub const PROGRESS_MAX: u32 = 0xFFFE;

/// TBPFLAG of SetProgressState.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProgressState {
    #[default]
    None,
    Indeterminate,
    Normal,
    Error,
    Paused,
}

impl ProgressState {
    pub fn from_flags(f: u32) -> ProgressState {
        // Windows takes the highest bit set: TBPF_PAUSED 8, TBPF_ERROR 4, TBPF_NORMAL 2, TBPF_INDETERMINATE 1.
        if f & 8 != 0 {
            ProgressState::Paused
        } else if f & 4 != 0 {
            ProgressState::Error
        } else if f & 2 != 0 {
            ProgressState::Normal
        } else if f & 1 != 0 {
            ProgressState::Indeterminate
        } else {
            ProgressState::None
        }
    }
}

/// One window's progress as the taskband keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Progress {
    pub state: ProgressState,
    /// 0..=1.
    pub value: f32,
}

impl Progress {
    /// SetProgressValue: a value without a state shows as Normal, like Windows.
    pub fn set_value(&mut self, raw: u32) {
        self.value = raw.min(PROGRESS_MAX) as f32 / PROGRESS_MAX as f32;
        if self.state == ProgressState::None || self.state == ProgressState::Indeterminate {
            self.state = ProgressState::Normal;
        }
    }

    pub fn set_state(&mut self, flags: u32) {
        self.state = ProgressState::from_flags(flags);
        if self.state == ProgressState::None {
            self.value = 0.0;
        }
    }

    pub fn shown(&self) -> bool {
        self.state != ProgressState::None
    }
}

/// The 16-bit string of a shared block (`SHAllocShared` of `wcslen * 2 + 2` bytes).
pub fn wide_string(units: &[u16]) -> String {
    let n = units.iter().position(|c| *c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..n])
}

/// The value the app's call maps to, for tests and logs: `completed / total` as SetProgressValue sends it.
pub fn raw_value(completed: u64, total: u64) -> u32 {
    if completed == 0 || total == 0 {
        0
    } else if completed >= total {
        PROGRESS_MAX
    } else {
        ((completed * 0xFFFC / total) + 1).min(u32::MAX as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_scale() {
        assert_eq!(raw_value(0, 100), 0);
        assert_eq!(raw_value(100, 100), PROGRESS_MAX);
        assert_eq!(raw_value(50, 100), 0x7FFF);
        let mut p = Progress::default();
        p.set_value(raw_value(50, 100));
        assert_eq!(p.state, ProgressState::Normal);
        assert!((p.value - 0.5).abs() < 0.001);
    }

    #[test]
    fn states() {
        assert_eq!(ProgressState::from_flags(0), ProgressState::None);
        assert_eq!(ProgressState::from_flags(1), ProgressState::Indeterminate);
        assert_eq!(ProgressState::from_flags(4), ProgressState::Error);
        assert_eq!(ProgressState::from_flags(8), ProgressState::Paused);
        let mut p = Progress::default();
        p.set_state(4);
        p.set_value(100);
        assert_eq!(p.state, ProgressState::Error);
        p.set_state(0);
        assert!(!p.shown());
        assert_eq!(p.value, 0.0);
    }

    #[test]
    fn strings() {
        assert_eq!(wide_string(&[0x48, 0x69, 0, 0x41]), "Hi");
        assert_eq!(wide_string(&[0x48]), "H");
    }
}
