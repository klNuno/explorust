#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Acknowledged,
    Failed,
    Handoff,
}

fn retry(mut attempt: impl FnMut(u32) -> Attempt) -> Attempt {
    for index in 1..=3 {
        match attempt(index) {
            Attempt::Failed => {}
            done => return done,
        }
    }
    Attempt::Failed
}

#[cfg(windows)]
pub use win::spawn;

#[cfg(windows)]
mod win {
    use super::{Attempt, retry};
    use crate::launch_timing::FALLBACK_EVENT;
    use crate::log;
    use crate::proc::{Child, Event, wait_any};
    use std::path::Path;

    pub fn spawn(exe: &Path) -> Result<(), String> {
        let exe = exe.to_owned();
        std::thread::Builder::new()
            .name("startup-claim".into())
            .spawn(move || {
                if let Err(e) = claim(&exe) {
                    log::line(&format!("session startup: {e}"));
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn claim(exe: &Path) -> Result<(), String> {
        // The worker survives reload; the guard keeps the handoff event alive across attempts.
        let fallback = Event::named(FALLBACK_EVENT)?;
        let name = format!(r"Local\explorust-startup-{}", std::process::id());
        let ack = Event::named(&name)?;
        let args = format!("--startup-apps {name}");
        let result = retry(|attempt| {
            // The pause gives a transient process or registry error time to clear.
            match wait_any(
                &[fallback.0, ack.0],
                Some(if attempt == 1 { 0 } else { 300 }),
            ) {
                Some(0) => return Attempt::Handoff,
                Some(_) => return Attempt::Acknowledged,
                None => {}
            }
            match Child::spawn(exe, &args) {
                Ok(runner) => {
                    log::line(&format!(
                        "session startup runner pid {} (attempt {attempt}/3)",
                        runner.pid
                    ));
                    // A runner can exit just after acknowledging an already claimed logon.
                    match wait_any(&[fallback.0, ack.0, runner.handle], Some(3_000)) {
                        Some(0) => Attempt::Handoff,
                        Some(1) => Attempt::Acknowledged,
                        result => {
                            let why = if result.is_some() {
                                format!("exit code {:#x}", runner.exit_code())
                            } else {
                                "3 s without acknowledgment".into()
                            };
                            // Do not kill a runner that may have already claimed the marker.
                            log::line(&format!("session startup claim attempt {attempt}/3: {why}"));
                            Attempt::Failed
                        }
                    }
                }
                Err(e) => {
                    log::line(&format!(
                        "session startup runner launch attempt {attempt}/3: {e}"
                    ));
                    Attempt::Failed
                }
            }
        });
        match result {
            Attempt::Acknowledged => log::line("session startup claim acknowledged"),
            Attempt::Handoff => log::line("session startup stopped for Explorer handoff"),
            Attempt::Failed => return Err("startup claim unacknowledged after 3 attempts".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_a_launch_error_and_an_unacknowledged_process() {
        let mut replies = [Attempt::Failed, Attempt::Failed, Attempt::Acknowledged].into_iter();
        assert_eq!(retry(|_| replies.next().unwrap()), Attempt::Acknowledged);
        assert_eq!(replies.next(), None);
    }

    #[test]
    fn stops_on_handoff_or_after_the_retry_budget() {
        let mut attempts = 0;
        assert_eq!(
            retry(|_| {
                attempts += 1;
                Attempt::Handoff
            }),
            Attempt::Handoff
        );
        assert_eq!(attempts, 1);
        assert_eq!(
            retry(|attempt| {
                attempts = attempt;
                Attempt::Failed
            }),
            Attempt::Failed
        );
        assert_eq!(attempts, 3);
    }
}
