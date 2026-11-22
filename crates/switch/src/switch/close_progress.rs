use std::time::Duration;

// Explorer normally destroys the window shortly after replying, even for a synchronous send.
// A deferred close can outlast the switch. Keep tracking it through the desktop exit budget.
const AFTER_REPLY: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub enum Close {
    Pending,
    Destroyed,
    Kept,
}

#[derive(Default)]
pub struct CloseProgress {
    acknowledged: Option<Duration>,
}

impl CloseProgress {
    pub fn observe(&mut self, now: Duration, replied: bool, present: bool) -> Close {
        if !present {
            return Close::Destroyed;
        }
        if replied && now - *self.acknowledged.get_or_insert(now) >= AFTER_REPLY {
            Close::Kept
        } else {
            Close::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgment_does_not_discard_a_close_that_outlives_rollback() {
        let mut progress = CloseProgress::default();
        let at = Duration::from_millis;
        assert_eq!(progress.observe(at(177), true, true), Close::Pending);
        assert_eq!(progress.observe(at(2000), true, true), Close::Pending);
        assert_eq!(progress.observe(at(2200), true, false), Close::Destroyed);
    }

    #[test]
    fn a_late_reply_gets_its_own_observation_window() {
        let mut progress = CloseProgress::default();
        let at = Duration::from_secs;
        assert_eq!(progress.observe(at(40), false, true), Close::Pending);
        assert_eq!(progress.observe(at(50), true, true), Close::Pending);
        assert_eq!(progress.observe(at(79), true, true), Close::Pending);
        assert_eq!(progress.observe(at(80), true, true), Close::Kept);
    }

    #[test]
    fn destruction_wins_even_before_a_reply_or_at_the_last_check() {
        for replied in [true, false] {
            let mut progress = CloseProgress::default();
            assert_eq!(
                progress.observe(Duration::ZERO, replied, true),
                Close::Pending
            );
            assert_eq!(
                progress.observe(AFTER_REPLY, replied, false),
                Close::Destroyed
            );
        }
    }
}
