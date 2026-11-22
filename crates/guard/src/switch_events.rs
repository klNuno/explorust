//! Names shared by the session supervisor, setup and the standalone switch.

pub const SWITCH_EXE: &str = "explorust-switch.exe";
pub const GUARD_MUTEX: &str = r"Local\explorust-guard";
pub const RELEASE_EXPLORER: &str = r"Local\explorust-guard-release-explorer";
pub const WATCHES_EXPLORER_HANDOFF: &str = r"Local\explorust-guard-watches-explorer-handoff";
pub const EXITING_EXPLORER: &str = r"Local\explorust-switch-exiting-explorer";
pub const CANCEL_FOR_RESTORE: &str = r"Local\explorust-cancel-switch-for-restore";

pub fn switch_event(stamp: u64, name: &str) -> String {
    format!(r"Local\explorust-switch-{stamp}-{name}")
}
