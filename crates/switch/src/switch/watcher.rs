//! Keep an independent process alive from Explorer preparation through the build verdict.

use super::{
    explorer, folder_close, prepare_lock,
    reopen_list::{self, Folder},
};
use guard::desktop_recovery::{ExplorerExit, restore_desktop};
use guard::policy::{ANSWER_WITHIN, ExitState};
use guard::proc::{Child, Event, wait_any};
use guard::switch_events::{CANCEL_FOR_RESTORE, EXITING_EXPLORER, SWITCH_EXE, switch_event};
use guard::{account, install, log, reopen, taskbar, ui};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::INFINITE;

const PREPARE_BUDGET: Duration = Duration::from_secs(15);
const RESTORED: u32 = 2;
const POSTPONED: u32 = 3;
const FAILED: u32 = 1;

pub(super) struct Signals {
    prepared: Event,
    prepare_exit: Event,
    close_ready: Event,
    exit: Event,
    post_desktop: Event,
    exit_intent: Event,
    exit_cancelled: Event,
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
            prepare_exit: get("prepare-exit")?,
            close_ready: get("close-ready")?,
            exit: get("exit")?,
            post_desktop: get("post-desktop")?,
            exit_intent: get("exit-intent")?,
            exit_cancelled: get("exit-cancelled")?,
            exiting: get("exiting")?,
            exited: get("exited")?,
            accepted: get("accepted")?,
            completed: get("completed")?,
            rollback: get("rollback")?,
            windows_ready: get("windows-ready")?,
        })
    }

    fn desktop_exit(&self) -> ExitState {
        ExitState::from_signals(self.exit_intent.signalled(), self.exiting.signalled(), self.exit_cancelled.signalled())
    }
}

pub enum Desktop {
    Cancelled,
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

