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

#[derive(Clone, Copy)]
pub struct DesktopFacts {
    pub exit: ExitState,
    pub original_alive: bool,
    pub original_tray: Probe,
    pub file_operations: bool,
    pub explorer_ready: Option<u32>,
    pub supervisor_available: bool,
    pub shell_owner: Option<u32>,
    pub adoption_elapsed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DesktopAction {
    WaitForOriginal,
    RepostExit,
    Ready(u32),
    RequestFallback,
    WaitForAdoption,
    StopOrphan(u32),
    Restore,
}

/// Decide from this observation only. Each caller probes again after waiting or taking action.
pub fn recover_desktop(facts: DesktopFacts) -> DesktopAction {
    let retiring = facts.exit.pending() && facts.original_alive;
    if retiring && facts.original_tray != Probe::Missing {
        // Sent probes can be answered ahead of queued exit requests. Resolve uncertainty by
        // posting to the same tray once, never by accepting its answer as proof of rollback.
        return if facts.exit == ExitState::Uncertain
            && facts.original_tray == Probe::Answered
            && !facts.file_operations
        {
            DesktopAction::RepostExit
        } else {
            DesktopAction::WaitForOriginal
        };
    }
    if let Some(pid) = facts.explorer_ready {
        return DesktopAction::Ready(pid);
    }
    // A supervisor of the retiring Explorer can still be waiting for its process to end.
    if !retiring && facts.supervisor_available {
        return DesktopAction::RequestFallback;
    }
    if let Some(pid) = facts.shell_owner {
        return if facts.adoption_elapsed {
            DesktopAction::StopOrphan(pid)
        } else {
            DesktopAction::WaitForAdoption
        };
    }
    DesktopAction::Restore
}

/// The timing decision for an Explorer launch when the desktop still needs recovery.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Wait,
    Start,
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryTray {
    Missing,
    Launched,
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
    ) -> Recovery {
        let expired = now - self.started >= EXPLORER_WINDOW;
        if self.attempts > 0 {
            // Explorer can hand the launch to another process before the tray appears.
            let handed_off = launcher_ended
                && now - *self.ended.get_or_insert(now) >= EXPLORER_RELAUNCH_GRACE;
            if !expired && !handed_off {
                return Recovery::Wait;
            }
        }
        if tray == RecoveryTray::Other || blocked {
            return if expired { Recovery::Exhausted } else { Recovery::Wait };
        }
        if self.attempts == EXPLORER_ATTEMPTS {
            Recovery::Exhausted
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
                for original_tray in [Probe::Missing, Probe::NoAnswer, Probe::Answered] {
                    for file_operations in [false, true] {
                        let facts = DesktopFacts {
                            exit, original_alive, original_tray, file_operations,
                            explorer_ready: None, supervisor_available: false,
                            shell_owner: None, adoption_elapsed: false,
                        };
                        let action = recover_desktop(facts);
                        let expected = match (exit, original_alive, original_tray, file_operations) {
                            (ExitState::Uncertain, true, Probe::Answered, false) => DesktopAction::RepostExit,
                            (ExitState::Uncertain | ExitState::Posted, true, Probe::Answered | Probe::NoAnswer, _) => DesktopAction::WaitForOriginal,
                            _ => DesktopAction::Restore,
                        };
                        assert_eq!(action, expected, "{exit:?} {original_alive} {original_tray:?} {file_operations}");
                    }
                }
            }
        }
    }

    #[test]
    fn ready_replacement_does_not_have_to_wait_for_the_old_process_to_exit() {
        for original_alive in [false, true] {
            let mut facts = DesktopFacts {
                exit: ExitState::Posted, original_alive, original_tray: Probe::Missing,
                file_operations: false, explorer_ready: Some(42), supervisor_available: true,
                shell_owner: None, adoption_elapsed: false,
            };
            assert_eq!(recover_desktop(facts), DesktopAction::Ready(42));
            facts.explorer_ready = None;
            assert_eq!(recover_desktop(facts), if original_alive {
                DesktopAction::Restore
            } else {
                DesktopAction::RequestFallback
            });
        }
    }

    #[test]
    fn supervisor_can_disappear_after_fallback_was_requested() {
        for supervisor_available in [false, true] {
            for adoption_elapsed in [false, true] {
                let facts = DesktopFacts {
                    exit: ExitState::Unsent, original_alive: false, original_tray: Probe::Missing,
                    file_operations: false, explorer_ready: None, supervisor_available,
                    shell_owner: Some(42), adoption_elapsed,
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
    fn third_problem_logon_turns_off() {
        assert_eq!(at_logon(0), AtLogon::Run);
        assert_eq!(at_logon(2), AtLogon::Run);
        assert_eq!(at_logon(3), AtLogon::TurnOff);
    }

    #[test]
    fn recovery_starts_once_the_retiring_taskbar_is_gone() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), RecoveryTray::Other, false, false), Recovery::Wait);
        // The retiring process can still be alive without a desktop taskbar.
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
                step == Recovery::Start
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
            assert_eq!(recovery.step(start, RecoveryTray::Missing, false, false), Recovery::Start);
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
        assert_eq!(recovery.step(secs(44), RecoveryTray::Launched, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), RecoveryTray::Launched, false, false), Recovery::Start);
        recovery.started(secs(47));
        assert_eq!(recovery.step(secs(91), RecoveryTray::Launched, false, false), Recovery::Wait);
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
