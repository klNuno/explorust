//! When to give up on explorust and start explorer.exe. No Windows calls here, so the rules are
//! unit-tested on any host: `rustc --test crates/guard/src/policy.rs`.
//!
//! The rule since 2026-11-21: at the first problem explorust has (an exit before the ready
//! signal, a ready timeout, a crash, a hung taskbar, a part the shell gave up on) the guard starts
//! explorer.exe for the rest of the session and tells the user once. A problem at three logons in
//! a row turns explorust off for good (the per-user Shell value is deleted), so a Windows update
//! that breaks it never makes every logon a fallback. A logon counts by its outcome: the streak
//! goes back to zero only at the logon after one that ended without a problem, never after some
//! healthy minutes, so a part that fails ten minutes into every session still ends the streak.

use std::time::Duration;

/// Logons in a row that ended in a problem before explorust is turned off.
pub const MAX_STREAK: u32 = 3;

/// What a logon does given the streak of problem logons before it.
#[derive(Debug, PartialEq, Eq)]
pub enum AtLogon {
    Run,
    TurnOff,
}

pub fn at_logon(streak: u32) -> AtLogon {
    if streak >= MAX_STREAK { AtLogon::TurnOff } else { AtLogon::Run }
}

/// The taskbar check: one probe every `PROBE`, each waiting `ANSWER_WITHIN` for an answer (the
/// time after which Windows itself calls a window hung). A problem needs at least `MIN_MISSES`
/// failed probes in a row and, from the start of the first, `HUNG_FOR` of silence (any probe
/// without an answer) or `MISSING_FOR` with the window gone, so a busy machine that answers late
/// once or twice is never a problem.
pub const PROBE: Duration = Duration::from_secs(5);
pub const ANSWER_WITHIN: Duration = Duration::from_secs(5);
pub const HUNG_FOR: Duration = Duration::from_secs(20);
pub const MISSING_FOR: Duration = Duration::from_secs(30);
pub const MIN_MISSES: u32 = 3;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Probe {
    Answered,
    NoAnswer,
    Missing,
}

/// Failed probes since the last answer: when the first started, how many, and whether one of them
/// found the window there but silent. A window that goes from silent to gone stays one outage.
#[derive(Default)]
pub struct Watchdog {
    since: Option<Duration>,
    misses: u32,
    hung: bool,
}

