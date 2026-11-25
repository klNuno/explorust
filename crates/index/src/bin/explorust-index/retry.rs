use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const RETRY_INTERVAL: Duration = Duration::from_millis(250);
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
const FAST_ATTEMPTS: u8 = 3;
const SUMMARY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct BackoffLog {
    last: Option<Instant>,
    count: usize,
}

impl BackoffLog {
    pub fn backed_off(&mut self, count: usize) {
        self.count += count;
    }

    pub fn summary(&mut self, now: Instant) -> Option<usize> {
        if self.count == 0 || self.last.is_some_and(|last| now.duration_since(last) < SUMMARY_INTERVAL) {
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.count))
    }
}

#[derive(Clone, Copy)]
pub struct LinkRetry {
    pub frn: u64,
    pub due: Instant,
    /// Fast attempts already made, capped once the slower schedule starts.
    attempts: u8,
    /// Whether the indexed name itself may be stale.
    pub repair_name: bool,
}

impl LinkRetry {
    pub fn new(frn: u64, now: Instant, repair_name: bool) -> Self {
        Self {
            frn,
            due: now,
            attempts: 0,
            repair_name,
        }
    }

    pub fn recovering(self) -> bool {
        self.attempts == FAST_ATTEMPTS
    }

    pub fn after_failure(self, now: Instant) -> Self {
        let attempts = (self.attempts + 1).min(FAST_ATTEMPTS);
        let interval = if attempts == FAST_ATTEMPTS {
            RECOVERY_INTERVAL
        } else {
            RETRY_INTERVAL
        };
        Self {
            due: now + interval,
            attempts,
            ..self
        }
    }
}

pub fn queue(
    retries: &mut HashMap<u32, LinkRetry>,
    rec: u32,
    frn: u64,
    now: Instant,
    repair_name: bool,
) {
    let next = LinkRetry::new(frn, now, repair_name);
    let job = retries.entry(rec).or_insert(next);
    if job.frn != frn || repair_name {
        *job = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_coalesces_backoff_transitions_within_one_interval() {
        let now = Instant::now();
        let mut log = BackoffLog::default();
        log.backed_off(2);
        assert_eq!(log.summary(now), Some(2));
        log.backed_off(1);
        assert_eq!(log.summary(now + RETRY_INTERVAL), None);
        assert_eq!(log.summary(now + SUMMARY_INTERVAL), Some(1));
        assert_eq!(log.summary(now + 2 * SUMMARY_INTERVAL), None);
    }

    #[test]
    fn a_link_change_promotes_a_failed_rename_lookup() {
        let now = Instant::now();
        let rename = LinkRetry::new(1234, now, false).after_failure(now);
        let mut retries = HashMap::from([(42, rename)]);
        queue(&mut retries, 42, 1234, now, true);
        assert!(retries[&42].repair_name);
        assert_eq!(retries[&42].attempts, 0);
        assert_eq!(retries[&42].due, now);
        queue(&mut retries, 42, 1234, now, false);
        assert!(retries[&42].repair_name);
    }

    #[test]
    fn link_failures_slow_down_after_the_first_three_attempts() {
        let now = Instant::now();
        let first = LinkRetry::new(1234, now, false);
        assert_eq!(first.due, now);
        let second = first.after_failure(now);
        assert_eq!(second.frn, 1234);
        assert!(!second.repair_name);
        assert!(second.due > now);
        let third = second.after_failure(second.due);
        assert!(third.due > second.due);
        let recovery = third.after_failure(third.due);
        assert!(recovery.recovering());
        assert_eq!(recovery.due - third.due, RECOVERY_INTERVAL);
        assert_eq!(recovery.frn, 1234);
        let later = recovery.after_failure(recovery.due);
        assert!(later.recovering());
        assert_eq!(later.due - recovery.due, RECOVERY_INTERVAL);
    }
}
