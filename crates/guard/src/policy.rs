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
}

pub const RECOVERY_LIMIT: Duration = Duration::from_secs(45);
const EXPLORER_RELAUNCH_GRACE: Duration = Duration::from_secs(10);

/// Give a forwarded launch time to create a tray while leaving time for a retry.
pub fn explorer_retry_due(elapsed: Duration, since_exit: Duration) -> bool {
    since_exit >= EXPLORER_RELAUNCH_GRACE
        || RECOVERY_LIMIT.saturating_sub(elapsed) <= EXPLORER_RELAUNCH_GRACE
}

pub fn recovery_step(replacement: Option<u32>, taskbar_present: bool, launch_pending: bool) -> Recovery {
    if let Some(pid) = replacement {
        Recovery::Ready(pid)
    } else if taskbar_present || launch_pending {
        Recovery::Wait
    } else {
        Recovery::Start
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
        assert_eq!(recovery_step(None, true, false), Recovery::Wait);
        // The retiring process can still be alive without a desktop taskbar.
        assert_eq!(recovery_step(None, false, false), Recovery::Start);
        assert_eq!(recovery_step(Some(42), false, false), Recovery::Ready(42));
    }

    #[test]
    fn recovery_starts_directly_when_the_signalled_guard_disappears() {
        assert_eq!(recovery_step(None, false, true), Recovery::Wait);
        assert_eq!(recovery_step(None, false, false), Recovery::Start);
    }

    #[test]
    fn recovery_leaves_time_to_retry_when_the_first_explorer_exits_late() {
        assert!(!explorer_retry_due(secs(7), secs(2)));
        assert!(explorer_retry_due(secs(15), secs(10)));
        assert!(explorer_retry_due(secs(39), Duration::ZERO));
    }

    #[test]
    fn recovery_waits_for_adoption_when_the_shell_still_owns_the_taskbar() {
        assert_eq!(recovery_step(None, true, true), Recovery::Wait);
        assert_eq!(recovery_step(None, true, false), Recovery::Wait);
        assert_eq!(recovery_step(None, true, true), Recovery::Wait);
        assert_eq!(recovery_step(Some(42), false, true), Recovery::Ready(42));
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
