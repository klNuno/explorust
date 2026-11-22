//! Keep an independent process alive from Explorer preparation through the build verdict.

use super::{explorer, reopen_list};
use crate::proc::{Child, Event, wait_any};
use crate::{account, install, log, manage, reopen, taskbar};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const PREPARE_BUDGET: Duration = Duration::from_secs(15);
const RESTORED: u32 = 2;
const POSTPONED: u32 = 3;
const FAILED: u32 = 1;

/// The previous supervisor only relinquishes Explorer after its process exits during a switch.
pub const EXITING_EXPLORER: &str = r"Local\explorust-switch-exiting-explorer";

struct Signals {
    prepared: Event,
    exit: Event,
    exiting: Event,
    exited: Event,
    accepted: Event,
    rollback: Event,
}

impl Signals {
    fn open(stamp: u64) -> Result<Self, String> {
        let get = |suffix| Event::named(&format!(r"Local\explorust-switch-{stamp}-{suffix}"));
        Ok(Self {
            prepared: get("prepared")?,
            exit: get("exit")?,
            exiting: get("exiting")?,
            exited: get("exited")?,
            accepted: get("accepted")?,
            rollback: get("rollback")?,
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
    folders: Vec<(isize, String)>,
}

impl Watcher {
    pub fn prepare(
        guard: &Path,
        stamp: u64,
        tray: HWND,
        explorer_pid: u32,
        cancel: &crate::ui::Cancel,
    ) -> Result<Option<Self>, String> {
        let signals = Signals::open(stamp)?;
        let args = format!(
            "--watch-switch {} {stamp} {} {explorer_pid}",
            std::process::id(),
            tray.0 as usize,
        );
        let mut watcher = Self {
            child: Child::spawn(guard, &args)?,
            signals,
            stamp,
            committed: false,
            folders: Vec::new(),
        };
        let start = Instant::now();
        while !super::cancel_requested(cancel) {
            match wait_any(&[watcher.signals.prepared.0, watcher.child.handle], Some(100)) {
                Some(0) if !watcher.child.ended() => {
                    let path = snapshot_path(stamp);
                    let snapshot = std::fs::read_to_string(&path)
                        .and_then(|contents| reopen_list::decode(&contents))
                        .map_err(|e| format!("read Explorer recovery snapshot: {e}"))?;
                    watcher.folders = snapshot;
                    let _ = std::fs::remove_file(path);
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
        match wait_any(&[self.signals.exited.0, self.child.handle], Some(35_000)) {
            Some(0) => Ok(Desktop::Vacated),
            Some(_) if self.child.exit_code() == POSTPONED => Ok(Desktop::Postponed),
            Some(_) if self.child.exit_code() == RESTORED => Ok(Desktop::Restored),
            Some(_) => Err("the independent switch watcher could not close Explorer".into()),
            None => Err("the independent switch watcher did not report Explorer's exit".into()),
        }
    }

    pub fn accept(&self) -> Result<(), String> {
        self.signals.accepted.signal()?;
        if wait_any(&[self.child.handle], Some(5_000)).is_some() && self.child.exit_code() == 0 {
            Ok(())
        } else {
            Err("the independent switch watcher did not confirm the takeover".into())
        }
    }

    pub fn restore(&self) -> Result<(), String> {
        self.signals.rollback.signal()?;
        if wait_any(&[self.child.handle], Some(90_000)).is_none() {
            self.child.kill();
            return Err("the independent switch watcher timed out restoring Windows".into());
        }
        if self.child.exit_code() == RESTORED {
            Ok(())
        } else {
            Err("the independent switch watcher could not restore Windows".into())
        }
    }

    pub fn exit_requested(&self) -> bool {
        wait_any(&[self.signals.exiting.0], Some(0)).is_some()
    }

    pub fn recover(&self, exe: &Path, explorer: &Child) -> Result<(), String> {
        clear_snapshot(self.stamp);
        let retiring = self.exit_requested().then_some(explorer);
        restore_desktop(exe, retiring, &self.folders)
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = self.signals.rollback.signal();
        if !self.child.ended() && wait_any(&[self.child.handle], Some(200)).is_none() {
            self.child.kill();
        }
        clear_snapshot(self.stamp);
        let _ = std::fs::remove_file(snapshot_path(self.stamp));
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
            image.eq_ignore_ascii_case(&installed.join("explorust-guard.exe").to_string_lossy())
        })
    {
        return Err("the switch watcher has no valid unelevated switch owner".into());
    }
    if taskbar::explorer_window() != Some((tray, explorer_pid)) {
        return Err("Explorer changed before the switch was prepared".into());
    }
    let signals = Signals::open(stamp)?;
    let retiring = Child::monitor(explorer_pid)?;
    log::line(&format!("switch: watcher reading Explorer folders for guard pid {owner}"));
    let folders = explorer::open_folders()?;
    let paths: Vec<_> = folders.iter().map(|(_, path)| path.clone()).collect();
    let snapshot = snapshot_path(stamp);
    std::fs::write(&snapshot, reopen_list::encode(&folders))
        .map_err(|e| format!("save Explorer recovery snapshot: {e}"))?;
    reopen::save(&log::dir(), stamp, &paths).map_err(|e| format!("save Files handoff: {e}"))?;
    log::line(&format!(
        "switch: watcher ready for guard pid {owner}, saved {} Explorer folder windows",
        folders.len()
    ));
    signals.prepared.signal()?;
    if wait_any(&[owner_process.handle, signals.rollback.0, signals.exit.0], None) != Some(2) {
        clear_snapshot(stamp);
        let _ = std::fs::remove_file(snapshot);
        return Ok(0);
    }
    // A file operation may have appeared between the owner's check and this process waking.
    if explorer::file_operations() {
        log::line("switch: Explorer started another file operation before exit");
        clear_snapshot(stamp);
        return Ok(POSTPONED);
    }
    let exiting = Event::named(EXITING_EXPLORER)?;
    signals.exiting.signal()?;
    exiting.signal()?;
    let started = explorer::exit(tray, explorer_pid);
    if let Err(e) = started {
        log::line(&format!("switch: {e}"));
    } else if signals.exited.signal().is_ok() {
        match wait_any(
            &[signals.accepted.0, signals.rollback.0, owner_process.handle],
            Some(super::VERDICT_LIMIT.as_millis() as u32 + 15_000),
        ) {
            Some(0) => return Ok(0),
            Some(1) => log::line("switch: watcher received a rollback request"),
            Some(2) => log::line("switch: watcher saw the switch guard exit before takeover"),
            _ => {
                log::line("switch: watcher timed out waiting for the switch guard");
                owner_process.kill();
            }
        }
    }
    drop(exiting);
    clear_snapshot(stamp);
    restore_desktop(&installed.join("explorust.exe"), Some(&retiring), &folders)?;
    if owner_process.ended() {
        super::show(Ok(super::Outcome::Windows));
    }
    Ok(RESTORED)
}

fn clear_snapshot(stamp: u64) {
    let _ = reopen::take(&log::dir(), stamp);
}

fn snapshot_path(stamp: u64) -> PathBuf {
    log::dir().join(format!("switch-{stamp}.folders"))
}

fn restore_desktop(exe: &Path, retiring: Option<&Child>, folders: &[(isize, String)]) -> Result<(), String> {
    if let Some(retiring) = retiring {
        let deadline = Instant::now() + explorer::EXIT_BUDGET;
        while !retiring.ended() {
            if let Some(pid) = taskbar::explorer_except(Some(retiring.pid)) {
                log::line(&format!("switch: replacement Explorer pid {pid} already answers"));
                explorer::restore_folders(folders);
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("no replacement taskbar while Explorer pid {} retires", retiring.pid));
            }
            let _ = wait_any(&[retiring.handle], Some(200));
        }
    }
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
            if Event::set_existing(crate::supervise::FALLBACK_EVENT).is_ok() {
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
    let status = manage::bring_back_explorer()?;
    log::line(&format!("switch: restored Windows: {status}"));
    explorer::restore_folders(folders);
    Ok(())
}
