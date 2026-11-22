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
    pub supervisor_available: bool,
    pub shell_window_owner: Option<u32>,
    pub shell_tray_owner: Option<u32>,
    pub adoption_elapsed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DesktopAction {
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
    if retiring && facts.original_tray != OriginalTray::Missing {
        // Sent probes can be answered ahead of queued exit requests. Resolve uncertainty by
        // posting to the current tray of that PID, which can have recreated its window.
        if let OriginalTray::Answering(window) = facts.original_tray
            && facts.exit == ExitState::Uncertain
            && !facts.file_operations
        {
            return DesktopAction::RepostExit(window);
        }
        return DesktopAction::WaitForOriginal;
    }
    if let Some(pid) = facts.explorer_ready {
        return DesktopAction::Ready { pid, retiring };
    }
    if retiring {
        // Explorer can still route a new launch to a folder while the old shell process owns
        // the desktop. Wait for it to release ownership, even after its tray disappears.
        return if facts.tray_gone_grace_elapsed && !facts.file_operations {
            DesktopAction::StopRetiringExplorer
        } else {
            DesktopAction::WaitForOriginal
        };
    }
    if facts.supervisor_available {
        return DesktopAction::RequestFallback;
    }
    if let Some(pid) = facts.shell_window_owner.or(facts.shell_tray_owner) {
        return if facts.adoption_elapsed {
            DesktopAction::StopOrphan(pid)
        } else {
            DesktopAction::WaitForAdoption
        };
    }
    DesktopAction::Restore
}

/// Hold both session locks while turning the current user back to Windows.
pub fn lock_for_restore<T, E>(
    switch: impl FnOnce(u32) -> Result<T, E>,
    recovery: impl FnOnce(u32) -> Result<T, E>,
) -> Result<(T, T), E> {
    const INFINITE: u32 = u32::MAX;
    Ok((switch(INFINITE)?, recovery(INFINITE)?))
}

/// The timing decision for an Explorer launch when the desktop still needs recovery.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Wait,
    Start,
    Restart(u32),
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryTray {
    Missing,
    Replacement(u32),
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
        file_operations: bool,
    ) -> Recovery {
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
        if matches!(tray, RecoveryTray::Replacement(_)) && file_operations {
            return Recovery::Wait;
        }
        if self.attempts == EXPLORER_ATTEMPTS {
            return Recovery::Exhausted;
        }
        if let RecoveryTray::Replacement(pid) = tray {
            Recovery::Restart(pid)
        } else {
            Recovery::Start
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for owners in [(Some(42), None), (None, Some(42)), (Some(42), Some(42))] {
            for supervisor_available in [false, true] {
                for adoption_elapsed in [false, true] {
                    let facts = DesktopFacts {
                        supervisor_available, adoption_elapsed,
                        shell_window_owner: owners.0, shell_tray_owner: owners.1,
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
        assert_eq!(recover_desktop(facts), DesktopAction::WaitForOriginal);
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
    fn per_user_restore_waits_only_for_its_session_even_during_a_machine_install() {
        let locks = lock_for_restore(
            |ms| Ok::<_, ()>(("session switch", ms)),
            |ms| Ok(("session recovery", ms)),
        );
        assert_eq!(locks, Ok((("session switch", u32::MAX), ("session recovery", u32::MAX))));
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
        assert_eq!(recovery.step(secs(0), RecoveryTray::Missing, true, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, false, false), Recovery::Start);
    }

    #[test]
    fn recovery_starts_directly_when_the_signalled_guard_disappears() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Missing, true, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, false, false), Recovery::Start);
    }

    #[test]
    fn every_first_exit_or_timeout_leaves_a_full_second_attempt() {
        for first_exit in 0..=60 {
            let mut recovery = ExplorerRecovery::default();
            recovery.started(secs(0));
            let retry = (0..=60).find(|&now| {
                let step = recovery.step(secs(now), RecoveryTray::Missing, false, now >= first_exit, false);
                assert_ne!(step, Recovery::Exhausted);
                step == Recovery::Start
            }).unwrap();
            assert_eq!(retry, (first_exit + 10).min(45));
            recovery.started(secs(retry));
            assert_eq!(recovery.step(secs(retry + 44), RecoveryTray::Missing, false, false, false), Recovery::Wait);
        }
    }

    #[test]
    fn recovery_waits_for_adoption_when_the_shell_still_owns_the_taskbar() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Other, true, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Other, false, false, false), Recovery::Wait);
    }

    #[test]
    fn recovery_gives_the_last_attempt_its_entire_window() {
        let mut recovery = ExplorerRecovery::default();
        for attempt in 0..3 {
            let start = secs(attempt * 45);
            assert_eq!(recovery.step(start, RecoveryTray::Missing, false, false, false), Recovery::Start);
            recovery.started(start);
        }
        assert_eq!(recovery.step(secs(134), RecoveryTray::Missing, false, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(135), RecoveryTray::Missing, false, true, false), Recovery::Exhausted);
    }

    #[test]
    fn recovery_gives_a_forwarded_process_time_to_create_its_tray() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, true, false), Recovery::Wait);
    }

    #[test]
    fn a_hung_tray_owned_by_the_first_launch_can_be_retried() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(44), RecoveryTray::Replacement(42), false, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), RecoveryTray::Replacement(42), false, false, false), Recovery::Restart(42));
        recovery.started(secs(47));
        assert_eq!(recovery.step(secs(91), RecoveryTray::Replacement(42), false, false, false), Recovery::Wait);
    }

    #[test]
    fn recovery_retries_the_forwarded_explorer_and_preserves_active_copies() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        let actual_owner = RecoveryTray::Replacement(54);
        assert_eq!(recovery.step(secs(1), RecoveryTray::Missing, false, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(40), actual_owner, false, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), actual_owner, false, true, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(46), actual_owner, false, true, false), Recovery::Restart(54));
    }

    #[test]
    fn recovery_does_not_restart_an_unrelated_tray() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(45), RecoveryTray::Other, false, false, false), Recovery::Exhausted);
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
