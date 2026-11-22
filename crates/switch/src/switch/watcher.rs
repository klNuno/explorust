//! Keep an independent process alive from Explorer preparation through the build verdict.

use super::{explorer, folder_close, reopen_list::{self, Folder}};
use guard::launch_timing::FALLBACK_EVENT;
use guard::proc::{Child, Event, wait_any};
use guard::switch_events::{CANCEL_FOR_RESTORE, EXITING_EXPLORER, SWITCH_EXE, switch_event};
use guard::{account, bring_back_explorer_except, install, log, policy::RECOVERY_BUDGET, reopen, taskbar, ui};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const PREPARE_BUDGET: Duration = Duration::from_secs(15);
const RESTORED: u32 = 2;
const POSTPONED: u32 = 3;
const FAILED: u32 = 1;

pub(super) struct Signals {
    prepared: Event,
    exit: Event,
    exiting: Event,
    exited: Event,
    accepted: Event,
    pub(super) completed: Event,
    pub(super) rollback: Event,
    pub(super) windows_ready: Event,
}

impl Signals {
    pub(super) fn open(stamp: u64) -> Result<Self, String> {
        let get = |name| Event::named(&switch_event(stamp, name));
        Ok(Self {
            prepared: get("prepared")?,
            exit: get("exit")?,
            exiting: get("exiting")?,
            exited: get("exited")?,
            accepted: get("accepted")?,
            completed: get("completed")?,
            rollback: get("rollback")?,
            windows_ready: get("windows-ready")?,
        })
    }
}

pub enum Desktop {
    Vacated,
    Restored,
    Postponed,
}

pub struct Watcher {
    child: Child,
    signals: Signals,
    stamp: u64,
    committed: bool,
    folders: Vec<Folder>,
}

impl Watcher {
    pub fn prepare(
        worker: &Path,
        stamp: u64,
        tray: HWND,
        explorer_pid: u32,
        cancel: &ui::Cancel,
    ) -> Result<Option<Self>, String> {
        let signals = Signals::open(stamp)?;
        let args = format!(
            "--watch-switch {} {stamp} {} {explorer_pid}",
            std::process::id(),
            tray.0 as usize,
        );
        let mut watcher = Self {
            child: Child::spawn(worker, &args)?,
            signals,
            stamp,
            committed: false,
            folders: Vec::new(),
        };
        let start = Instant::now();
        while !super::cancel_requested(cancel) {
            match wait_any(&[watcher.signals.prepared.0, watcher.child.handle], Some(100)) {
                Some(0) if !watcher.child.ended() => {
                    watcher.folders = read_snapshot(stamp)?;
                    return Ok(Some(watcher));
                }
                Some(_) => return Err("Explorer folder preparation failed".into()),
                None if start.elapsed() >= PREPARE_BUDGET => {
                    return Err("Explorer did not return its folder windows within 15 s".into());
                }
                None => {}
            }
        }
        // Drop stops a hung COM client. Only a committed watcher may close Explorer.
        Ok(None)
    }

    pub fn vacate_desktop(&mut self) -> Result<Desktop, String> {
        self.signals.exit.signal()?;
        self.committed = true;
        let limit = PREPARE_BUDGET + explorer::EXIT_BUDGET * 2 + Duration::from_secs(5);
        match wait_any(&[self.signals.exited.0, self.child.handle], Some(limit.as_millis() as u32)) {
            Some(0) => Ok(Desktop::Vacated),
            Some(_) if self.child.exit_code() == POSTPONED => Ok(Desktop::Postponed),
            Some(_) if self.child.exit_code() == RESTORED => Ok(Desktop::Restored),
            Some(_) => Err("the independent switch watcher could not close Explorer".into()),
            None => Err("the independent switch watcher did not report Explorer's exit".into()),
        }
    }

    pub fn accept(&self) -> Result<(), String> {
        self.signals.accepted.signal()?;
        if wait_any(&[self.signals.completed.0, self.child.handle], Some(5_000)) == Some(0) {
            Ok(())
        } else {
            Err("the independent switch watcher did not confirm the takeover".into())
        }
    }

    pub fn restore(&self) -> Result<(), String> {
        self.signals.rollback.signal()?;
        let limit = explorer::EXIT_BUDGET + RECOVERY_BUDGET + Duration::from_secs(5);
        if wait_any(&[self.child.handle], Some(limit.as_millis() as u32)).is_none() {
            self.child.kill();
            return Err("the independent switch watcher timed out restoring Windows".into());
        }
        if self.child.exit_code() == RESTORED {
            Ok(())
        } else {
            Err("the independent switch watcher could not restore Windows".into())
        }
    }

    pub fn committed(&self) -> bool {
        self.committed
    }

