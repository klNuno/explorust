//! Keep an independent process alive from Explorer preparation through the build verdict.

use super::{
    explorer, folder_close, prepare_lock,
    reopen_list::{self, Folder},
};
use guard::desktop_recovery::{ExplorerExit, ensure_desktop, restore_desktop, show_sign_out_notice};
use guard::policy::{self, ANSWER_WITHIN, DESKTOP_RECOVERY_LIMIT, DesktopAction, ExitState};
use guard::proc::{Child, Event, wait_any};
use guard::switch_events::{CANCEL_FOR_RESTORE, EXITING_EXPLORER, SWITCH_EXE, switch_event};
use guard::{account, install, log, reopen, taskbar, ui};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const PREPARE_BUDGET: Duration = Duration::from_secs(15);
const RESTORED: u32 = 2;
const POSTPONED: u32 = 3;
const FAILED: u32 = 1;
const RECOVERY_WAIT_MS: u32 = (DESKTOP_RECOVERY_LIMIT.as_millis() as u32 + 15_000) * 2;

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
    stalled: Event,
    postponed: Event,
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
            stalled: get("recovery-stalled")?,
            postponed: get("postponed")?,
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
        let events = [self.child.handle, self.signals.postponed.0, self.signals.post_desktop.0];
        let status = wait_any(&events, Some(limit.as_millis() as u32));
        if status == Some(1) {
            return Ok(Desktop::Postponed);
        }
        if status == Some(2) {
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
            let events = [
                self.signals.exited.0, self.child.handle, self.signals.postponed.0,
                self.signals.windows_ready.0, self.signals.stalled.0,
            ];
            match wait_any(&events, Some(RECOVERY_WAIT_MS)) {
                Some(0) if !self.child.ended() => return Ok(Desktop::Vacated),
                Some(2) => return Ok(Desktop::Postponed),
                Some(3) if result == Ok(true) => return Ok(Desktop::Restored),
                Some(3) => { wait_any(&[self.child.handle], Some(1_000)); }
                Some(4) => return Err(sign_out_message()),
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
        let timeout = if self.committed { RECOVERY_WAIT_MS } else { 1_000 };
        let status = wait_any(&[self.signals.windows_ready.0, self.child.handle, self.signals.stalled.0], Some(timeout));
        if status == Some(0) || self.child.exit_code() == RESTORED {
            Ok(())
        } else if status == Some(2) {
            Err(sign_out_message())
        } else {
            Err("the independent switch watcher could not restore Windows".into())
        }
    }

    pub fn committed(&self) -> bool {
        self.committed
    }

    pub fn observing(&self) -> bool {
        self.signals.stalled.signalled() && !self.child.ended()
    }

    pub fn recover(&mut self, worker: &Path, original: &Child) -> Result<(), String> {
        if !self.child.ended() {
            return Err("the independent switch watcher is still running".into());
        }
        let folders = read_snapshot(self.stamp).unwrap_or_else(|_| std::mem::take(&mut self.folders));
        std::fs::write(snapshot_path(self.stamp), reopen_list::encode(&folders))
            .map_err(|e| format!("save the replacement watcher's folder snapshot: {e}"))?;
        self.signals.stalled.reset()?;
        let args = format!("--watch-switch {} {} --recover {}", std::process::id(), self.stamp, original.pid);
        self.child = Child::spawn(worker, &args)?;
        self.restore()
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
    let explorer_pid = explorer_pid.parse::<u32>().map_err(|_| "invalid Explorer PID")?;
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
    let signals = Signals::open(stamp)?;
    let _cancel_for_restore = Event::named(CANCEL_FOR_RESTORE)?;
    let retiring = Child::monitor(explorer_pid)?;
    if tray == "--recover" {
        if !signals.rollback.signalled() || (!retiring.ended() && !taskbar::is_explorer(explorer_pid)) {
            return Err("the replacement watcher requires a rollback and the retiring Explorer".into());
        }
        let folders = read_snapshot(stamp)?;
        let recovery = install::try_recovery_lock_for(0)?;
        finish_recovery(stamp, &signals, &owner_process, &retiring, &folders, recovery)?;
        return Ok(RESTORED);
    }
    let tray = HWND(tray.parse::<usize>().map_err(|_| "invalid Explorer window handle")? as *mut _);
    if taskbar::explorer_window() != Some((tray, explorer_pid)) {
        return Err("Explorer changed before the switch was prepared".into());
    }
    log::line(&format!("switch: watcher reading Explorer folders for switch pid {owner}"));
    let snapshot = snapshot_path(stamp);
    let mut folders = capture_folders(&snapshot, Instant::now())?;
    log::line(&format!(
        "switch: watcher ready for switch pid {owner}, saved {} Explorer folder windows",
        folders.len()
    ));
    signals.prepared.signal()?;
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.prepare_exit.0], Some(PREPARE_BUDGET.as_millis() as u32)) != Some(2) {
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
        // Serialize the close with the uninstall's desktop check, including after our owner dies.
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
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.exit.0], Some(PREPARE_BUDGET.as_millis() as u32)) != Some(2) {
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
        match wait_any(&events, Some(PREPARE_BUDGET.as_millis() as u32)) {
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
    if signals.desktop_exit() == ExitState::Unsent
        && (started == Ok(false) || explorer::file_operations())
    {
        clear_snapshot(stamp);
        signals.postponed.signal()?;
        drop(recovery);
        drop(exiting);
        observe_recovery(&signals, &retiring, &folders, true)?;
        let _ = std::fs::remove_file(&snapshot);
        return Ok(POSTPONED);
    }
    finish_recovery(stamp, &signals, &owner_process, &retiring, &folders, Some(recovery))?;
    drop(exiting);
    if owner_lost {
        super::show(Ok(super::Outcome::Windows));
    }
    Ok(RESTORED)
}

fn finish_recovery(
    stamp: u64,
    signals: &Signals,
    owner: &Child,
    original: &Child,
    folders: &[Folder],
    recovery_lock: Option<install::Lock>,
) -> Result<(), String> {
    let exiting = Event::named(EXITING_EXPLORER)?;
    if signals.desktop_exit().pending() && !original.ended() {
        exiting.signal()?;
    }
    let Some(recovery_lock) = recovery_lock else {
        clear_snapshot(stamp);
        observe_recovery(signals, original, folders, false)?;
        let _ = std::fs::remove_file(snapshot_path(stamp));
        return Ok(());
    };
    let exe = install::dir().join("explorust.exe");
    let result = recover(stamp, signals, &exe, original, folders);
    drop(recovery_lock);
    match result {
        Ok(true) => {
            log::line(&format!("switch: Windows is back while Explorer pid {} is still exiting", original.pid));
            observe_recovery(signals, original, folders, true)
        }
        Ok(false) => Ok(()),
        Err(error) => {
            log::line(&format!("switch: {error}"));
            signals.stalled.signal()?;
            if owner.ended() {
                show_sign_out_notice();
            }
            observe_recovery(signals, original, folders, false)?;
            let _ = std::fs::remove_file(snapshot_path(stamp));
            Ok(())
        }
    }
}

fn wait_for_original_exit(original: &Child, owner: &Child, signals: &Signals) -> Result<(), String> {
    let events = [original.handle, owner.handle, signals.rollback.0];
    let started = Instant::now();
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
        if started.elapsed() >= DESKTOP_RECOVERY_LIMIT {
            return Err("Explorer did not exit within the handoff budget".into());
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

fn observe_recovery(signals: &Signals, original: &Child, folders: &[Folder], allow_explorust: bool) -> Result<(), String> {
    let exe = install::dir().join("explorust.exe");
    let may_recover = || {
        policy::observe_recovery(signals.desktop_exit(), !original.ended(), explorer::file_operations())
            == DesktopAction::Restore
    };
    loop {
        if may_recover() && let Some(_lock) = install::try_recovery_lock_for(0)? && may_recover() {
            let desktop = if allow_explorust {
                ensure_desktop(&exe)
            } else {
                restore_desktop(&exe, None)
            };
            match desktop {
                Ok(desktop) => {
                    log::line(&format!("switch: rechecked the desktop: {}", desktop.description));
                    if original.ended() {
                        for folder in folders.iter().filter(|folder| folder.owner == original.pid) {
                            if let Err(error) = folder_close::reopen_in_current_shell(&folder.path) {
                                log::line(&format!("switch: {error}"));
                            }
                        }
                    }
                    return signals.windows_ready.signal();
                }
                Err(error) => log::line(&format!("switch: {error}")),
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(super) fn sign_out_message() -> String {
    "The Windows desktop did not return. Sign out and sign in again.".into()
}
