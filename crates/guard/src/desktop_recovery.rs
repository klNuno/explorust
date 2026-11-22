//! Apply the same desktop recovery policy from uninstall and from either switch process.

use crate::policy::{self, DesktopAction, DesktopFacts, ExitState, ExplorerRecovery, OriginalTray, Recovery, RecoveryTray};
use crate::proc::{Child, Event, wait_any};
use crate::{install, log, supervise, taskbar};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::System::Threading::TerminateProcess;

const RETIRING_EXPLORER_GRACE: Duration = Duration::from_secs(10);

pub struct ExplorerExit<'a> {
    pub process: &'a Child,
    pub state: ExitState,
    pub file_operations: fn() -> bool,
    pub repost: &'a mut dyn FnMut(usize) -> Result<bool, String>,
}

pub struct RestoredDesktop {
    pub description: String,
    pub original_retiring: bool,
}

pub fn restore_desktop(
    installed_shell: &Path,
    mut exit: Option<ExplorerExit<'_>>,
) -> Result<RestoredDesktop, String> {
    let started = Instant::now();
    let keep_trying = exit.is_some();
    let mut recovery = ExplorerRecovery::default();
    let mut requested = false;
    let mut launch: Option<Child> = None;
    let mut last_error = None;
    let mut orphan: Option<(u32, Instant)> = None;
    let mut watching_original = false;
    let mut original_missing_since = None;
    loop {
        let state = exit.as_ref().map_or(ExitState::Unsent, |exit| exit.state);
        let original = exit.as_ref().filter(|exit| !exit.process.ended());
        let retiring = original.filter(|_| state.pending()).map(|exit| exit.process.pid);
        let original_tray = match retiring.and_then(taskbar::tray_for) {
            Some(window) if state == ExitState::Uncertain && taskbar::answers(window, policy::ANSWER_WITHIN) => {
                OriginalTray::Answering(window.0 as usize)
            }
            Some(_) => OriginalTray::Present,
            None => OriginalTray::Missing,
        };
        let file_operations = original.is_some_and(|exit| (exit.file_operations)());
        let tray_gone_grace_elapsed = if retiring.is_some()
            && state == ExitState::Posted && original_tray == OriginalTray::Missing && !file_operations
        {
            original_missing_since.get_or_insert_with(Instant::now).elapsed() >= RETIRING_EXPLORER_GRACE
        } else {
            original_missing_since = None;
            false
        };
        let available = (retiring.is_none())
            .then(|| Event::open_for_signal(supervise::FALLBACK_EVENT).ok()).flatten();
        let shell_window_owner = taskbar::explorust_shell_window(installed_shell);
        let shell_tray_owner = taskbar::explorust(installed_shell);
        match shell_window_owner.or(shell_tray_owner) {
            Some(pid) if available.is_none() => {
                if orphan.is_none_or(|(owner, _)| owner != pid) {
                    orphan = Some((pid, Instant::now()));
                }
            }
            _ => orphan = None,
        }
        let decision = policy::recover_desktop(DesktopFacts {
            exit: state,
            original_alive: original.is_some(),
            original_tray,
            tray_gone_grace_elapsed,
            file_operations,
            explorer_ready: taskbar::explorer_except(retiring),
            supervisor_available: available.is_some(),
            shell_window_owner,
            shell_tray_owner,
            adoption_elapsed: orphan.is_some_and(|(_, since)| since.elapsed() >= Duration::from_secs(3)),
        });
        match decision {
            DesktopAction::WaitForOriginal => {
                if let Some(exit) = original {
                    if !watching_original {
                        log::line(&format!("switch: waiting for Explorer pid {} to finish its pending exit", exit.process.pid));
                        watching_original = true;
                    }
                    wait_any(&[exit.process.handle], Some(200));
                }
                continue;
            }
            DesktopAction::RepostExit(window) => {
                if let Some(exit) = &mut exit {
                    match (exit.repost)(window) {
                        Ok(true) => exit.state = ExitState::Posted,
                        Ok(false) => {}
                        Err(error) => log::line(&format!("switch: could not resolve the unconfirmed exit: {error}")),
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::StopRetiringExplorer => {
                if let Some(exit) = original {
                    match stop_retiring_explorer(exit) {
                        Ok(true) => recovery = ExplorerRecovery::default(),
                        Ok(false) => {}
                        Err(error) => log::line(&format!("restore: {error}")),
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::Ready { pid, retiring: original_retiring } => {
                if retiring.is_some_and(taskbar::owns_tray) {
                    continue;
                }
                let description = if launch.is_some() {
                    format!("replacement Explorer taskbar answers (pid {pid})")
                } else if requested {
                    format!("the guard brought explorer back (pid {pid})")
                } else {
                    format!("explorer's taskbar already answers (pid {pid})")
                };
                return Ok(RestoredDesktop { description, original_retiring });
            }
            DesktopAction::RequestFallback => {
                requested |= available.is_some_and(|event| event.signal().is_ok());
            }
            DesktopAction::StopOrphan(pid) => {
                // An adopting guard may have arrived since the snapshot. Verify the same
                // installed desktop owner after opening its process and before stopping it.
                if available.is_none() {
                    match Child::open(pid) {
                        Ok(child) if recovery_shell_owner(installed_shell) == Some(pid)
                            && Event::open_for_signal(supervise::FALLBACK_EVENT).is_err() => {
                            log::line(&format!("restore: stopping orphaned explorust desktop pid {pid}"));
                            child.kill();
                        }
                        Err(e) => log::line(&format!("restore: could not open orphaned explorust: {e}")),
                        _ => {}
                    }
                }
            }
            DesktopAction::WaitForAdoption | DesktopAction::Restore => {}
        }

        let launched = launch.as_ref().map(|child| child.pid);
        let tray = taskbar::recovery_tray(launched);
        let blocked = decision != DesktopAction::Restore;
        match recovery.step(started.elapsed(), tray, blocked, launch.as_ref().is_none_or(Child::ended)) {
            Recovery::Exhausted if keep_trying => {
                if !blocked && tray != RecoveryTray::Other {
                    // Keep the launcher handle so the next attempt can stop its hung process.
                    recovery = ExplorerRecovery::default();
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Recovery::Exhausted if matches!(decision, DesktopAction::WaitForAdoption | DesktopAction::StopOrphan(_)) => {
                // A guard can vanish at the end of a launch window. Complete the adoption
                // grace and re-evaluate the resulting desktop before exhausting the budget.
                recovery = ExplorerRecovery::default();
                std::thread::sleep(Duration::from_secs(1));
            }
            Recovery::Exhausted => {
                return Err(last_error.unwrap_or_else(|| "Explorer recovery ended without an answering taskbar".into()));
            }
            Recovery::Start if taskbar::recovery_tray(launched) != RecoveryTray::Other => {
                if install::elevated() {
                    return Err("no shell runs: start explorer.exe from Task Manager".into());
                }
                if let Some(child) = &launch {
                    if !child.ended() {
                        if taskbar::explorer_except(retiring).is_some()
                            || taskbar::recovery_tray(launched) == RecoveryTray::Other
                        {
                            continue;
                        }
                        log::line(&format!("restore: stopping unresponsive recovery Explorer pid {}", child.pid));
                        child.kill();
                    }
                    log::line(&format!("restore: retrying after Explorer pid {} did not produce an answering taskbar", child.pid));
                }
                if taskbar::any() || recovery_shell_owner(installed_shell).is_some() {
                    continue;
                }
                launch = match Child::spawn(&supervise::explorer_path(), "") {
                    Ok(child) => {
                        last_error = None;
                        log::line(&format!("restore: started Explorer pid {} and waiting for its taskbar", child.pid));
                        Some(child)
                    }
                    Err(e) => {
                        let error = format!("explorer did not start: {e}");
                        log::line(&format!("restore: {error}"));
                        last_error = Some(error);
                        None
                    }
                };
                recovery.started(started.elapsed());
            }
            Recovery::Start | Recovery::Wait => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn recovery_shell_owner(exe: &Path) -> Option<u32> {
    taskbar::explorust_shell_window(exe).or_else(|| taskbar::explorust(exe))
}

fn stop_retiring_explorer(exit: &ExplorerExit<'_>) -> Result<bool, String> {
    let pid = exit.process.pid;
    let child = Child::open(pid)?;
    if taskbar::explorer_except(Some(pid)).is_some()
        || (exit.file_operations)()
        || taskbar::owns_tray(pid)
        || exit.process.ended()
    {
        return Ok(false);
    }
    // SAFETY: the original handle still identifies the same live process as the terminate
    // handle. Its exit was posted, its taskbar is gone, and no file operation is active.
    unsafe { TerminateProcess(child.handle, 1) }
        .map_err(|e| format!("could not stop retiring Explorer pid {pid}: {e}"))?;
    log::line(&format!("restore: stopped retiring Explorer pid {pid} after its taskbar disappeared"));
    wait_any(&[exit.process.handle], Some(1_000));
    Ok(true)
}