    pub fn recover(&self, exe: &Path, explorer: &Child) -> Result<(), String> {
        let _lock = install::recovery_lock_for(0)?;
        let refreshed = read_snapshot(self.stamp);
        recover(self.stamp, &self.signals, exe, explorer, refreshed.as_deref().unwrap_or(&self.folders))
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.signals.rollback.signal();
            if !self.child.ended() && wait_any(&[self.child.handle], Some(200)).is_none() {
                self.child.kill();
            }
        }
        clear_snapshot(self.stamp);
        let snapshot = snapshot_path(self.stamp);
        let _ = std::fs::remove_file(snapshot.with_extension("tmp"));
        let _ = std::fs::remove_file(snapshot);
        let _ = std::fs::remove_file(log::dir().join(format!("reopen-{}.tmp", self.child.pid)));
    }
}

pub fn run(arguments: impl Iterator<Item = String>) -> u32 {
    match watch(arguments) {
        Ok(code) => code,
        Err(e) => {
            log::line(&format!("switch watcher: {e}"));
            FAILED
        }
    }
}

fn watch(arguments: impl Iterator<Item = String>) -> Result<u32, String> {
    let arguments: Vec<_> = arguments.collect();
    let [owner, stamp, tray, explorer_pid] = arguments.as_slice() else {
        return Err("missing switch watcher arguments".into());
    };
    let owner = owner.parse::<u32>().map_err(|_| "invalid switch owner PID")?;
    let stamp = stamp.parse::<u64>().map_err(|_| "invalid switch owner stamp")?;
    let tray = tray.parse::<usize>().map_err(|_| "invalid Explorer window handle")?;
    let explorer_pid = explorer_pid.parse::<u32>().map_err(|_| "invalid Explorer PID")?;
    let tray = HWND(tray as *mut _);
    let installed = install::dir();
    let owner_process = Child::open(owner)?;
    if install::elevated()
        || !account::is_session_user()
        || reopen::guard_stamp(owner) != Some(stamp)
        || !taskbar::image_path(owner).is_some_and(|image| {
            image.eq_ignore_ascii_case(&installed.join(SWITCH_EXE).to_string_lossy())
        })
    {
        return Err("the switch watcher has no valid unelevated switch owner".into());
    }
    if taskbar::explorer_window() != Some((tray, explorer_pid)) {
        return Err("Explorer changed before the switch was prepared".into());
    }
    let signals = Signals::open(stamp)?;
    let retiring = Child::monitor(explorer_pid)?;
    log::line(&format!("switch: watcher reading Explorer folders for switch pid {owner}"));
    let snapshot = snapshot_path(stamp);
    let mut folders = capture_folders(&snapshot, Instant::now())?;
    log::line(&format!(
        "switch: watcher ready for switch pid {owner}, saved {} Explorer folder windows",
        folders.len()
    ));
    signals.prepared.signal()?;
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.exit.0], None) != Some(2) {
        clear_snapshot(stamp);
        let _ = std::fs::remove_file(snapshot);
        return Ok(0);
    }
    let exiting = Event::named(EXITING_EXPLORER)?;
    let refresh_started = Instant::now();
    let abandoned = || {
        owner_process.ended() || signals.rollback.signalled() || Event::is_signalled(CANCEL_FOR_RESTORE)
    };
    let recovery = loop {
        if abandoned() {
            clear_snapshot(stamp);
            let _ = std::fs::remove_file(&snapshot);
            return Ok(0);
        }
        // A file operation may have appeared after the owner's final check.
        if explorer::file_operations() || refresh_started.elapsed() >= PREPARE_BUDGET {
            log::line("switch: Explorer changed during the final exit preparation; postponed");
            clear_snapshot(stamp);
            let _ = std::fs::remove_file(&snapshot);
            return Ok(POSTPONED);
        }
        // Uninstall can wait for the close and rollback, including after our owner dies.
        // Release the lock before any further COM capture.
        let recovery = install::recovery_lock_for(0)?;
        if reopen_list::same_windows(&folders, explorer::folder_windows().into_iter()) {
            break recovery;
        }
        drop(recovery);
        log::line("switch: Explorer folder windows changed; refreshing the handoff");
        folders = capture_folders(&snapshot, refresh_started).inspect_err(|_| {
            let _ = std::fs::remove_file(&snapshot);
        })?;
    };
    let started = (|| {
        let paths: Vec<_> = folders.iter().map(|folder| folder.path.clone()).collect();
        reopen::save(&log::dir(), stamp, &paths).map_err(|e| format!("save Files handoff: {e}"))?;
        if !close_folders(&folders, explorer_pid, owner, stamp, abandoned)? {
            return Ok(false);
        }
        explorer::exit_desktop(tray, explorer_pid, || {
            if abandoned() {
                return Err("the switch ended before Exit Explorer".into());
            }
            exiting.signal()
        }, || signals.exiting.signal())
    })();
    if matches!(started, Ok(true)) && signals.exited.signal().is_ok() {
        match wait_any(
            &[signals.accepted.0, signals.rollback.0, owner_process.handle],
            Some(super::VERDICT_LIMIT.as_millis() as u32 + 15_000),
        ) {
            Some(0) => {
                signals.completed.signal()?;
                let _ = std::fs::remove_file(&snapshot);
                return Ok(0);
            }
            Some(1) => log::line("switch: watcher received a rollback request"),
            Some(2) => log::line("switch: watcher saw the switch worker exit before takeover"),
            _ => {
                log::line("switch: watcher timed out waiting for the switch worker");
                owner_process.kill();
            }
        }
    }
    if let Err(e) = &started {
        log::line(&format!("switch: {e}"));
    } else if started == Ok(false) {
        log::line("switch: Explorer changed during folder closing; postponed");
    }
    signals.rollback.signal()?;
    drop(exiting);
    recover(stamp, &signals, &installed.join("explorust.exe"), &retiring, &folders)?;
    drop(recovery);
    if started == Ok(false) {
        return Ok(POSTPONED);
    }
    if owner_process.ended() {
        super::show(Ok(super::Outcome::Windows));
    }
    Ok(RESTORED)
}

