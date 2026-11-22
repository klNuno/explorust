//! Hand the current desktop from Explorer to explorust without ending the logon session.

mod explorer;
mod reopen_list;

use crate::launch_timing::{self, FALLBACK_EVENT, build_verdict_event};
use crate::proc::{Child, Event};
use crate::ui::{self, Icon, tr};
use crate::{GUARD_MUTEX, account, install, log, manage, policy, reg, reopen, supervise, taskbar};
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::core::HSTRING;

pub fn run() -> u32 {
    let dir = install::dir();
    if taskbar::explorust(&dir.join("explorust.exe")).is_some() {
        return show(Ok(Outcome::AlreadyRunning));
    }
    if !account::is_session_user() {
        return show(Err("the switch must run as the signed-in user".into()));
    }
    let guard = dir.join("explorust-guard.exe");
    if install::elevated() {
        return match explorer::launch_switch(&guard) {
            Ok(()) => {
                log::line("switch: Explorer launched the unelevated session switch");
                0
            }
            Err(e) => show(Err(e)),
        };
    }
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
    let guard_pid = std::process::id();
    let stamp = reopen::guard_stamp(guard_pid)
        .ok_or("could not identify this guard for the Files handoff")?;
    loop {
        let Some(folders) = wait_for_folders(tray, pid, cancel)? else {
            log::line("switch: cancelled before closing Explorer; installed for next sign-in");
            return Ok(Outcome::Postponed);
        };
        reopen::save(&log::dir(), stamp, &folders)
            .map_err(|e| format!("save the Files handoff: {e}"))?;
        if let Err(e) = release_old_guard(pid, guard) {
            let _ = reopen::take(&log::dir(), stamp);
            return Err(e);
        }
        // File operations can start while the handoff is being prepared. Check again before
        // disabling Cancel and closing Explorer. Retry the folder snapshot after the operation.
        if !explorer::file_operations() && cancel.commit() {
            log::line(&format!(
                "switch: saved {} Explorer folder windows for guard pid {}",
                folders.len(),
                guard_pid
            ));
            break;
        }
        let _ = reopen::take(&log::dir(), stamp);
    }
    let started = explorer::exit(tray, pid).and_then(|()| {
        thread::Builder::new()
            .name("supervise".into())
            .spawn(|| crate::run_shell(None, true))
            .map_err(|e| format!("start supervision: {e}"))
    });
    let thread = match started {
        Ok(thread) => supervisor.insert(thread),
        Err(e) => {
            let _ = reopen::take(&log::dir(), stamp);
            return recover_windows(&e);
        }
    };
    log::line(&format!(
        "switch: guard pid {guard_pid} took over the session"
    ));
    wait_for_verdict(exe, thread)
}

fn wait_for_folders(
    tray: HWND,
    pid: u32,
    cancel: &ui::Cancel,
) -> Result<Option<Vec<String>>, String> {
    let mut waiting = false;
    while !cancel.requested() {
        if taskbar::explorer_window() != Some((tray, pid)) {
            return Err("Explorer's taskbar changed while waiting".into());
        }
        if !explorer::file_operations() {
            return explorer::open_folders().map(Some);
        }
        if !waiting {
            waiting = true;
            log::line("switch: waiting for Explorer file operations");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(None)
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

fn release_old_guard(explorer: u32, guard: &Path) -> Result<(), String> {
    if !guard_present()? {
        return Ok(());
    }
    if let Ok(request) = Event::open_for_signal(supervise::RELEASE_EXPLORER) {
        request.signal()?;
    } else {
        // Old builds have no release event. Only retire the installed guard that started this
        // Explorer process, after verifying its image and that the PID has not been reused.
        let parent = explorer::processes()?
            .into_iter()
            .find(|process| process.pid == explorer)
            .ok_or("Explorer exited while checking its previous guard")?
            .parent;
        let child = Child::open(parent)?;
        let image =
            taskbar::image_path(parent).ok_or("could not identify Explorer's parent process")?;
        let image = image.to_ascii_lowercase();
        let expected = guard.to_string_lossy().to_ascii_lowercase();
        let is_guard = image == expected
            || image
                .strip_prefix(&format!("{expected}.old"))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                });
        let predates_explorer = reopen::guard_stamp(parent)
            .zip(reopen::guard_stamp(explorer))
            .is_some_and(|(guard, explorer)| guard < explorer);
        if !is_guard || !predates_explorer || child.ended() {
            return Err("the previous guard has no handoff event and was not identified as Explorer's parent".into());
        }
        // SAFETY: the verified old guard will otherwise relaunch Explorer when it exits.
        unsafe { windows::Win32::System::Threading::TerminateProcess(child.handle, 0) }
            .map_err(|e| format!("retire legacy guard pid {parent}: {e}"))?;
        log::line(&format!(
            "switch: retired the legacy Explorer supervisor pid {parent}"
        ));
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

fn wait_for_verdict(exe: &Path, supervisor: &JoinHandle<u32>) -> Result<Outcome, String> {
    let started = Instant::now();
    let limit = launch_timing::NATIVE_BUDGET
        + launch_timing::IMMERSIVE_BUDGET
        + launch_timing::VERDICT_BUDGET
        + Duration::from_secs(30);
    loop {
        if let Some(pid) = taskbar::explorer() {
            log::line(&format!("switch: Windows took over (Explorer pid {pid})"));
            return Ok(Outcome::Windows);
        }
        if supervisor.is_finished() {
            return recover_windows("supervision ended before the build verdict");
        }
        if !Event::is_signalled(FALLBACK_EVENT)
            && let Some(pid) = taskbar::explorust(exe)
            && Event::is_signalled(&build_verdict_event(pid))
            && taskbar::probe(pid) == policy::Probe::Answered
            && !Event::is_signalled(FALLBACK_EVENT)
        {
            log::line(&format!(
                "switch: explorust pid {pid} passed the build verdict and owns an answering taskbar"
            ));
            return Ok(Outcome::Running);
        }
        if started.elapsed() >= limit {
            return recover_windows("the new shell did not pass the build verdict in time");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn recover_windows(problem: &str) -> Result<Outcome, String> {
    log::line(&format!("switch: {problem}"));
    manage::bring_back_explorer().map(|_| Outcome::Windows)
}
