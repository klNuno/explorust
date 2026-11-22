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
    pub repost: &'a mut dyn FnMut(usize) -> Result<bool, String>,
}

pub struct RestoredDesktop {
    pub description: String,
    pub original_retiring: bool,
}

pub fn restore_desktop(
    installed_shell: &Path,
    exit: Option<ExplorerExit<'_>>,
) -> Result<RestoredDesktop, String> {
    recover(installed_shell, exit, false)
}

pub fn ensure_desktop(installed_shell: &Path) -> Result<RestoredDesktop, String> {
    recover(installed_shell, None, true)
}

pub fn show_sign_out_notice() {
    let _ = std::thread::Builder::new().name("desktop-recovery-notice".into()).spawn(|| {
        use crate::ui::{self, Icon, tr};
        ui::tell(
            Icon::Warning,
            tr("La barre des tâches de Windows ne répond pas", "The Windows taskbar is not responding"),
            tr("Fermez la session et rouvrez-la pour retrouver le bureau.",
                "Sign out and sign in again to restore the desktop."),
            None,
        );
    });
}

fn recover(
    installed_shell: &Path,
    mut exit: Option<ExplorerExit<'_>>,
    accept_explorust: bool,
) -> Result<RestoredDesktop, String> {
    let started = Instant::now();
    let mut recovery = ExplorerRecovery::default();
    let mut requested = false;
    let mut launch: Option<Child> = None;
    let mut stopping_launcher = false;
    let mut observed: Option<Child> = None;
    let mut last_error = None;
    let mut orphan: Option<(u32, Instant)> = None;
    let mut watching_original = false;
    let mut original_missing_since = None;
    let mut termination_attempts = 0;
    let mut last_termination: Option<Instant> = None;
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
        let file_operations = taskbar::explorer_file_operations();
        let tray_gone_grace_elapsed = if retiring.is_some()
            && original_tray == OriginalTray::Missing && !file_operations
        {
            original_missing_since.get_or_insert_with(Instant::now).elapsed() >= RETIRING_EXPLORER_GRACE
                && last_termination.is_none_or(|last| last.elapsed() >= Duration::from_secs(2))
        } else {
            original_missing_since = None;
            false
        };
        let available = (retiring.is_none())
            .then(|| Event::open_for_signal(supervise::FALLBACK_EVENT).ok()).flatten();
        let desktop_owner = taskbar::desktop_owner();
        let explorer_owner = desktop_owner.filter(|&pid| Some(pid) != retiring && taskbar::is_explorer(pid));
        let explorust_owner = desktop_owner.filter(|&pid| {
            taskbar::image_path(pid).is_some_and(|path| path.eq_ignore_ascii_case(&installed_shell.to_string_lossy()))
        });
        let tray = explorer_owner.map_or_else(
            || if desktop_owner.is_none() { RecoveryTray::Missing } else { RecoveryTray::Other },
            RecoveryTray::Explorer,
        );
        if observed.as_ref().is_some_and(Child::ended) || observed.as_ref().map(|child| child.pid) != explorer_owner {
            observed = explorer_owner.and_then(|pid| Child::monitor(pid).ok());
        }
        let responsive = |owner: Option<u32>| {
            owner.filter(|&pid| taskbar::tray_for(pid).is_some_and(|window| {
                taskbar::answers(window, Duration::from_millis(500))
                    && taskbar::desktop_owner() == Some(pid)
                    && taskbar::tray_for(pid) == Some(window)
                    && !taskbar::explorer_file_operations()
            }))
        };
        match explorust_owner {
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
            explorer_ready: responsive(explorer_owner),
            explorer_owner,
            explorust_ready: if accept_explorust { responsive(explorust_owner) } else { None },
            recovery_expired: started.elapsed() >= policy::DESKTOP_RECOVERY_LIMIT,
            retirement_attempts: termination_attempts,
            supervisor_available: available.is_some(),
            explorust_owner,
            adoption_elapsed: orphan.is_some_and(|(_, since)| since.elapsed() >= Duration::from_secs(3)),
        });
        match decision {
            DesktopAction::NeedsSignOut => {
                return Err(last_error.unwrap_or_else(|| "the desktop did not return; sign out and sign in again".into()));
            }
            DesktopAction::WaitForCopy => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
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
                    termination_attempts += 1;
                    last_termination = Some(Instant::now());
                    if let Err(error) = stop_retiring_explorer(exit) {
                        log::line(&format!("restore: {error}"));
                        last_error = Some(error);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::Ready { pid, retiring: original_retiring } => {
                if retiring.is_some_and(taskbar::owns_tray) {
                    continue;
                }
                let description = if explorust_owner == Some(pid) {
                    format!("explorust's taskbar answers (pid {pid})")
                } else if launch.is_some() {
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
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::StopOrphan(pid) => {
                // An adopting guard may have arrived since the snapshot. Verify the same
                // installed desktop owner after opening its process and before stopping it.
                if available.is_none() {
                    match Child::open(pid) {
                        Ok(child) if taskbar::desktop_owner() == Some(pid) && !taskbar::explorer_file_operations()
                            && Event::open_for_signal(supervise::FALLBACK_EVENT).is_err() => {
                            log::line(&format!("restore: stopping orphaned explorust desktop pid {pid}"));
                            child.kill();
                        }
                        Err(e) => log::line(&format!("restore: could not open orphaned explorust: {e}")),
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::WaitForAdoption => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            DesktopAction::Restore => {}
        }

        match recovery.step(started.elapsed(), tray, false, launch.as_ref().is_none_or(Child::ended)) {
            Recovery::Exhausted => {
                return Err(last_error.unwrap_or_else(|| "Explorer recovery ended without an answering taskbar".into()));
            }
            Recovery::Restart(pid) => {
                if let Some(child) = observed.as_ref().filter(|child| child.pid == pid)
                    && let Err(error) = stop_hung_replacement(child)
                {
                    log::line(&format!("restore: {error}"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Recovery::StopLauncher => {
                if let Some(child) = &launch
                    && !stopping_launcher
                    && taskbar::desktop_owner().is_none()
                    && !taskbar::explorer_file_operations()
                    && !child.ended()
                {
                    // This is the process we launched with no arguments on a vacant desktop.
                    // Never start again until its process handle signals that it has exited.
                    match unsafe { TerminateProcess(child.handle, 1) } {
                        Ok(()) => {
                            stopping_launcher = true;
                            log::line(&format!("restore: stopped Explorer launcher pid {} before it registered a desktop", child.pid));
                        }
                        Err(e) => {
                            last_error = Some(format!("could not stop Explorer launcher pid {}: {e}", child.pid));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Recovery::Start if taskbar::desktop_owner().is_none() && !taskbar::explorer_file_operations() => {
                if install::elevated() {
                    return Err("no shell runs: start explorer.exe from Task Manager".into());
                }
                if let Some(child) = &launch {
                    if !child.ended() {
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    log::line(&format!("restore: retrying after Explorer pid {} did not produce an answering taskbar", child.pid));
                }
                if taskbar::desktop_owner().is_some() || taskbar::explorer_file_operations() {
                    continue;
                }
                stopping_launcher = false;
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

fn stop_retiring_explorer(exit: &ExplorerExit<'_>) -> Result<(), String> {
    let pid = exit.process.pid;
    let child = Child::open(pid)?;
    if taskbar::desktop_owner().is_some_and(|owner| owner != pid)
        || taskbar::explorer_file_operations()
        || taskbar::owns_tray(pid)
        || exit.process.ended()
    {
        return Ok(());
    }
    // SAFETY: the original handle still identifies the same live process as the terminate
    // handle. Its exit may have been posted, its taskbar is gone, and no file operation is active.
    unsafe { TerminateProcess(child.handle, 1) }
        .map_err(|e| format!("could not stop retiring Explorer pid {pid}: {e}"))?;
    log::line(&format!("restore: stopped retiring Explorer pid {pid} after its taskbar disappeared"));
    wait_any(&[exit.process.handle], Some(1_000));
    Ok(())
}

fn stop_hung_replacement(process: &Child) -> Result<(), String> {
    let pid = process.pid;
    let window = taskbar::explorer_window_for(pid);
    let child = Child::open(pid)?;
    if window.is_some_and(|hwnd| taskbar::answers(hwnd, policy::ANSWER_WITHIN))
        || taskbar::explorer_file_operations()
        || !taskbar::is_explorer(pid)
        || taskbar::desktop_owner() != Some(pid)
        || taskbar::explorer_window_for(pid) != window
        || process.ended()
    {
        return Ok(());
    }
    // SAFETY: the saved process is still the verified desktop owner. Its tray is absent or
    // unresponsive and no Explorer file operation is active.
    unsafe { TerminateProcess(child.handle, 1) }
        .map_err(|e| format!("could not stop unresponsive replacement Explorer pid {pid}: {e}"))?;
    log::line(&format!("restore: stopped unresponsive replacement Explorer pid {pid}"));
    wait_any(&[process.handle], Some(1_000));
    Ok(())
}
