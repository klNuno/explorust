//! Hand the current desktop from Explorer to explorust without ending the logon session.

mod close_progress;
mod explorer;
pub(crate) mod folder_close;
mod prepare_lock;
mod reopen_list;
pub(crate) mod watcher;

use guard::launch_timing::{self, FALLBACK_EVENT, build_verdict_event};
use guard::proc::{Child, Event};
use guard::switch_events::{CANCEL_FOR_RESTORE, RELEASE_EXPLORER, SWITCH_EXE, WATCHES_EXPLORER_HANDOFF};
use guard::ui::{self, Icon, tr};
use guard::{account, explorer_launch, install, log, policy, reg, reopen, taskbar};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const VERDICT_LIMIT: Duration = Duration::from_secs(
    launch_timing::NATIVE_BUDGET.as_secs()
        + launch_timing::IMMERSIVE_BUDGET.as_secs()
        + launch_timing::VERDICT_BUDGET.as_secs()
        + 30,
);
pub fn launch_from_setup() -> Result<(), String> {
    explorer_launch::launch_switch(&install::dir().join(SWITCH_EXE))?;
    log::line("switch: Explorer launched the unelevated session switch");
    Ok(())
}

pub fn run() -> u32 {
    if install::elevated() {
        if taskbar::explorust(&install::dir().join("explorust.exe")).is_some() {
            return show(Ok(Outcome::AlreadyRunning));
        }
        return match launch_from_setup() {
            Ok(()) => 0,
            Err(e) => show(Err(e)),
        };
    }
    run_worker()
}

pub fn run_worker() -> u32 {
    if install::elevated() {
        return show(Err("Explorer launched an elevated switch worker".into()));
    }
    let dir = install::dir();
    if taskbar::explorust(&dir.join("explorust.exe")).is_some() {
        return show(Ok(Outcome::AlreadyRunning));
    }
    if !account::is_session_user() {
        return show(Err("the switch must run as the signed-in user".into()));
    }
    let guard = dir.join("explorust-guard.exe");
    let exe = dir.join("explorust.exe");
    let worker = dir.join(SWITCH_EXE);
    let result = ui::working_cancellable(
        tr("Passage à explorust", "Switching to explorust"),
        tr(
            "explorust attend la fin des opérations de fichiers de l’Explorateur Windows.",
            "explorust waits for File Explorer to finish its file operations.",
        ),
        |cancel| switch(&worker, &guard, &exe, cancel),
    );
    show(result)
}

enum Outcome {
    AlreadyRunning,
    Postponed,
    CancelledForRestore,
    Running,
    Windows,
}

fn show(result: Result<Outcome, String>) -> u32 {
    let details = log::dir().join("guard.log").display().to_string();
    match result {
        Ok(Outcome::CancelledForRestore) => {}
        Ok(Outcome::AlreadyRunning) => ui::tell(
            Icon::Info,
            tr("explorust est déjà lancé", "explorust is already running"),
            tr(
                "Votre session utilise déjà explorust.",
                "This session is already using explorust.",
            ),
            None,
        ),
        Ok(Outcome::Postponed) => ui::tell(
            Icon::Info,
            tr(
                "Passage à explorust reporté",
                "Switch to explorust postponed",
            ),
            tr(
                "explorust reste installé et démarrera à la prochaine ouverture de session.",
                "explorust remains installed and will start at the next sign-in.",
            ),
            None,
        ),
        Ok(Outcome::Running) => ui::tell(
            Icon::Info,
            tr("explorust est lancé", "explorust is running"),
            tr(
                "Votre session utilise maintenant explorust.",
                "This session is now using explorust.",
            ),
            None,
        ),
        Ok(Outcome::Windows) => ui::tell(
            Icon::Warning,
            tr("Windows a repris la main", "Windows took over"),
            tr(
                "Le journal indique pourquoi explorust n’a pas démarré.",
                "See the log for why explorust did not start.",
            ),
            Some(&details),
        ),
        Err(e) => {
            log::line(&format!("switch: {e}"));
            let content = if e == watcher::sign_out_message() {
                tr("La barre des tâches n’est pas revenue. Fermez la session et rouvrez-la.",
                    "The taskbar did not return. Sign out and sign in again.")
            } else {
                tr("Consultez le journal pour en savoir plus.", "See the log for details.")
            };
            ui::tell(
                Icon::Warning,
                tr(
                    "Impossible de passer à explorust",
                    "Could not switch to explorust",
                ),
                content,
                Some(&details),
            );
            return 1;
        }
    }
    0
}

