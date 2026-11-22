use std::time::{Duration, Instant};

const RETRY_INTERVAL: Duration = Duration::from_millis(250);
const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy)]
pub struct LinkRetry {
    pub frn: u64,
    pub due: Instant,
    /// Attempts already made.
    pub attempts: u8,
    /// Whether the indexed name itself may be stale.
    pub repair_name: bool,
}

impl LinkRetry {
    pub fn new(frn: u64, now: Instant, repair_name: bool) -> Self {
        Self { frn, due: now, attempts: 0, repair_name }
    }

    pub fn after_failure(self, now: Instant) -> Option<Self> {
        let attempts = self.attempts + 1;
        (attempts < MAX_ATTEMPTS).then_some(Self { due: now + RETRY_INTERVAL, attempts, ..self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_failures_have_deadlines_and_stop_after_three_attempts() {
        let now = Instant::now();
        let first = LinkRetry::new(1234, now, false);
        assert_eq!(first.due, now);
        let second = first.after_failure(now).unwrap();
        assert_eq!(second.frn, 1234);
        assert!(!second.repair_name);
        assert!(second.due > now);
        let third = second.after_failure(second.due).unwrap();
        assert!(third.due > second.due);
        assert!(third.after_failure(third.due).is_none());
    }
}