    pub fn vacate_desktop(&mut self, tray: HWND, pid: u32, cancel: &ui::Cancel) -> Result<Desktop, String> {
        let exiting = Event::named(EXITING_EXPLORER)?;
        self.signals.prepare_exit.signal()?;
        let prepare_started = Instant::now();
        loop {
            if super::cancel_requested(cancel) {
                return Ok(Desktop::Cancelled);
            }
            match wait_any(&[self.child.handle, self.signals.close_ready.0], Some(100)) {
                Some(0) if self.child.exit_code() == POSTPONED => return Ok(Desktop::Postponed),
                Some(0) => return Err("the watcher stopped before it was ready to close folders".into()),
                Some(1) if !super::cancel_requested(cancel) && cancel.commit() => break,
                Some(1) => return Ok(Desktop::Cancelled),
                _ if prepare_started.elapsed() >= PREPARE_BUDGET + Duration::from_secs(1) => {
                    return Ok(Desktop::Postponed);
                }
                _ => {}
            }
        }
        self.signals.exit.signal()?;
        self.committed = true;
        let limit = explorer::CLOSE_BUDGET + Duration::from_secs(5);
        let status = wait_any(&[self.child.handle, self.signals.post_desktop.0], Some(limit.as_millis() as u32));
        if status == Some(1) {
            // The worker posts the desktop message. If the watcher is killed here, the
            // surviving worker still knows whether its own PostMessage call succeeded.
            let result = explorer::post_exit(tray, pid, || {
                if self.child.ended() || self.signals.rollback.signalled() || Event::is_signalled(CANCEL_FOR_RESTORE) {
                    return Err("the switch ended before Exit Explorer".into());
                }
                exiting.signal()?;
                self.signals.exit_intent.signal()
            });
            if result == Ok(true) {
                self.signals.exiting.signal()?;
                log::line(&format!("switch: worker posted Exit Explorer for pid {pid}"));
            } else {
                self.signals.exit_cancelled.signal()?;
                if let Err(error) = &result {
                    log::line(&format!("switch: {error}"));
                }
            }
            match wait_any(&[self.signals.exited.0, self.child.handle, self.signals.windows_ready.0], None) {
                Some(0) if !self.child.ended() => return Ok(Desktop::Vacated),
                Some(2) if result == Ok(true) => return Ok(Desktop::Restored),
                Some(2) => { wait_any(&[self.child.handle], None); }
                _ => {}
            }
        }
        if !self.child.ended() {
            return Err("the independent switch watcher did not report Explorer's exit".into());
        }
        match self.child.exit_code() {
            POSTPONED => Ok(Desktop::Postponed),
            RESTORED => Ok(Desktop::Restored),
            _ => Err("the independent switch watcher could not close Explorer".into()),
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
        let timeout = (!self.committed).then_some(1_000);
        let status = wait_any(&[self.signals.windows_ready.0, self.child.handle], timeout);
        if status == Some(0) || self.child.exit_code() == RESTORED {
            Ok(())
        } else {
            Err("the independent switch watcher could not restore Windows".into())
        }
    }

    pub fn committed(&self) -> bool {
        self.committed
    }

    pub fn recover(&mut self, exe: &Path, original: &Child) -> Result<Option<Vec<Folder>>, String> {
        let _lock = install::recovery_lock_for(INFINITE)?;
        let folders = read_snapshot(self.stamp).unwrap_or_else(|_| std::mem::take(&mut self.folders));
        let track = recover(self.stamp, &self.signals, exe, original, &folders)?;
        Ok(track.then_some(folders))
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
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.prepare_exit.0], None) != Some(2) {
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
        let Some(recovery) = prepare_lock::wait(
            || (!abandoned()).then(|| PREPARE_BUDGET.saturating_sub(refresh_started.elapsed())),
            |slice| install::try_recovery_lock_for(slice.as_millis() as u32),
        )? else {
            continue;
        };
        if abandoned() || explorer::file_operations() {
            continue;
        }
        if reopen_list::same_windows(&folders, explorer::folder_windows().into_iter()) {
            break recovery;
        }
        drop(recovery);
        log::line("switch: Explorer folder windows changed; refreshing the handoff");
        folders = capture_folders(&snapshot, refresh_started).inspect_err(|_| {
            let _ = std::fs::remove_file(&snapshot);
        })?;
    };
    signals.close_ready.signal()?;
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.exit.0], None) != Some(2) {
        clear_snapshot(stamp);
        let _ = std::fs::remove_file(&snapshot);
        return Ok(0);
    }
    let started = (|| -> Result<bool, String> {
        let Some(earlier_receipts) = close_folders(&folders, explorer_pid, owner, stamp, abandoned)? else {
            return Ok(false);
        };
        if abandoned() {
            return Err("the switch ended before Exit Explorer".into());
        }
        let paths = reopen_list::handoff_paths(&folders, &earlier_receipts);
        reopen::save(&log::dir(), stamp, &paths).map_err(|e| format!("save Files handoff: {e}"))?;
        signals.post_desktop.signal()?;
        let events = [signals.exiting.0, signals.exit_cancelled.0, owner_process.handle, signals.rollback.0];
        match wait_any(&events, None) {
            Some(0) => {
                log::line(&format!("switch: requested Exit Explorer for pid {explorer_pid}"));
                let started = Instant::now();
                wait_for_original_exit(&retiring, &owner_process, &signals)?;
                explorer::wait_for_desktop_windows()?;
                log::line(&format!("switch: Explorer desktop exited after {} ms", started.elapsed().as_millis()));
                Ok(true)
            }
            Some(1) => Ok(false),
            _ => Err("the switch worker did not confirm the desktop exit request".into()),
        }
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
    let owner_lost = owner_process.ended();
    let track = recover(stamp, &signals, &installed.join("explorust.exe"), &retiring, &folders)?;
    drop(exiting);
    drop(recovery);
    if track {
        restore_late_folders(&retiring, &folders)?;
    }
    if started == Ok(false) {
        return Ok(POSTPONED);
    }
    if owner_lost {
        super::show(Ok(super::Outcome::Windows));
    }
    Ok(RESTORED)
}

fn wait_for_original_exit(original: &Child, owner: &Child, signals: &Signals) -> Result<(), String> {
    let events = [original.handle, owner.handle, signals.rollback.0];
    let mut missing_since = None;
    loop {
        if Event::is_signalled(CANCEL_FOR_RESTORE) {
            return Err("turning off explorust while Explorer's exit is pending".into());
        }
        match wait_any(&events, Some(200)) {
            Some(0) => return Ok(()),
            Some(_) => return Err("the switch ended while Explorer's exit was pending".into()),
            None => {}
        }
        if taskbar::owns_tray(original.pid) {
            missing_since = None;
        } else if missing_since.get_or_insert_with(Instant::now).elapsed() >= ANSWER_WITHIN {
            return Err("Explorer lost its taskbar and did not finish exiting".into());
        }
    }
}

fn close_folders(
    folders: &[Folder],
    desktop: u32,
    owner: u32,
    stamp: u64,
    abandoned: impl Fn() -> bool,
) -> Result<Option<HashSet<(isize, u32)>>, String> {
    let started = Instant::now();
    let mut earlier_receipts = HashSet::new();
    for folder in folders.iter().filter(|folder| folder.owner != desktop) {
        if abandoned() {
            return Err("the switch ended while closing Explorer folders".into());
        }
        if explorer::file_operations() {
            log::line("switch: a file operation started during folder closing");
            return Ok(None);
        }
        let budget = explorer::CLOSE_BUDGET.saturating_sub(started.elapsed()).min(Duration::from_secs(2));
        let handoff = if budget.is_zero() {
            folder_close::Handoff::Pending
        } else {
            folder_close::close(owner, stamp, folder, budget)?
        };
        match handoff {
            folder_close::Handoff::Snapshot => {}
            folder_close::Handoff::EarlierReceipt => {
                earlier_receipts.insert((folder.hwnd, folder.owner));
            }
            folder_close::Handoff::Pending => {
                log::line(&format!("switch: Explorer folder in pid {} did not complete the close request", folder.owner));
                return Ok(None);
            }
        }
    }
    // Another window may have opened while the outgoing desktop processed the closes.
    let original: Vec<_> = folders.iter().filter(|folder| folder.owner == desktop).map(|folder| (folder.hwnd, folder.owner)).collect();
    Ok(explorer::folder_windows().iter().all(|window| original.contains(window)).then_some(earlier_receipts))
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

fn recover(
    stamp: u64,
    signals: &Signals,
    exe: &Path,
    original: &Child,
    folders: &[Folder],
) -> Result<bool, String> {
    clear_snapshot(stamp);
    if signals.windows_ready.signalled() {
        return Ok(false);
    }
    let state = signals.desktop_exit();
    let exiting = Event::named(EXITING_EXPLORER)?;
    if state.pending() {
        exiting.signal()?;
    }
    let mut repost = |window| {
        if original.ended() {
            return Ok(false);
        }
        let tray = HWND(window as *mut _);
        if explorer::post_exit(tray, original.pid, || exiting.signal())? {
            log::line(&format!("switch: resolved the unconfirmed Exit Explorer for pid {}", original.pid));
            signals.exiting.signal()?;
            Ok(true)
        } else {
            Ok(false)
        }
    };
    let restored = restore_desktop(exe, Some(ExplorerExit {
        state,
        process: original,
        repost: &mut repost,
    }))?;
    log::line(&format!("switch: restored Windows: {}", restored.description));
    let track = restored.original_retiring && !original.ended();
    if track {
        let guard = exe.with_file_name("explorust-guard.exe");
        if let Err(error) = super::release_retiring_guard(original.pid, &guard) {
            log::line(&format!("switch: could not release the retiring Explorer supervisor: {error}"));
        }
    }
    if !track && original.ended() {
        explorer::restore_desktop_folders(folders, original.pid);
    }
    signals.windows_ready.signal()?;
    let _ = std::fs::remove_file(snapshot_path(stamp));
    Ok(track)
}

pub(super) fn restore_late_folders(original: &Child, folders: &[Folder]) -> Result<(), String> {
    log::line(&format!("switch: Windows is back while Explorer pid {} is still exiting", original.pid));
    wait_any(&[original.handle], None);
    let _recovery = install::recovery_lock_for(INFINITE)?;
    for folder in folders.iter().filter(|folder| folder.owner == original.pid) {
        if let Err(error) = folder_close::reopen_in_current_shell(&folder.path) {
            log::line(&format!("switch: {error}"));
        }
    }
    Ok(())
}
