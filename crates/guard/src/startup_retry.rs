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

fn run_on_ready(requests: impl IntoIterator<Item = ()>, mut claim: impl FnMut() -> Attempt) {
    let mut acknowledged = false;
    for () in requests {
        if acknowledged {
            continue;
        }
        match claim() {
            Attempt::Acknowledged => acknowledged = true,
            Attempt::Failed => {}
            Attempt::Handoff => return,
        }
    }
}

#[cfg(windows)]
pub use win::SessionStartup;

#[cfg(windows)]
mod win {
    use super::{Attempt, retry, run_on_ready};
    use crate::launch_timing::{FALLBACK_EVENT, STARTUP_MARKER, startup_session_key};
    use crate::log;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc::{self, SyncSender, TrySendError};
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};
    use windows::core::HSTRING;

    #[derive(Default)]
    pub struct SessionStartup(Option<SyncSender<()>>);

    impl SessionStartup {
        pub fn desktop_ready(&mut self, exe: &Path) {
            if let Some(request) = &self.0
                && !matches!(request.try_send(()), Err(TrySendError::Disconnected(())))
            {
                return;
            }
            // A reload during a burst leaves one pending request. The worker services it only
            // if the in-flight burst fails, and it idles without polling between reloads.
            let (tx, rx) = mpsc::sync_channel(1);
            let exe = exe.to_owned();
            let _ = tx.try_send(());
            let worker = std::thread::Builder::new()
                .name("startup-claim".into())
                .spawn(move || {
                    run_on_ready(rx, || match claim(&exe) {
                        Ok(outcome) => outcome,
                        Err(e) => {
                            log::line(&format!("session startup claim unacknowledged: {e}. Will retry on reload."));
                            Attempt::Failed
                        }
                    });
                });
            self.0 = match worker {
                Ok(_) => Some(tx),
                Err(e) => {
                    log::line(&format!("could not start the session startup worker: {e}. Will retry on reload."));
                    None
                }
            };
        }
    }

    fn claim(exe: &Path) -> Result<Attempt, String> {
        let fallback = event(FALLBACK_EVENT)?;
        let name = format!(r"Local\explorust-startup-{}", std::process::id());
        let ack = event(&name)?;
        let events = [HANDLE(fallback.as_raw_handle()), HANDLE(ack.as_raw_handle())];
        let result = retry(|attempt| {
            // The pause gives a transient process or registry error time to clear.
            match wait(&events, if attempt == 1 { 0 } else { 300 }) {
                Some(0) => return Attempt::Handoff,
                Some(_) => return Attempt::Acknowledged,
                None if marker_exists() => return Attempt::Acknowledged,
                None => {}
            }
            let mut command = Command::new(exe);
            command.args(["--startup-apps", &name]);
            if let Some(dir) = exe.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                command.current_dir(dir);
            }
            match command.spawn() {
                Ok(mut runner) => {
                    log::line(&format!(
                        "session startup runner pid {} (attempt {attempt}/3)",
                        runner.id()
                    ));
                    // A runner can exit just after acknowledging an already claimed logon.
                    match wait(&[events[0], events[1], HANDLE(runner.as_raw_handle())], 3_000) {
                        Some(0) => Attempt::Handoff,
                        Some(1) => Attempt::Acknowledged,
                        // The explicit-only shell claims the same key but predates the ack event.
                        _ if marker_exists() => Attempt::Acknowledged,
                        result => {
                            let why = match result {
                                Some(_) => match runner.wait() {
                                    Ok(status) => status.to_string(),
                                    Err(e) => format!("could not read the runner's exit status: {e}"),
                                },
                                None => "3 s without acknowledgment".into(),
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
            Attempt::Failed => return Err("3 attempts exhausted".into()),
        }
        Ok(result)
    }

    fn event(name: &str) -> Result<OwnedHandle, String> {
        // SAFETY: create a manual-reset event and take ownership of the returned handle.
        unsafe { CreateEventW(None, true, false, &HSTRING::from(name)) }
            .map(|h| unsafe { OwnedHandle::from_raw_handle(h.0) })
            .map_err(|e| format!("CreateEvent {name}: {e}"))
    }

    fn wait(handles: &[HANDLE], millis: u32) -> Option<usize> {
        // SAFETY: callers retain each handle's owner for the wait.
        let result = unsafe { WaitForMultipleObjects(handles, false, millis) };
        let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        (index < handles.len()).then_some(index)
    }

    fn marker_exists() -> bool {
        let mut session = 0;
        let mut key = HKEY::default();
        // SAFETY: outputs and the NUL-terminated registry path are valid for the calls.
        unsafe {
            if ProcessIdToSessionId(std::process::id(), &mut session).is_err() {
                return false;
            }
            let path = HSTRING::from(format!(r"{}\{STARTUP_MARKER}", startup_session_key(session)));
            if RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_QUERY_VALUE, &mut key).ok().is_err() {
                return false;
            }
            let _ = RegCloseKey(key);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_rearms_an_exhausted_burst_without_repeating_an_acknowledged_claim() {
        let mut outcomes = [Attempt::Failed, Attempt::Acknowledged].into_iter();
        run_on_ready([(), (), ()], || outcomes.next().expect("unexpected duplicate burst"));
        assert_eq!(outcomes.next(), None);
    }

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
