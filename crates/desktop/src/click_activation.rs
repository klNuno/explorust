use std::sync::atomic::{AtomicU64, Ordering};

const ACTIVATION_WINDOW_MS: u64 = 1_000;

#[derive(Default)]
pub(super) struct ClickActivation {
    expires_at: AtomicU64,
}

impl ClickActivation {
    pub(super) fn record_down(&self, on_wallpaper: bool, now: u64) {
        let expires_at = if on_wallpaper {
            now + ACTIVATION_WINDOW_MS
        } else {
            0
        };
        self.expires_at.store(expires_at, Ordering::Relaxed);
    }

    pub(super) fn suppresses_activation(&self, now: u64) -> bool {
        now < self.expires_at.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::ClickActivation;

    #[test]
    fn wallpaper_down_protects_delayed_activation_until_it_expires() {
        let activation = ClickActivation::default();
        assert!(!activation.suppresses_activation(100));
        activation.record_down(true, 100);
        assert!(activation.suppresses_activation(100));
        assert!(activation.suppresses_activation(1_099));
        assert!(!activation.suppresses_activation(1_100));
    }

    #[test]
    fn later_down_outside_wallpaper_clears_activation_protection() {
        let activation = ClickActivation::default();
        activation.record_down(true, 100);
        activation.record_down(false, 200);
        assert!(!activation.suppresses_activation(200));
    }
}
