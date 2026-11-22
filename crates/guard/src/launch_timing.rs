//! Startup budgets shared by the guard and the shell. Each phase gets its own clock.

use std::io::{self, Read};
use std::time::Duration;

pub const FALLBACK_EVENT: &str = r"Local\explorust-guard-fallback";
pub const GRAPHICS_LOST: u32 = 7;
pub const RELOAD_EVENT: &str = r"Local\explorust-reload";

pub fn reload_preserves_files_event(pid: u32) -> String {
    format!(r"Local\explorust-reload-preserves-files-{pid}")
}

pub fn reload_postponed_event(pid: u32) -> String {
    format!(r"Local\explorust-reload-postponed-{pid}")
}

/// The build verdict addressed by shell PID, so setup need not know the guard's launch attempt.
pub fn build_verdict_event(pid: u32) -> String {
    format!(r"Local\explorust-build-verified-{pid}")
}
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

/// Pre-stage shell versions send a single readiness signal after the whole startup.
pub fn legacy_budget(self_test: bool) -> Duration {
    Duration::from_secs(if self_test { 120 } else { 45 })
}

pub fn staged_event(ready: &str) -> String {
    format!("{ready}-staged")
}

pub fn native_event(ready: &str) -> String {
    format!("{ready}-native")
}

pub fn immersive_event(ready: &str) -> String {
    format!("{ready}-immersive")
}

pub const STARTUP_MARKER: &str = "RunStuffHasBeenRun";

pub fn startup_session_key(session: u32) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}")
}

// The old guard already verified this build. Its single ready event starts its
// taskbar watchdog, while the shell still enforces its immersive deadline.
#[allow(dead_code)]
pub fn legacy_ready_at_native(guarded: bool, staged: bool, self_test: bool) -> bool {
    guarded && !staged && !self_test
}

pub const STAGED_FLAG: &str = "--staged-ready";

/// The explicit-only releases had neither version metadata nor a capability query.
/// Identify those builds by their phase diagnostics, which survive release stripping.
/// The CLI flag itself can compile into several separate machine instructions.
pub fn needs_staged_flag(mut image: impl Read) -> io::Result<bool> {
    const PHASE: &[u8] = b"immersive ready";
    const OFFER: &[u8] = b"staged readiness negotiation";
    let mut buffer = [0u8; 32 * 1024];
    let mut phased = false;
    let mut kept = 0;
    loop {
        let bytes = image.read(&mut buffer[kept..])?;
        if bytes == 0 {
            return Ok(phased);
        }
        let end = bytes + kept;
        let chunk = &buffer[..end];
        if chunk.windows(OFFER.len()).any(|bytes| bytes == OFFER) {
            return Ok(false);
        }
        phased |= chunk.windows(PHASE.len()).any(|bytes| bytes == PHASE);
        kept = end.min(OFFER.len() - 1);
        buffer.copy_within(end - kept..end, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pre_stage_guards_on_a_verified_build_get_the_native_ready_signal() {
        assert!(legacy_ready_at_native(true, false, false));
        assert!(!legacy_ready_at_native(true, false, true));
        assert!(!legacy_ready_at_native(true, true, false));
        assert!(!legacy_ready_at_native(false, false, false));
    }

    #[test]
    fn distinguishes_explicit_only_images_even_across_reads() {
        let mut image = vec![0; 32 * 1024 - 4];
        image.extend_from_slice(b"immersive read");
        assert!(!needs_staged_flag(image.as_slice()).unwrap());
        image.extend_from_slice(b"y");
        assert!(needs_staged_flag(image.as_slice()).unwrap());
        image.extend_from_slice(b"staged readiness negotiation");
        assert!(!needs_staged_flag(image.as_slice()).unwrap());
    }
}
