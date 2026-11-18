//! When to restart explorust and when to give up and start explorer.exe. No Windows calls here,
//! so the rules are unit-tested on any host: `rustc --test crates/guard/src/policy.rs`.

use std::time::{Duration, Instant};

/// A failure is any end of explorust that nobody asked for: an exit before the ready signal, a ready
/// timeout, or a crash after ready.
pub struct Policy {
    window: Duration,
    max_failures: usize,
    failures: Vec<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Restart,
    Fallback,
}

impl Policy {
    /// Restart once; a second failure within `window` falls back to explorer.
    pub fn new(window: Duration) -> Policy {
        Policy { window, max_failures: 2, failures: Vec::new() }
    }

    pub fn failed(&mut self, now: Instant) -> Next {
        self.failures.retain(|t| now.duration_since(*t) < self.window);
        self.failures.push(now);
        if self.failures.len() >= self.max_failures { Next::Fallback } else { Next::Restart }
    }
}

/// How long explorust gets to signal ready. A changed Windows build adds the symbol download and the
/// UWP self-test.
pub fn ready_timeout(self_test: bool) -> Duration {
    if self_test { Duration::from_secs(120) } else { Duration::from_secs(45) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_failure_restarts_second_falls_back() {
        let t = Instant::now();
        let mut p = Policy::new(Duration::from_secs(300));
        assert_eq!(p.failed(t), Next::Restart);
        assert_eq!(p.failed(t + Duration::from_secs(10)), Next::Fallback);
    }

    #[test]
    fn failures_far_apart_each_restart() {
        let t = Instant::now();
        let mut p = Policy::new(Duration::from_secs(300));
        assert_eq!(p.failed(t), Next::Restart);
        assert_eq!(p.failed(t + Duration::from_secs(301)), Next::Restart);
        assert_eq!(p.failed(t + Duration::from_secs(602)), Next::Restart);
        assert_eq!(p.failed(t + Duration::from_secs(610)), Next::Fallback);
    }

    #[test]
    fn self_test_gets_more_time() {
        assert!(ready_timeout(true) > ready_timeout(false));
    }
}
