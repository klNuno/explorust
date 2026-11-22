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

/// The next step when returning from a failed live switch or from an uninstall.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Ready(u32),
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
/// One window for the existing desktop owner and each fresh Explorer process, plus time to
/// reap a hung launcher between attempts and finish the last taskbar probe.
pub const RECOVERY_BUDGET: Duration =
    Duration::from_secs(EXPLORER_WINDOW.as_secs() * (1 + EXPLORER_ATTEMPTS as u64) + 30);

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
        ready: Option<u32>,
        tray: RecoveryTray,
        guard_waiting: bool,
        launcher_ended: bool,
    ) -> Recovery {
        if let Some(pid) = ready {
            return Recovery::Ready(pid);
        }
        let expired = now - self.started >= EXPLORER_WINDOW;
        if self.attempts > 0 {
            // Explorer can hand the launch to another process before the tray appears.
            let handed_off = launcher_ended
                && now - *self.ended.get_or_insert(now) >= EXPLORER_RELAUNCH_GRACE;
            if !expired && !handed_off {
                return Recovery::Wait;
            }
        }
        if tray == RecoveryTray::Other || guard_waiting {
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
    fn third_problem_logon_turns_off() {
        assert_eq!(at_logon(0), AtLogon::Run);
        assert_eq!(at_logon(2), AtLogon::Run);
        assert_eq!(at_logon(3), AtLogon::TurnOff);
    }

    #[test]
    fn recovery_starts_once_the_retiring_taskbar_is_gone() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), None, RecoveryTray::Other, false, false), Recovery::Wait);
        // The retiring process can still be alive without a desktop taskbar.
        assert_eq!(recovery.step(secs(1), None, RecoveryTray::Missing, false, false), Recovery::Start);
        assert_eq!(recovery.step(secs(2), Some(42), RecoveryTray::Other, false, false), Recovery::Ready(42));
    }

    #[test]
    fn recovery_starts_directly_when_the_signalled_guard_disappears() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), None, RecoveryTray::Missing, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), None, RecoveryTray::Missing, false, false), Recovery::Start);
    }

    #[test]
    fn every_first_exit_or_timeout_leaves_a_full_second_attempt() {
        for first_exit in 0..=60 {
            let mut recovery = ExplorerRecovery::default();
            recovery.started(secs(0));
            let retry = (0..=60).find(|&now| {
                let step = recovery.step(secs(now), None, RecoveryTray::Missing, false, now >= first_exit);
                assert_ne!(step, Recovery::Exhausted);
                step == Recovery::Start
            }).unwrap();
            assert_eq!(retry, (first_exit + 10).min(45));
            recovery.started(secs(retry));
            assert_eq!(recovery.step(secs(retry + 44), None, RecoveryTray::Missing, false, false), Recovery::Wait);
            assert_eq!(recovery.step(secs(retry + 45), Some(42), RecoveryTray::Other, false, true), Recovery::Ready(42));
        }
    }

    #[test]
    fn recovery_waits_for_adoption_when_the_shell_still_owns_the_taskbar() {
        let mut recovery = ExplorerRecovery::default();
        assert_eq!(recovery.step(secs(0), None, RecoveryTray::Other, true, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(1), None, RecoveryTray::Other, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), Some(42), RecoveryTray::Other, true, false), Recovery::Ready(42));
    }

    #[test]
    fn recovery_probes_before_exhausting_the_last_attempt() {
        let mut recovery = ExplorerRecovery::default();
        for attempt in 0..3 {
            let start = secs(attempt * 45);
            assert_eq!(recovery.step(start, None, RecoveryTray::Missing, false, false), Recovery::Start);
            recovery.started(start);
        }
        assert_eq!(recovery.step(secs(134), None, RecoveryTray::Missing, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(135), None, RecoveryTray::Missing, false, true), Recovery::Exhausted);
        assert_eq!(recovery.step(secs(135), Some(42), RecoveryTray::Other, false, true), Recovery::Ready(42));
    }

    #[test]
    fn recovery_accepts_a_tray_created_by_the_forwarded_process() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(1), None, RecoveryTray::Missing, false, true), Recovery::Wait);
        assert_eq!(recovery.step(secs(11), Some(42), RecoveryTray::Other, false, true), Recovery::Ready(42));
    }

    #[test]
    fn a_hung_tray_owned_by_the_first_launch_can_be_retried() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(44), None, RecoveryTray::Launched, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(45), None, RecoveryTray::Launched, false, false), Recovery::Start);
        recovery.started(secs(47));
        assert_eq!(recovery.step(secs(91), None, RecoveryTray::Launched, false, false), Recovery::Wait);
        assert_eq!(recovery.step(secs(92), Some(42), RecoveryTray::Launched, false, false), Recovery::Ready(42));
    }

    #[test]
    fn recovery_does_not_restart_an_unrelated_tray() {
        let mut recovery = ExplorerRecovery::default();
        recovery.started(secs(0));
        assert_eq!(recovery.step(secs(45), None, RecoveryTray::Other, false, false), Recovery::Exhausted);
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
