//! Hand the current desktop from Explorer to explorust without ending the logon session.

mod explorer;
mod reopen_list;
pub(crate) mod watcher;

use crate::launch_timing::{self, FALLBACK_EVENT, build_verdict_event};
use crate::proc::{Child, Event, wait_any};
use crate::ui::{self, Icon, tr};
use crate::{GUARD_MUTEX, account, install, log, manage, policy, reg, reopen, supervise, taskbar};
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::core::HSTRING;

const VERDICT_LIMIT: Duration = Duration::from_secs(
    launch_timing::NATIVE_BUDGET.as_secs()
        + launch_timing::IMMERSIVE_BUDGET.as_secs()
        + launch_timing::VERDICT_BUDGET.as_secs()
        + 30,
);

pub use explorer::can_launch_switch;

pub fn launch_from_setup() -> Result<(), String> {
    explorer::launch_switch(&install::dir().join("explorust-guard.exe"))?;
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
    let mut supervisor = None;
    let result = ui::working_cancellable(
        tr("Passage à explorust", "Switching to explorust"),
        tr(
            "explorust attend la fin des opérations de fichiers de l’Explorateur Windows.",
            "explorust waits for File Explorer to finish its file operations.",
        ),
        |cancel| switch(&guard, &exe, cancel, &mut supervisor),
    );
    let code = show(result);
    supervisor.map_or(code, |thread| thread.join().unwrap_or(1))
}

enum Outcome {
    AlreadyRunning,
    Postponed,
    Running,
    Windows,
}

fn show(result: Result<Outcome, String>) -> u32 {
    let details = log::dir().join("guard.log").display().to_string();
    match result {
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
            ui::tell(
                Icon::Warning,
                tr(
                    "Impossible de passer à explorust",
                    "Could not switch to explorust",
                ),
                tr(
                    "Consultez le journal pour en savoir plus.",
                    "See the log for details.",
                ),
                Some(&details),
            );
            return 1;
        }
    }
    0
}

fn switch(
    guard: &Path,
    exe: &Path,
    cancel: &ui::Cancel,
    supervisor: &mut Option<JoinHandle<u32>>,
) -> Result<Outcome, String> {
    let _lock = install::lock()?;
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
    let guard_pid = std::process::id();
    let stamp = reopen::guard_stamp(guard_pid)
        .ok_or("could not identify this guard for the Files handoff")?;
    let (mut watcher, previous) = loop {
        wait_for_file_operations(tray, pid, cancel)?;
        if cancel.requested() {
            return Ok(postponed());
        }
        let Some(watcher) = watcher::Watcher::prepare(guard, stamp, tray, pid, cancel)? else {
            return Ok(postponed());
        };
        // Just collect the legacy guard's handle. The previous guard still watches Explorer
        // throughout the cancellable preparation, including if a new file operation starts.
        let previous = previous_guard(pid, guard)?;
        if !explorer::file_operations() && cancel.commit() {
            break (watcher, previous);
        }
    };
    let started = watcher.vacate_desktop().and_then(|desktop| {
        match desktop {
            watcher::Desktop::Postponed => return Ok(postponed()),
            watcher::Desktop::Restored => return Ok(Outcome::Windows),
            watcher::Desktop::Vacated => {}
        }
        release_previous_guard(previous)?;
        thread::Builder::new()
            .name("supervise".into())
            .spawn(|| crate::run_shell(None, true))
            .map_err(|e| format!("start supervision: {e}"))
            .and_then(|thread| {
                log::line(&format!("switch: guard pid {guard_pid} took over the session"));
                wait_for_verdict(exe, supervisor.insert(thread))?;
                watcher.accept()?;
                Ok(Outcome::Running)
            })
    });
    match started {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            log::line(&format!("switch: {e}"));
            if let Err(e) = watcher.restore() {
                log::line(&format!("switch: {e}; restoring Windows directly"));
                if watcher.exit_requested() {
                    // Exit Explorer may still be in flight. Wait before checking whether the
                    // taskbar answers, so a retiring Explorer does not look like a recovery.
                    let _ = wait_any(
                        &[explorer_process.handle],
                        Some(explorer::EXIT_BUDGET.as_millis() as u32),
                    );
                }
                let status = manage::bring_back_explorer()?;
                log::line(&format!("switch: guard restored Windows: {status}"));
            }
            Ok(Outcome::Windows)
        }
    }
}

fn postponed() -> Outcome {
    log::line("switch: postponed before closing Explorer; installed for next sign-in");
    Outcome::Postponed
}

fn wait_for_file_operations(tray: HWND, pid: u32, cancel: &ui::Cancel) -> Result<(), String> {
    let mut waiting = false;
    while !cancel.requested() {
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

fn guard_present() -> Result<bool, String> {
    // SAFETY: the open handle is closed immediately. The running guard keeps its own handle.
    match unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            &HSTRING::from(GUARD_MUTEX),
        )
    } {
        Ok(handle) => {
            let _ = unsafe { CloseHandle(handle) };
            Ok(true)
        }
        Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => Ok(false),
        Err(e) => Err(format!("query the previous guard: {e}")),
    }
}

enum PreviousGuard {
    WatchingHandoff,
    Release(Event),
    Legacy(Child),
}

fn previous_guard(explorer: u32, guard: &Path) -> Result<Option<PreviousGuard>, String> {
    if !guard_present()? {
        return Ok(None);
    }
    if Event::open_for_signal(supervise::WATCHES_EXPLORER_HANDOFF).is_ok() {
        return Ok(Some(PreviousGuard::WatchingHandoff));
    }
    if let Ok(request) = Event::open_for_signal(supervise::RELEASE_EXPLORER) {
        return Ok(Some(PreviousGuard::Release(request)));
    }
    // Old builds have no handoff event. Keep a handle to the verified Explorer parent while it
    // still exists. This handle identifies the process even if its PID is reused.
    let parent = explorer::processes("explorer.exe")?
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
    while guard_present()? {
        if started.elapsed() > Duration::from_secs(5) {
            return Err("the previous guard did not release Explorer within 5 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn wait_for_verdict(exe: &Path, supervisor: &JoinHandle<u32>) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if let Some((_, pid)) = taskbar::explorer_window() {
            let _ = Event::set_existing(FALLBACK_EVENT);
            return Err(format!("Explorer pid {pid} took back the taskbar"));
        }
        if supervisor.is_finished() {
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