impl Watchdog {
    /// `at`: when this probe started, on any clock that only goes forward.
    pub fn probe(&mut self, p: Probe, at: Duration) -> Option<String> {
        if p == Probe::Answered {
            *self = Self::default();
            return None;
        }
        let since = *self.since.get_or_insert(at);
        self.misses += 1;
        self.hung |= p == Probe::NoAnswer;
        let limit = if self.hung { HUNG_FOR } else { MISSING_FOR };
        let out = at.saturating_sub(since);
        if self.misses < MIN_MISSES || out < limit {
            return None;
        }
        Some(if self.hung {
            format!("the taskbar has not answered for {} s ({} checks)", out.as_secs(), self.misses)
        } else {
            format!("the taskbar window has been gone for {} s ({} checks)", out.as_secs(), self.misses)
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExitState {
    #[default]
    Unsent,
    Uncertain,
    Posted,
}

impl ExitState {
    pub fn from_signals(intent: bool, posted: bool, cancelled: bool) -> Self {
        if posted {
            Self::Posted
        } else if intent && !cancelled {
            Self::Uncertain
        } else {
            Self::Unsent
        }
    }

    pub fn pending(self) -> bool {
        self != Self::Unsent
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OriginalTray {
    #[default]
    Missing,
    Present,
    Answering(usize),
}

#[derive(Clone, Copy, Default)]
pub struct DesktopFacts {
    pub exit: ExitState,
    pub original_alive: bool,
    pub original_tray: OriginalTray,
    pub tray_gone_grace_elapsed: bool,
    pub file_operations: bool,
    pub explorer_ready: Option<u32>,
    pub explorer_owner: Option<u32>,
    pub explorust_ready: Option<u32>,
    pub recovery_expired: bool,
    pub retirement_attempts: u8,
    pub supervisor_available: bool,
    pub explorust_owner: Option<u32>,
    pub adoption_elapsed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DesktopAction {
    WaitForCopy,
    NeedsSignOut,
    WaitForOriginal,
    RepostExit(usize),
    StopRetiringExplorer,
    Ready { pid: u32, retiring: bool },
    RequestFallback,
    WaitForAdoption,
    StopOrphan(u32),
    Restore,
}

/// Decide from this observation only. Each caller probes again after waiting or taking action.
pub fn recover_desktop(facts: DesktopFacts) -> DesktopAction {
    let retiring = facts.exit.pending() && facts.original_alive;
    if !facts.file_operations && (!retiring || facts.original_tray == OriginalTray::Missing)
        && let Some(pid) = facts.explorer_ready.or(facts.explorust_ready)
    {
        return DesktopAction::Ready { pid, retiring };
    }
    if facts.recovery_expired || (retiring && facts.explorer_owner.is_none()
        && facts.tray_gone_grace_elapsed && facts.retirement_attempts >= RETIRING_EXPLORER_ATTEMPTS)
    {
        return DesktopAction::NeedsSignOut;
    }
    if facts.file_operations {
        return DesktopAction::WaitForCopy;
    }
    if retiring && facts.original_tray != OriginalTray::Missing {
        // Sent probes can be answered ahead of queued exit requests. Resolve uncertainty by
        // posting to the current tray of that PID, which can have recreated its window.
        if let OriginalTray::Answering(window) = facts.original_tray
            && facts.exit == ExitState::Uncertain
        {
            return DesktopAction::RepostExit(window);
        }
        return DesktopAction::WaitForOriginal;
    }
    if retiring {
        if facts.explorer_owner.is_some() {
            // A different verified Explorer owns the desktop. Recovery can retry that owner.
            return DesktopAction::Restore;
        }
        // Explorer can still route a new launch to a folder while the old shell process owns
        // the desktop. Wait for it to release ownership, even after its tray disappears.
        return if facts.tray_gone_grace_elapsed {
            DesktopAction::StopRetiringExplorer
        } else {
            DesktopAction::WaitForOriginal
        };
    }
    if facts.explorer_owner.is_some() {
        return DesktopAction::Restore;
    }
    if facts.supervisor_available {
        return DesktopAction::RequestFallback;
    }
    if let Some(pid) = facts.explorust_owner {
        return if facts.adoption_elapsed {
            DesktopAction::StopOrphan(pid)
        } else {
            DesktopAction::WaitForAdoption
        };
    }
    DesktopAction::Restore
}

pub const DESKTOP_RECOVERY_LIMIT: Duration = Duration::from_secs(155);
pub const DESKTOP_OBSERVE_LIMIT: Duration = Duration::from_secs(600);
pub const RETIRING_EXPLORER_ATTEMPTS: u8 = 3;

/// A healthy Explorer exits within a few seconds of destroying its taskbar. One that crashed on
/// the way out (a shell extension or a mod injected into it) sits on its error box until someone
/// answers it, or forever when the error box is not shown.
pub const RETIRING_EXPLORER_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum RetiringExit {
    Wait,
    /// Its taskbar is gone: end the process and carry on with the switch.
    Stop,
    /// Hand the session back to recovery.
    GiveUp,
}

/// The live switch after Exit Explorer: `tray_missing_for` is how long the retiring Explorer has
/// had no taskbar, `None` while it still owns one.
pub fn retiring_exit(elapsed: Duration, tray_missing_for: Option<Duration>, file_operations: bool) -> RetiringExit {
    if elapsed >= DESKTOP_RECOVERY_LIMIT {
        RetiringExit::GiveUp
    } else if !file_operations && tray_missing_for.is_some_and(|missing| missing >= RETIRING_EXPLORER_GRACE) {
        RetiringExit::Stop
    } else {
        RetiringExit::Wait
    }
}

/// What a guard started by Winlogon finds. Winlogon starts the shell again whenever the process
/// it watched ends with an error code, also in the middle of a session (Explorer crashing while a
/// live switch retires it); only an empty session is a sign-in to take.
#[derive(Debug, PartialEq, Eq)]
pub enum ShellStart {
    SignIn,
    GuardRuns,
    ExplorerRuns(u32),
}

pub fn shell_start(guard_running: bool, explorer_taskbar: Option<u32>) -> ShellStart {
    match (guard_running, explorer_taskbar) {
        (true, _) => ShellStart::GuardRuns,
        (false, Some(pid)) => ShellStart::ExplorerRuns(pid),
        (false, None) => ShellStart::SignIn,
    }
}

/// After the UI wait, observe a pending exit or copy without holding the recovery lock.
pub fn observe_recovery(exit: ExitState, original_alive: bool, file_operations: bool) -> DesktopAction {
    if exit.pending() && original_alive {
        DesktopAction::WaitForOriginal
    } else if file_operations {
        DesktopAction::WaitForCopy
    } else {
        DesktopAction::Restore
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestoreLocks<T, E> {
    Acquired(T, T),
    Pending(E),
}

pub fn try_restore_locks<T, E>(
    switch: impl FnOnce(u32) -> Result<T, E>,
    recovery: impl FnOnce(u32) -> Result<T, E>,
) -> RestoreLocks<T, E> {
    match (|| Ok((switch(0)?, recovery(0)?)))() {
        Ok((switch, recovery)) => RestoreLocks::Acquired(switch, recovery),
        Err(error) => RestoreLocks::Pending(error),
    }
}

/// The timing decision for an Explorer launch when the desktop still needs recovery.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Wait,
    Start,
    Restart(u32),
    StopLauncher,
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryTray {
    Missing,
    Explorer(u32),
    Other,
}

const EXPLORER_ATTEMPTS: u8 = 3;
const EXPLORER_WINDOW: Duration = Duration::from_secs(45);
const EXPLORER_RELAUNCH_GRACE: Duration = Duration::from_secs(10);
#[derive(Default)]
pub struct ExplorerRecovery {
    attempts: u8,
    started: Duration,
    ended: Option<Duration>,
    owner: Option<(u32, Duration)>,
}

impl ExplorerRecovery {
    pub fn started(&mut self, now: Duration) {
        self.attempts += 1;
        self.started = now;
        self.ended = None;
    }

    pub fn step(
        &mut self,
        now: Duration,
        tray: RecoveryTray,
        blocked: bool,
        launcher_ended: bool,
    ) -> Recovery {
        self.owner = match tray {
            RecoveryTray::Explorer(pid) => Some(self.owner.filter(|&(owner, _)| owner == pid).unwrap_or((pid, now))),
            _ => None,
        };
        let expired = now - self.started >= EXPLORER_WINDOW;
        if self.attempts > 0 {
            // Explorer can hand the launch to another process before the tray appears.
            let handed_off = tray == RecoveryTray::Missing && launcher_ended
                && now - *self.ended.get_or_insert(now) >= EXPLORER_RELAUNCH_GRACE;
            if !expired && !handed_off {
                return Recovery::Wait;
            }
        }
        if tray == RecoveryTray::Other || blocked {
            return if expired { Recovery::Exhausted } else { Recovery::Wait };
        }
        if self.attempts > 0 && tray == RecoveryTray::Missing && !launcher_ended {
            return Recovery::StopLauncher;
        }
        if self.attempts == EXPLORER_ATTEMPTS {
            return Recovery::Exhausted;
        }
        if let RecoveryTray::Explorer(pid) = tray {
            if self.owner.is_some_and(|(_, since)| now - since >= EXPLORER_WINDOW) {
                self.owner = Some((pid, now));
                Recovery::Restart(pid)
            } else {
                Recovery::Wait
            }
        } else {
            Recovery::Start
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_crashing_on_exit_is_stopped_not_rolled_back() {
        let secs = Duration::from_secs;
        // His PC: the taskbar went 5.4 s after Exit Explorer was posted and the process sat on an
        // access violation box for 8.5 s more. The switch must keep waiting, then end it.
        assert_eq!(retiring_exit(secs(6), Some(Duration::from_millis(5_400)), false), RetiringExit::Wait);
        assert_eq!(retiring_exit(secs(9), Some(secs(9)), false), RetiringExit::Wait);
        assert_eq!(retiring_exit(secs(11), Some(RETIRING_EXPLORER_GRACE), false), RetiringExit::Stop);
        // A copy still running in it is never cut off; the budget bounds the wait.
        assert_eq!(retiring_exit(secs(60), Some(secs(59)), true), RetiringExit::Wait);
        assert_eq!(retiring_exit(DESKTOP_RECOVERY_LIMIT, Some(secs(150)), true), RetiringExit::GiveUp);
        assert_eq!(retiring_exit(DESKTOP_RECOVERY_LIMIT, None, false), RetiringExit::GiveUp);
        // A taskbar still owned is a slow exit, not a crash.
        assert_eq!(retiring_exit(secs(40), None, false), RetiringExit::Wait);
    }

    #[test]
    fn a_winlogon_restart_in_a_running_session_is_not_a_sign_in() {
        // After Explorer crashed out of a live switch, Winlogon started the guard again while the
        // restored Explorer owned the taskbar: explorust was refused and the logon counted a problem.
        assert_eq!(shell_start(false, Some(11604)), ShellStart::ExplorerRuns(11604));
        // The switch's own guard took the session first.
        assert_eq!(shell_start(true, None), ShellStart::GuardRuns);
        assert_eq!(shell_start(true, Some(7)), ShellStart::GuardRuns);
        assert_eq!(shell_start(false, None), ShellStart::SignIn);
    }

    #[test]
    fn a_pending_exit_keeps_the_observer_after_the_ui_deadline() {
        assert_eq!(observe_recovery(ExitState::Posted, true, false), DesktopAction::WaitForOriginal);
        assert_eq!(observe_recovery(ExitState::Posted, false, false), DesktopAction::Restore);
        assert_eq!(observe_recovery(ExitState::Uncertain, true, false), DesktopAction::WaitForOriginal);
        assert_eq!(observe_recovery(ExitState::Unsent, true, false), DesktopAction::Restore);
    }

    #[test]
    fn a_postponed_copy_checks_the_original_desktop_when_the_copy_ends() {
        assert_eq!(observe_recovery(ExitState::Unsent, true, true), DesktopAction::WaitForCopy);
        assert_eq!(observe_recovery(ExitState::Unsent, true, false), DesktopAction::Restore);
    }

    #[test]
    fn restore_defers_when_the_switch_is_cancelling() {
        let locks = try_restore_locks(
            |ms| { assert_eq!(ms, 0); Err::<(), _>("switch cancelling") },
            |_| panic!("must give control back without waiting for recovery"),
        );
        assert_eq!(locks, RestoreLocks::Pending("switch cancelling"));
    }

    #[test]
    fn an_expired_launcher_with_no_desktop_is_stopped_before_retry() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(44), RecoveryTray::Missing, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), RecoveryTray::Missing, false, false), Recovery::StopLauncher);
        assert_eq!(recovery.step(secs(46), RecoveryTray::Missing, false, true), Recovery::Start);
    }

    #[test]
    fn a_confirmed_post_outlives_the_sender_and_a_cancelled_attempt_does_not() {
        assert_eq!(ExitState::from_signals(false, false, false), ExitState::Unsent);
        assert_eq!(ExitState::from_signals(true, false, false), ExitState::Uncertain);
        assert_eq!(ExitState::from_signals(true, false, true), ExitState::Unsent);
        assert_eq!(ExitState::from_signals(true, true, true), ExitState::Posted);
    }

    #[test]
    fn desktop_recovery_uses_the_same_observations_for_each_restorer() {
        for exit in [ExitState::Unsent, ExitState::Uncertain, ExitState::Posted] {
            for original_alive in [false, true] {
                for original_tray in [OriginalTray::Missing, OriginalTray::Present, OriginalTray::Answering(9)] {
                    for file_operations in [false, true] {
                        for tray_gone_grace_elapsed in [false, true] {
                            let facts = DesktopFacts {
                                exit, original_alive, original_tray, file_operations,
                                tray_gone_grace_elapsed, ..DesktopFacts::default()
                            };
                            let action = recover_desktop(facts);
                            let expected = match (exit, original_alive, original_tray, file_operations, tray_gone_grace_elapsed) {
                                (_, _, _, true, _) => DesktopAction::WaitForCopy,
                                (ExitState::Uncertain, true, OriginalTray::Answering(window), false, _) => DesktopAction::RepostExit(window),
                                (ExitState::Uncertain | ExitState::Posted, true, OriginalTray::Answering(_) | OriginalTray::Present, _, _) => DesktopAction::WaitForOriginal,
                                (ExitState::Uncertain | ExitState::Posted, true, OriginalTray::Missing, false, true) => DesktopAction::StopRetiringExplorer,
                                (ExitState::Uncertain | ExitState::Posted, true, OriginalTray::Missing, _, _) => DesktopAction::WaitForOriginal,
                                _ => DesktopAction::Restore,
                            };
                            assert_eq!(action, expected, "{exit:?} {original_alive} {original_tray:?} {file_operations} {tray_gone_grace_elapsed}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_uncertain_post_uses_the_recreated_tray_of_the_same_process() {
        let mut facts = DesktopFacts {
            exit: ExitState::Uncertain, original_alive: true,
            original_tray: OriginalTray::Answering(70), ..DesktopFacts::default()
        };
        assert_eq!(recover_desktop(facts), DesktopAction::RepostExit(70));

        facts.original_tray = OriginalTray::Answering(71);
        assert_eq!(recover_desktop(facts), DesktopAction::RepostExit(71));
    }

    #[test]
    fn ready_replacement_can_release_a_still_retiring_desktop_and_its_guard() {
        for original_alive in [false, true] {
            let mut facts = DesktopFacts {
                exit: ExitState::Posted, original_alive, tray_gone_grace_elapsed: true,
                explorer_ready: Some(42), supervisor_available: true, ..DesktopFacts::default()
            };
            assert_eq!(recover_desktop(facts), DesktopAction::Ready { pid: 42, retiring: original_alive });
            facts.explorer_ready = None;
            assert_eq!(recover_desktop(facts), if original_alive {
                DesktopAction::StopRetiringExplorer
            } else {
                DesktopAction::RequestFallback
            });
        }
    }

    #[test]
    fn supervisor_can_disappear_with_a_shell_window_or_a_tray() {
        for supervisor_available in [false, true] {
            for adoption_elapsed in [false, true] {
                let facts = DesktopFacts {
                    supervisor_available, adoption_elapsed, explorust_owner: Some(42),
                    ..DesktopFacts::default()
                };
                assert_eq!(recover_desktop(facts), match (supervisor_available, adoption_elapsed) {
                    (true, _) => DesktopAction::RequestFallback,
                    (false, false) => DesktopAction::WaitForAdoption,
                    (false, true) => DesktopAction::StopOrphan(42),
                });
            }
        }
    }

    #[test]
    fn a_missing_original_tray_can_expire_even_if_the_post_confirmation_was_lost() {
        let mut facts = DesktopFacts {
            exit: ExitState::Uncertain, original_alive: true, ..DesktopFacts::default()
        };
        assert_eq!(recover_desktop(facts), DesktopAction::WaitForOriginal);
        facts.tray_gone_grace_elapsed = true;
        assert_eq!(recover_desktop(facts), DesktopAction::StopRetiringExplorer);
        facts.file_operations = true;
        assert_eq!(recover_desktop(facts), DesktopAction::WaitForCopy);
    }

    #[test]
    fn a_missing_tray_does_not_release_the_original_shell_process() {
        for exit in [ExitState::Uncertain, ExitState::Posted] {
            let mut facts = DesktopFacts { exit, original_alive: true, ..DesktopFacts::default() };
            assert_eq!(recover_desktop(facts), DesktopAction::WaitForOriginal);
            facts.original_alive = false;
            assert_eq!(recover_desktop(facts), DesktopAction::Restore);
        }
    }

    #[test]
    fn per_user_restore_only_tries_its_session_locks() {
        let locks = try_restore_locks(
            |ms| Ok::<_, ()>(("session switch", ms)),
            |ms| Ok(("session recovery", ms)),
        );
        assert_eq!(locks, RestoreLocks::Acquired(("session switch", 0), ("session recovery", 0)));
    }

    #[test]
    fn third_problem_logon_turns_off() {
        assert_eq!(at_logon(0), AtLogon::Run);
        assert_eq!(at_logon(2), AtLogon::Run);
        assert_eq!(at_logon(3), AtLogon::TurnOff);
    }

    #[test]
    fn recovery_starts_once_the_retiring_process_is_gone() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Missing, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, false), Recovery::Start);
    }

    #[test]
    fn recovery_starts_directly_when_the_signalled_guard_disappears() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Missing, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, false), Recovery::Start);
    }

    #[test]
    fn every_first_exit_or_timeout_leaves_a_full_second_attempt() {
        for first_exit in 0..=60 {
            let mut recovery = ExplorerRecovery::default();
            recovery.started(secs(0));
            let retry = (0..=60).find(|&now| {
                let step = recovery.step(secs(now), RecoveryTray::Missing, false, now >= first_exit);
                assert_ne!(step, Recovery::Exhausted);
                matches!(step, Recovery::Start | Recovery::StopLauncher)
            }).unwrap();
            assert_eq!(retry, (first_exit + 10).min(45));
            recovery.started(secs(retry));
            assert_eq!(recovery.step(secs(retry + 44), RecoveryTray::Missing, false, false), Recovery::Wait);
        }
    }

    #[test]
    fn recovery_waits_for_adoption_when_the_shell_still_owns_the_taskbar() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Other, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Other, false, false), Recovery::Wait);
    }

    #[test]
    fn recovery_gives_the_last_attempt_its_entire_window() {
        let mut recovery = ExplorerRecovery::default();
        for attempt in 0..3 {
            let start = secs(attempt * 45);
            assert_eq!(recovery.step(start, RecoveryTray::Missing, false, true), Recovery::Start);
            recovery.started(start);
        }
        assert_eq!(recovery.step(secs(134), RecoveryTray::Missing, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(135), RecoveryTray::Missing, false, true), Recovery::Exhausted);
    }

    #[test]
    fn recovery_gives_a_forwarded_process_time_to_create_its_tray() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, true), Recovery::Wait);
    }

    #[test]
    fn a_hung_tray_owned_by_the_first_launch_can_be_retried() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        let tray = RecoveryTray::Explorer(42);
        assert_eq!(recovery.step(secs(0), tray, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(44), tray, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), tray, false, false), Recovery::Restart(42));
        assert_eq!(recovery.step(secs(46), tray, false, false), Recovery::Wait);
    }

    #[test]
    fn recovery_retries_the_forwarded_explorer_and_preserves_active_copies() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, true), Recovery::Wait);
        let actual_owner = RecoveryTray::Explorer(54);
        assert_eq!(recovery.step(secs(2), actual_owner, false, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(46), actual_owner, false, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(47), actual_owner, false, true), Recovery::Restart(54));
        let facts = DesktopFacts { explorer_owner: Some(54), file_operations: true, ..DesktopFacts::default() };
        assert_eq!(recover_desktop(facts), DesktopAction::WaitForCopy);
    }

    #[test]
    fn recovery_warns_after_retirement_attempts_are_exhausted() {
        let mut facts = DesktopFacts {
            exit: ExitState::Posted, original_alive: true, tray_gone_grace_elapsed: true,
            ..DesktopFacts::default()
        };
        assert_eq!(recover_desktop(facts), DesktopAction::StopRetiringExplorer);
        facts.retirement_attempts = RETIRING_EXPLORER_ATTEMPTS;
        assert_eq!(recover_desktop(facts), DesktopAction::NeedsSignOut);
        facts.original_alive = false;
        assert_eq!(recover_desktop(facts), DesktopAction::Restore);
    }

    #[test]
    fn recovery_can_retry_an_explorer_that_windows_launched() {
        let mut recovery = ExplorerRecovery::default();
        let windows_owner = RecoveryTray::Explorer(54);
        assert_eq!(recovery.step(secs(0), windows_owner, false, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(44), windows_owner, false, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), windows_owner, false, true), Recovery::Restart(54));
        let facts = DesktopFacts {
            exit: ExitState::Posted, original_alive: true, explorer_owner: Some(54),
            ..DesktopFacts::default()
        };
        assert_eq!(recover_desktop(facts), DesktopAction::Restore);
    }

    #[test]
    fn late_folder_recovery_uses_the_desktop_at_the_time_of_reopen() {
        let mut facts = DesktopFacts {
            exit: ExitState::Posted, original_alive: true, explorer_ready: Some(54),
            ..DesktopFacts::default()
        };
        assert_eq!(recover_desktop(facts), DesktopAction::Ready { pid: 54, retiring: true });
        facts.original_alive = false;
        facts.explorer_ready = None;
        assert_eq!(recover_desktop(facts), DesktopAction::Restore);
        facts.explorust_ready = Some(72);
        facts.file_operations = true;
        assert_eq!(recover_desktop(facts), DesktopAction::WaitForCopy);
        facts.file_operations = false;
        assert_eq!(recover_desktop(facts), DesktopAction::Ready { pid: 72, retiring: false });
    }

    #[test]
    fn recovery_does_not_restart_an_unrelated_tray() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(45), RecoveryTray::Other, false, false), Recovery::Exhausted);
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn hung_taskbar_is_a_problem_after_20_s_and_3_misses() {
        let mut w = Watchdog::default();
        // A hung probe takes ANSWER_WITHIN, so they start 10 s apart.
        assert_eq!(w.probe(Probe::NoAnswer, secs(0)), None);
        assert_eq!(w.probe(Probe::NoAnswer, secs(10)), None);
        assert!(w.probe(Probe::NoAnswer, secs(20)).unwrap().contains("not answered"));
    }

    #[test]
    fn an_answer_resets() {
        let mut w = Watchdog::default();
        w.probe(Probe::NoAnswer, secs(0));
        w.probe(Probe::NoAnswer, secs(10));
        assert_eq!(w.probe(Probe::Answered, secs(20)), None);
        assert_eq!(w.probe(Probe::NoAnswer, secs(25)), None);
        assert_eq!(w.probe(Probe::NoAnswer, secs(35)), None);
        assert_eq!(w.probe(Probe::NoAnswer, secs(44)), None);
        assert!(w.probe(Probe::NoAnswer, secs(45)).is_some());
    }

    #[test]
    fn misses_alone_are_not_enough() {
        // A sleep or a frozen guard: many misses counted in a short time are not 20 s of silence.
        let mut w = Watchdog::default();
        for i in 0..10 {
            assert_eq!(w.probe(Probe::NoAnswer, Duration::from_millis(i * 100)), None);
        }
    }

    #[test]
    fn time_alone_is_not_enough() {
        // Two misses a minute apart (the machine was asleep between them).
        let mut w = Watchdog::default();
        assert_eq!(w.probe(Probe::NoAnswer, secs(0)), None);
        assert_eq!(w.probe(Probe::NoAnswer, secs(60)), None);
    }

    #[test]
    fn silent_then_gone_is_one_outage() {
        let mut w = Watchdog::default();
        assert_eq!(w.probe(Probe::NoAnswer, secs(0)), None);
        assert_eq!(w.probe(Probe::Missing, secs(10)), None);
        assert!(w.probe(Probe::Missing, secs(20)).unwrap().contains("not answered"));
    }

    #[test]
    fn missing_taskbar_is_a_problem_after_30_s() {
        let mut w = Watchdog::default();
        for t in [0, 5, 10, 15, 20, 25] {
            assert_eq!(w.probe(Probe::Missing, secs(t)), None);
        }
        assert!(w.probe(Probe::Missing, secs(30)).unwrap().contains("gone"));
    }
}
