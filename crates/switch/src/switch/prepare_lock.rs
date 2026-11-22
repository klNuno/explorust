use std::time::Duration;

pub fn wait<T, E>(
    mut time_left: impl FnMut() -> Option<Duration>,
    mut try_lock: impl FnMut(Duration) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    while let Some(left) = time_left().filter(|left| !left.is_zero()) {
        if let Some(lock) = try_lock(left.min(Duration::from_millis(100)))? {
            return Ok(Some(lock));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_busy_recovery_lock_can_be_released_during_preparation() {
        let mut attempts = 0;
        let lock = wait(
            || Some(Duration::from_secs(1)),
            |slice| {
                assert_eq!(slice, Duration::from_millis(100));
                attempts += 1;
                Ok::<_, ()>((attempts == 2).then_some("lock"))
            },
        );
        assert_eq!(lock, Ok(Some("lock")));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn checks_cancellation_or_deadline_between_attempts() {
        let busy = Cell::new(false);
        let lock = wait(
            || Some(Duration::from_millis(20)).filter(|_| !busy.get()),
            |slice| {
                assert_eq!(slice, Duration::from_millis(20));
                busy.set(true);
                Ok::<Option<()>, ()>(None)
            },
        );
        assert_eq!(lock, Ok(None));
    }
}