fn switch(
    worker: &Path,
    guard: &Path,
    exe: &Path,
    cancel: &ui::Cancel,
) -> Result<Outcome, String> {
    let lock = install::switch_lock_for(0)?;
    let expected = format!("\"{}\"", guard.display());
    if !reg::shell().is_some_and(|value| value.eq_ignore_ascii_case(&expected)) || !exe.is_file() {
        return Err("explorust is not installed as this user's sign-in shell".into());
    }
    if taskbar::explorust(exe).is_some() {
        return Ok(Outcome::AlreadyRunning);
    }
    let Some((tray, pid)) = taskbar::explorer_window() else {
        return Err("no Explorer taskbar in this session".into());
    };
    let explorer_process = Child::monitor(pid)?;
    let worker_pid = std::process::id();
    let stamp = reopen::guard_stamp(worker_pid)
        .ok_or("could not identify the switch worker for the Files handoff")?;
    let (mut watcher, previous) = loop {
        wait_for_file_operations(tray, pid, cancel)?;
        if cancel_requested(cancel) {
            return Ok(cancelled());
        }
        let Some(watcher) = watcher::Watcher::prepare(worker, stamp, tray, pid, cancel)? else {
            return Ok(cancelled());
        };
        // Just collect the legacy guard's handle. The previous guard still watches Explorer
        // throughout the cancellable preparation, including if a new file operation starts.
        let previous = previous_guard(pid, guard)?;
        if !explorer::file_operations() && !cancel_requested(cancel) {
            break (watcher, previous);
        }
    };
    let started = watcher.vacate_desktop(tray, pid, cancel).and_then(|desktop| {
        match desktop {
            watcher::Desktop::Cancelled => return Ok(cancelled()),
            watcher::Desktop::Postponed => return Ok(postponed()),
            watcher::Desktop::Restored => return Ok(Outcome::Windows),
            watcher::Desktop::Vacated => {}
        }
        release_previous_guard(previous)?;
        if Event::is_signalled(CANCEL_FOR_RESTORE) {
            return Err("turning off explorust before starting the new shell".into());
        }
        let guard_process = Child::spawn(guard, &format!("--live {worker_pid} {stamp}"))?;
        log::line(&format!("switch: guard pid {} took over the session", guard_process.pid));
        wait_for_verdict(exe, &guard_process, watcher.rollback_event())?;
        watcher.accept()?;
        Ok(Outcome::Running)
    });
    match started {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            log::line(&format!("switch: {e}"));
            if let Err(e) = watcher.restore() {
                if watcher.observing() {
                    return Err(e);
                }
                if !watcher.committed() {
                    return Err("the switch stopped while preparing Explorer".into());
                }
                log::line(&format!("switch: {e}; starting a replacement recovery watcher"));
                drop(lock);
                watcher.recover(worker, &explorer_process)?;
            }
            Ok(Outcome::Windows)
        }
    }
}

fn postponed() -> Outcome {
    log::line("switch: postponed before closing Explorer; installed for next sign-in");
    Outcome::Postponed
}

fn cancel_requested(cancel: &ui::Cancel) -> bool {
    cancel.requested() || Event::is_signalled(CANCEL_FOR_RESTORE)
}

fn cancelled() -> Outcome {
    if Event::is_signalled(CANCEL_FOR_RESTORE) {
        log::line("switch: cancelled because explorust is being turned off");
        Outcome::CancelledForRestore
    } else {
        postponed()
    }
}

