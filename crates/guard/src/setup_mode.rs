//! Decides which confirmation to offer from the install and session probes.

use std::time::Duration;

const RESTART_LIMIT: Duration = Duration::from_secs(5 * 60);
const MISSING_TRAY_LIMIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartProgress {
    OldShell,
    Starting,
    DrainCancelled,
    Verified(u32),
    Windows(u32),
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Restart {
    Updated(u32),
    Windows(u32),
    Deferred,
    Postponed,
    Unconfirmed,
}

pub fn restart_outcome(progress: RestartProgress, elapsed: Duration, without_tray: Duration) -> Option<Restart> {
    match progress {
        RestartProgress::Windows(pid) => Some(Restart::Windows(pid)),
        RestartProgress::Verified(pid) => Some(Restart::Updated(pid)),
        RestartProgress::DrainCancelled => Some(Restart::Postponed),
        RestartProgress::OldShell if elapsed >= RESTART_LIMIT => Some(Restart::Deferred),
        _ if elapsed >= RESTART_LIMIT => Some(Restart::Unconfirmed),
        RestartProgress::Missing if without_tray >= MISSING_TRAY_LIMIT => Some(Restart::Unconfirmed),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    UpdateOnSignIn,
    UpdateLive { pid: u32, preserves_files: bool },
}

pub fn choose(installed: bool, shell_pid: Option<u32>, reload: bool, guarded: bool, preserves_files: bool) -> Mode {
    match (installed, shell_pid, reload, guarded) {
        (true, Some(pid), true, true) => Mode::UpdateLive { pid, preserves_files },
        (true, _, _, _) => Mode::UpdateOnSignIn,
        (false, _, _, _) => Mode::Install,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_drain_reports_postponement_immediately() {
        assert_eq!(restart_outcome(RestartProgress::DrainCancelled, Duration::from_secs(120), Duration::ZERO), Some(Restart::Postponed));
    }

    #[test]
    fn a_file_drain_cannot_keep_the_installer_open_indefinitely() {
        assert_eq!(restart_outcome(RestartProgress::OldShell, RESTART_LIMIT - Duration::from_secs(1), Duration::ZERO), None);
        assert_eq!(restart_outcome(RestartProgress::OldShell, RESTART_LIMIT, Duration::ZERO), Some(Restart::Deferred));
    }

    #[test]
    fn the_new_taskbar_is_not_enough_until_the_build_verdict() {
        let elapsed = Duration::from_secs(70);
        assert_eq!(restart_outcome(RestartProgress::Starting, elapsed, Duration::ZERO), None);
        assert_eq!(restart_outcome(RestartProgress::Verified(42), elapsed, Duration::ZERO), Some(Restart::Updated(42)));
        assert_eq!(restart_outcome(RestartProgress::Starting, RESTART_LIMIT, Duration::ZERO), Some(Restart::Unconfirmed));
    }

    #[test]
    fn a_missing_tray_times_out_and_a_confirmed_takeover_is_reported() {
        assert_eq!(restart_outcome(RestartProgress::Missing, MISSING_TRAY_LIMIT, MISSING_TRAY_LIMIT), Some(Restart::Unconfirmed));
        assert_eq!(restart_outcome(RestartProgress::Windows(42), MISSING_TRAY_LIMIT, MISSING_TRAY_LIMIT), Some(Restart::Windows(42)));
    }

    #[test]
    fn only_an_installed_guarded_explorust_with_a_reload_event_updates_live() {
        assert_eq!(choose(false, Some(42), true, true, true), Mode::Install);
        assert_eq!(choose(true, None, true, true, true), Mode::UpdateOnSignIn);
        assert_eq!(choose(true, Some(42), false, true, true), Mode::UpdateOnSignIn);
        assert_eq!(choose(true, Some(42), true, false, true), Mode::UpdateOnSignIn);
        for preserves_files in [false, true] {
            assert_eq!(choose(true, Some(42), true, true, preserves_files), Mode::UpdateLive { pid: 42, preserves_files });
        }
    }
}
