//! When to give up on explorust and start explorer.exe. No Windows calls here, so the rules are
//! unit-tested on any host: `rustc --test crates/guard/src/policy.rs`.
//!
//! The rule since 2026-11-21: at the first problem explorust has (an exit before the ready
//! signal, a ready timeout, a crash, a hung taskbar, a part the shell gave up on) the guard starts
//! explorer.exe for the rest of the session and tells the user once. A problem at three logons in
//! a row turns explorust off for good (the per-user Shell value is deleted), so a Windows update
//! that breaks it never makes every logon a fallback.

use std::time::Duration;

/// How long explorust gets to signal ready. A changed Windows build adds the symbol download and the
/// UWP self-test.
pub fn ready_timeout(self_test: bool) -> Duration {
    if self_test { Duration::from_secs(120) } else { Duration::from_secs(45) }
}

/// Logons in a row that ended in a problem before explorust is turned off.
pub const MAX_STREAK: u32 = 3;

/// How long a ready explorust must run without a problem before the streak goes back to zero.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(300);

/// What a logon does given the streak of problem logons before it.
#[derive(Debug, PartialEq, Eq)]
pub enum AtLogon {
    Run,
    TurnOff,
}

pub fn at_logon(streak: u32) -> AtLogon {
    if streak >= MAX_STREAK { AtLogon::TurnOff } else { AtLogon::Run }
}

/// The taskbar check: one probe every `PROBE`, a window that does not answer for `HUNG_FOR` or is
/// missing for `MISSING_FOR` is a problem.
pub const PROBE: Duration = Duration::from_secs(5);
pub const HUNG_FOR: Duration = Duration::from_secs(20);
pub const MISSING_FOR: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Probe {
    Answered,
    NoAnswer,
    Missing,
}

/// Counts probes in a row of each bad kind; `problem` names the first threshold crossed.
#[derive(Default)]
pub struct Watchdog {
    hung: u32,
    missing: u32,
}

impl Watchdog {
    pub fn probe(&mut self, p: Probe) -> Option<String> {
        match p {
            Probe::Answered => {
                self.hung = 0;
                self.missing = 0;
            }
            Probe::NoAnswer => {
                self.hung += 1;
                self.missing = 0;
            }
            Probe::Missing => {
                self.missing += 1;
                self.hung = 0;
            }
        }
        let ticks = |d: Duration| (d.as_secs() / PROBE.as_secs()) as u32;
        if self.hung >= ticks(HUNG_FOR) {
            return Some(format!("the taskbar has not answered for {} s", HUNG_FOR.as_secs()));
        }
        if self.missing >= ticks(MISSING_FOR) {
            return Some(format!("the taskbar window has been gone for {} s", MISSING_FOR.as_secs()));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_test_gets_more_time() {
        assert!(ready_timeout(true) > ready_timeout(false));
    }

    #[test]
    fn third_problem_logon_turns_off() {
        assert_eq!(at_logon(0), AtLogon::Run);
        assert_eq!(at_logon(2), AtLogon::Run);
        assert_eq!(at_logon(3), AtLogon::TurnOff);
    }

    #[test]
    fn hung_taskbar_is_a_problem_after_20_s() {
        let mut w = Watchdog::default();
        for _ in 0..3 {
            assert_eq!(w.probe(Probe::NoAnswer), None);
        }
        assert!(w.probe(Probe::NoAnswer).unwrap().contains("not answered"));
    }

    #[test]
    fn an_answer_resets_the_count() {
        let mut w = Watchdog::default();
        for _ in 0..3 {
            w.probe(Probe::NoAnswer);
        }
        assert_eq!(w.probe(Probe::Answered), None);
        for _ in 0..3 {
            assert_eq!(w.probe(Probe::NoAnswer), None);
        }
    }

    #[test]
    fn missing_taskbar_is_a_problem_after_30_s() {
        let mut w = Watchdog::default();
        for _ in 0..5 {
            assert_eq!(w.probe(Probe::Missing), None);
        }
        assert!(w.probe(Probe::Missing).unwrap().contains("gone"));
    }
}
