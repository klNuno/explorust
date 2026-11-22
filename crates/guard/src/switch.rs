//! Hand the current desktop from Explorer to explorust without ending the logon session.

mod explorer;
mod reopen_list;

use crate::launch_timing::{self, FALLBACK_EVENT, build_verdict_event};
use crate::proc::{Child, Event, SuspendedChild, wait_any};
use crate::ui::{self, Icon, tr};
use crate::{GUARD_MUTEX, account, install, log, manage, policy, prefetch, reg, reopen, supervise, taskbar};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::core::HSTRING;

pub fn run() -> u32 {
    let dir = install::dir();
    if taskbar::explorust(&dir.join("explorust.exe")).is_some() {
        show(Ok(Outcome::AlreadyRunning))
    } else if let Some((_, pid)) = taskbar::explorer_window() {
        // Start the handoff before Explorer exits. The worker and the guard it starts then use
        // Explorer's primary token and environment, even when setup was elevated.
        match prefetch::spawn_as_shell(pid, &dir.join("explorust-guard.exe"), "--switch-session") {
            Ok(worker) => {
                let _ = unsafe { AllowSetForegroundWindow(worker.pid) };
                wait_any(&[worker.handle], None);
                worker.exit_code()
            }
            Err(e) => show(Err(e)),
        }
    } else {
        show(Err("no Explorer taskbar in this session".into()))
    }
}

pub fn session() -> u32 {
    if install::elevated() || !account::is_session_user() {
        return show(Err("the switch must run as the unelevated session user".into()));
    }
    let dir = install::dir();
    let guard = dir.join("explorust-guard.exe");
    let exe = dir.join("explorust.exe");
    let result = ui::working_cancellable(
        tr("Passage à explorust", "Switching to explorust"),
        tr(
            "explorust attend la fin des opérations de fichiers de l’Explorateur Windows.",
            "explorust waits for File Explorer to finish its file operations.",
        ),
        |cancel| switch(&guard, &exe, cancel),
    );
    show(result)
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
            tr("Passage à explorust reporté", "Switch to explorust postponed"),
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
                tr("Impossible de passer à explorust", "Could not switch to explorust"),
                tr("Consultez le journal pour en savoir plus.", "See the log for details."),
                Some(&details),
            );
            return 1;
        }
    }
    0
}

fn switch(guard: &Path, exe: &Path, cancel: &ui::Cancel) -> Result<Outcome, String> {
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
    let (prepared, stamp) = loop {
        let Some(folders) = wait_for_folders(tray, pid, cancel)? else {
            log::line("switch: cancelled before closing Explorer; installed for next sign-in");
            return Ok(Outcome::Postponed);
        };
        let prepared = SuspendedChild::spawn(guard, "--live-switch")?;
        let stamp =
            reopen::guard_stamp(prepared.pid()).ok_or("could not identify the new guard for the Files handoff")?;
        reopen::save(&log::dir(), stamp, &folders).map_err(|e| format!("save the Files handoff: {e}"))?;
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
                prepared.pid()
            ));
            break (prepared, stamp);
        }
        let _ = reopen::take(&log::dir(), stamp);
    };
    let result = explorer::exit(tray, pid).and_then(|()| prepared.start());
    let guard = match result {
        Ok(guard) => guard,
        Err(e) => {
            let _ = reopen::take(&log::dir(), stamp);
            return recover_windows(&e);
        }
    };
    log::line(&format!(
        "switch: started guard pid {} with the Explorer session token",
        guard.pid
    ));
    wait_for_verdict(exe, &guard)
}

fn wait_for_folders(tray: HWND, pid: u32, cancel: &ui::Cancel) -> Result<Option<Vec<String>>, String> {
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
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(GUARD_MUTEX)) } {
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
        let image = taskbar::image_path(parent).ok_or("could not identify Explorer's parent process")?;
        let image = image.to_ascii_lowercase();
        let expected = guard.to_string_lossy().to_ascii_lowercase();
        let is_guard = image == expected
            || image
                .strip_prefix(&format!("{expected}.old"))
                .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()));
        let predates_explorer = reopen::guard_stamp(parent)
            .zip(reopen::guard_stamp(explorer))
            .is_some_and(|(guard, explorer)| guard < explorer);
        if !is_guard || !predates_explorer || child.ended() {
            return Err("the previous guard has no handoff event and was not identified as Explorer's parent".into());
        }
        // SAFETY: the verified old guard will otherwise relaunch Explorer when it exits.
        unsafe { windows::Win32::System::Threading::TerminateProcess(child.handle, 0) }
            .map_err(|e| format!("retire legacy guard pid {parent}: {e}"))?;
        log::line(&format!("switch: retired the legacy Explorer supervisor pid {parent}"));
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

fn wait_for_verdict(exe: &Path, guard: &Child) -> Result<Outcome, String> {
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
        if guard.ended() {
            return recover_windows(&format!(
                "new guard exited before the build verdict ({:#x})",
                guard.exit_code()
            ));
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
