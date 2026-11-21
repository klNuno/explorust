//! Startup budgets shared by the guard and the shell. Each phase gets its own clock.

use std::time::Duration;

pub const FALLBACK_EVENT: &str = r"Local\explorust-guard-fallback";
pub const REGISTRATION: Duration = Duration::from_secs(20);
pub const PARTS: Duration = Duration::from_secs(20);
pub const HOST: Duration = Duration::from_secs(100);
pub const SELF_TEST: Duration = Duration::from_secs(50);
const HANDOFF: Duration = Duration::from_secs(5);

pub const NATIVE_BUDGET: Duration =
    Duration::from_secs(REGISTRATION.as_secs() + PARTS.as_secs() + HANDOFF.as_secs());
pub const IMMERSIVE_BUDGET: Duration =
    Duration::from_secs(HOST.as_secs() + PARTS.as_secs() + HANDOFF.as_secs());
pub const VERDICT_BUDGET: Duration = Duration::from_secs(SELF_TEST.as_secs() + HANDOFF.as_secs());

pub fn native_event(ready: &str) -> String {
    format!("{ready}-native")
}

pub fn immersive_event(ready: &str) -> String {
    format!("{ready}-immersive")
}