fn close_folders(folders: &[Folder], desktop: u32, owner: u32, stamp: u64, abandoned: impl Fn() -> bool) -> Result<bool, String> {
    let started = Instant::now();
    for folder in folders.iter().filter(|folder| folder.owner != desktop) {
        if abandoned() {
            return Err("the switch ended while closing Explorer folders".into());
        }
        if explorer::file_operations() {
            log::line("switch: a file operation started during folder closing");
            return Ok(false);
        }
        let budget = explorer::EXIT_BUDGET.saturating_sub(started.elapsed()).min(Duration::from_secs(2));
        if budget.is_zero() || !folder_close::close(owner, stamp, folder, budget)? {
            log::line(&format!("switch: Explorer folder in pid {} did not complete the close request", folder.owner));
            return Ok(false);
        }
    }
    // Another window may have opened while the outgoing desktop processed the closes.
    let original: Vec<_> = folders.iter().filter(|folder| folder.owner == desktop).map(|folder| (folder.hwnd, folder.owner)).collect();
    Ok(explorer::folder_windows().iter().all(|window| original.contains(window)))
}

fn capture_folders(snapshot: &Path, started: Instant) -> Result<Vec<Folder>, String> {
    let folders = loop {
        if let Some(folders) = explorer::open_folders()? {
            break folders;
        }
        if started.elapsed() >= PREPARE_BUDGET {
            return Err("Explorer folder windows did not settle before the switch".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let temp = snapshot.with_extension("tmp");
    let result = std::fs::write(&temp, reopen_list::encode(&folders))
        .and_then(|()| std::fs::rename(&temp, snapshot));
    if let Err(error) = result {
        let _ = std::fs::remove_file(temp);
        return Err(format!("save Explorer recovery snapshot: {error}"));
    }
    Ok(folders)
}

fn clear_snapshot(stamp: u64) {
    let _ = reopen::take(&log::dir(), stamp);
}

fn snapshot_path(stamp: u64) -> PathBuf {
    log::dir().join(format!("switch-{stamp}.folders"))
}

pub(super) fn read_snapshot(stamp: u64) -> Result<Vec<Folder>, String> {
    std::fs::read_to_string(snapshot_path(stamp))
        .and_then(|contents| reopen_list::decode(&contents))
        .map_err(|e| format!("read Explorer recovery snapshot: {e}"))
}

fn recover(stamp: u64, signals: &Signals, exe: &Path, original: &Child, folders: &[Folder]) -> Result<(), String> {
    clear_snapshot(stamp);
    if !signals.windows_ready.signalled() {
        let retiring = signals.exiting.signalled().then_some(original);
        restore_desktop(exe, retiring)?;
        if retiring.is_some() || original.ended() {
            explorer::restore_desktop_folders(folders, original.pid);
        }
        signals.windows_ready.signal()?;
    }
    let _ = std::fs::remove_file(snapshot_path(stamp));
    Ok(())
}

fn restore_desktop(exe: &Path, retiring: Option<&Child>) -> Result<(), String> {
    // A running shell can adopt a new guard after the original guard dies. Give it time to
    // offer the normal fallback event before closing an orphaned switch shell ourselves.
    let installed = |pid| {
        taskbar::image_path(pid)
            .is_some_and(|image| image.eq_ignore_ascii_case(&exe.to_string_lossy()))
    };
    for process in explorer::processes("explorust.exe")? {
        let pid = process.pid;
        if !installed(pid) {
            continue;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while installed(pid) {
            if Event::set_existing(FALLBACK_EVENT).is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                match Child::open(pid) {
                    Ok(child) if installed(pid) => child.kill(),
                    Err(e) if installed(pid) => log::line(&format!("switch: open orphaned shell: {e}")),
                    _ => {}
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let status = bring_back_explorer_except(retiring)?;
    log::line(&format!("switch: restored Windows: {status}"));
    Ok(())
}
