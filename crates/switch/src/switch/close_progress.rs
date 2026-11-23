use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Close {
    Pending,
    Destroyed,
    Abandoned,
}

/// A reply only acknowledges the message. Explorer may destroy the HWND much later.
pub fn close_state(submitted: bool, present: bool) -> Close {
    match (submitted, present) {
        (false, _) => Close::Abandoned,
        (true, true) => Close::Pending,
        (true, false) => Close::Destroyed,
    }
}

/// Allow a responsive folder more time to finish its close within the handoff's budget.
pub fn close_wait(budget: Duration, elapsed: Duration, responsive: bool) -> Duration {
    let limit = if responsive { Duration::from_secs(6) } else { Duration::from_secs(2) };
    budget.min(limit).saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_close_can_finish_more_than_thirty_seconds_after_reply() {
        // Explorer replies at second 1, the switch rolls back at second 2, and the folder
        // finishes closing at second 61. The receipt survives every intermediate probe.
        for second in 0..=61 {
            let expected = if second < 61 { Close::Pending } else { Close::Destroyed };
            assert_eq!(close_state(true, second < 61), expected, "at second {second}");
        }
    }

    #[test]
    fn a_rejected_close_cannot_reopen_a_window_closed_independently() {
        assert_eq!(close_state(false, true), Close::Abandoned);
        assert_eq!(close_state(false, false), Close::Abandoned);
    }

    #[test]
    fn a_responding_folder_can_finish_just_after_the_initial_wait() {
        let budget = Duration::from_secs(30);
        let closing = Duration::from_millis(2_262);
        assert!(!close_wait(budget, closing, true).is_zero());
        assert!(close_wait(budget, closing, false).is_zero());
        assert!(close_wait(Duration::from_secs(2), closing, true).is_zero());
        assert!(close_wait(budget, Duration::from_secs(6), true).is_zero());
    }
}