fn wait_for_file_operations(tray: HWND, pid: u32, cancel: &ui::Cancel) -> Result<(), String> {
    let mut waiting = false;
    while !cancel_requested(cancel) {
        if taskbar::explorer_window() != Some((tray, pid)) {
            return Err("Explorer's taskbar changed while waiting".into());
        }
        if !explorer::file_operations() {
            return Ok(());
        }
        if !waiting {
            waiting = true;
            log::line("switch: waiting for Explorer file operations");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

enum PreviousGuard {
    WatchingHandoff,
    Release(Event),
    Legacy(Child),
}

fn previous_guard(explorer: u32, guard: &Path) -> Result<Option<PreviousGuard>, String> {
    if !install::guard_running()? {
        return Ok(None);
    }
    if Event::open_for_signal(WATCHES_EXPLORER_HANDOFF).is_ok() {
        return Ok(Some(PreviousGuard::WatchingHandoff));
    }
    if let Ok(request) = Event::open_for_signal(RELEASE_EXPLORER) {
        return Ok(Some(PreviousGuard::Release(request)));
    }
    // Old builds have no handoff event. Keep a handle to the verified Explorer parent while it
    // still exists. This handle identifies the process even if its PID is reused.
    let parent = guard::processes::in_session("explorer.exe")?
        .into_iter()
        .find(|process| process.pid == explorer)
        .ok_or("Explorer exited while checking its previous guard")?
        .parent;
    let child = Child::open(parent)?;
    let image = taskbar::image_path(parent)
        .ok_or("could not identify Explorer's parent process")?
        .to_ascii_lowercase();
    let expected = guard.to_string_lossy().to_ascii_lowercase();
    let is_guard = image == expected
        || image
            .strip_prefix(&format!("{expected}.old"))
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
    let predates_explorer = reopen::guard_stamp(parent)
        .zip(reopen::guard_stamp(explorer))
        .is_some_and(|(guard, explorer)| guard < explorer);
    if !is_guard || !predates_explorer || child.ended() {
        return Err("the previous guard has no handoff event and was not identified as Explorer's parent".into());
    }
    Ok(Some(PreviousGuard::Legacy(child)))
}

fn release_previous_guard(previous: Option<PreviousGuard>) -> Result<(), String> {
    match previous {
        Some(PreviousGuard::Release(request)) => request.signal()?,
        Some(PreviousGuard::Legacy(child)) if !child.ended() => {
            // SAFETY: this is the verified legacy guard that would otherwise relaunch Explorer.
            unsafe { windows::Win32::System::Threading::TerminateProcess(child.handle, 0) }
                .map_err(|e| format!("retire legacy guard pid {}: {e}", child.pid))?;
            log::line(&format!("switch: retired the legacy Explorer supervisor pid {}", child.pid));
        }
        _ => {}
    }
    let started = Instant::now();
    while install::guard_running()? {
        if started.elapsed() > Duration::from_secs(5) {
            return Err("the previous guard did not release Explorer within 5 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn release_retiring_guard(explorer: u32, guard: &Path) -> Result<(), String> {
    let previous = match previous_guard(explorer, guard)? {
        Some(PreviousGuard::WatchingHandoff) => Some(PreviousGuard::Release(Event::open_for_signal(RELEASE_EXPLORER)?)),
        other => other,
    };
    release_previous_guard(previous)
}

fn wait_for_verdict(exe: &Path, supervisor: &Child, rollback: &Event) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if Event::is_signalled(CANCEL_FOR_RESTORE) {
            return Err("turning off explorust during the switch".into());
        }
        if let Some((_, pid)) = taskbar::explorer_window() {
            let _ = Event::set_existing(FALLBACK_EVENT);
            return Err(format!("Explorer pid {pid} took back the taskbar"));
        }
        if supervisor.ended() || rollback.signalled() {
            return Err("supervision ended before the build verdict".into());
        }
        if !Event::is_signalled(FALLBACK_EVENT)
            && let Some(pid) = taskbar::explorust(exe)
            && Event::is_signalled(&build_verdict_event(pid))
            && taskbar::probe(pid) == policy::Probe::Answered
            && !Event::is_signalled(FALLBACK_EVENT)
            && taskbar::explorer_window().is_none()
        {
            log::line(&format!(
                "switch: explorust pid {pid} passed the build verdict and owns an answering taskbar"
            ));
            return Ok(());
        }
        if started.elapsed() >= VERDICT_LIMIT {
            return Err("the new shell did not pass the build verdict in time".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
