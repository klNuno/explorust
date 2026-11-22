//! Keep each WM_CLOSE receipt alive through rollback, even if the watcher exits.

use super::{
    close_progress::{Close, close_state, close_wait},
    explorer,
    reopen_list::Folder,
    watcher::{Signals, read_snapshot},
};
use guard::proc::{Child, Event, wait_any};
use guard::switch_events::{CANCEL_FOR_RESTORE, SWITCH_EXE, switch_event};
use guard::{account, install, log, reopen, taskbar};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, MSG, PM_NOREMOVE, PeekMessageW, RemovePropW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SendMessageCallbackW, SendMessageTimeoutW, SetPropW, WM_CLOSE, WM_NULL,
};
use windows::core::HSTRING;

const RESPONSIVENESS_CHECK: Duration = Duration::from_millis(500);
static CLOSE_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);

fn receipt_name(stamp: u64, hwnd: isize) -> String {
    switch_event(stamp, &format!("folder-{hwnd}"))
}

pub enum Handoff {
    Snapshot,
    EarlierReceipt,
    Pending,
}

pub fn close(owner: u32, stamp: u64, folder: &Folder, budget: Duration) -> Result<Handoff, String> {
    let hwnd = folder.hwnd;
    let open = || explorer::folder_windows().contains(&(hwnd, folder.owner));
    let started = Instant::now();
    if let Some(probe) = install::try_folder_close_lock(folder.owner, hwnd)? {
        drop(probe);
    } else {
        // The earlier receipt keeps its lock while we hold the recovery mutex. Leave it
        // responsible for reopening this window after the new switch commits or rolls back.
        while open() {
            let remaining = close_wait(budget, started.elapsed(), false);
            if remaining.is_zero() {
                return Ok(Handoff::Pending);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(50)));
        }
        return Ok(Handoff::EarlierReceipt);
    }
    if !open() {
        return Ok(Handoff::Snapshot);
    }
    let settled = Event::named(&receipt_name(stamp, hwnd))?;
    let watcher = std::process::id();
    let closer = Child::spawn(
        &install::dir().join(SWITCH_EXE),
        &format!("--close-folder {watcher} {owner} {stamp} {hwnd}"),
    )?;
    let wait = |duration: Duration| {
        wait_any(&[settled.0, closer.handle], Some(duration.as_millis() as u32)).is_none()
    };
    let mut pending = wait(close_wait(budget, started.elapsed(), false));
    while pending && open() {
        let remaining = close_wait(budget, started.elapsed(), true);
        if remaining.is_zero()
            || Event::is_signalled(CANCEL_FOR_RESTORE)
            || explorer::file_operations()
            || !window_answers(HWND(hwnd as *mut _), remaining.min(RESPONSIVENESS_CHECK))
        {
            break;
        }
        // A message reply cannot replace the close receipt. Give it short intervals to settle
        // so an unresponsive window or a new file operation postpones the switch promptly.
        let remaining = close_wait(budget, started.elapsed(), true);
        pending = wait(remaining.min(RESPONSIVENESS_CHECK));
    }
    // Even on timeout, the close process owns the request and can reopen this folder later.
    Ok(if open() { Handoff::Pending } else { Handoff::Snapshot })
}

pub fn run(arguments: impl Iterator<Item = String>) -> u32 {
    match track(arguments) {
        Ok(()) => 0,
        Err(e) => {
            log::line(&format!("switch: folder close: {e}"));
            1
        }
    }
}

fn track(arguments: impl Iterator<Item = String>) -> Result<(), String> {
    let arguments: Vec<_> = arguments.collect();
    let [watcher, owner, stamp, hwnd] = arguments.as_slice() else {
        return Err("missing close request arguments".into());
    };
    let watcher = watcher.parse::<u32>().map_err(|_| "invalid watcher PID")?;
    let owner = owner
        .parse::<u32>()
        .map_err(|_| "invalid switch owner PID")?;
    let stamp = stamp
        .parse::<u64>()
        .map_err(|_| "invalid switch owner stamp")?;
    let hwnd = hwnd
        .parse::<isize>()
        .map_err(|_| "invalid Explorer folder handle")?;
    let switch = install::dir().join(SWITCH_EXE);
    let watcher_process = Child::monitor(watcher)?;
    let owner_process = Child::monitor(owner)?;
    if install::elevated()
        || !account::is_session_user()
        || reopen::guard_stamp(owner) != Some(stamp)
        || !taskbar::image_path(watcher)
            .is_some_and(|image| image.eq_ignore_ascii_case(&switch.to_string_lossy()))
    {
        return Err("no valid unelevated switch watcher".into());
    }
    let signals = Signals::open(stamp)?;
    let settled = Event::open_for_signal(&receipt_name(stamp, hwnd))?;
    let folder = read_snapshot(stamp)?
        .into_iter()
        .find(|folder| folder.hwnd == hwnd)
        .ok_or("folder not in the switch snapshot")?;
    let Some(_receipt) = install::try_folder_close_lock(folder.owner, hwnd)? else {
        return Ok(());
    };
    if !explorer::folder_windows().contains(&(hwnd, folder.owner)) {
        return Ok(());
    }
    let window = TrackedWindow::new(HWND(hwnd as *mut _), &receipt_name(stamp, hwnd))?;
    if !window_answers(window.hwnd, RESPONSIVENESS_CHECK) || !window.exists() {
        return Err(format!(
            "Explorer folder in pid {} did not answer before the close request",
            folder.owner
        ));
    }
    if watcher_process.ended()
        || owner_process.ended()
        || signals.rollback.signalled()
        || explorer::file_operations()
    {
        return Ok(());
    }
    let started = Instant::now();
    let request = window.request_close();
    if request.is_ok() {
        log::line(&format!(
            "switch: requested close of Explorer folder HWND {hwnd:#x} in pid {}",
            folder.owner
        ));
    }
    // Track the original HWND through rollback. An acknowledgment from its message pump
    // cannot confirm destruction or that Explorer has abandoned the request.
    loop {
        if signals.completed.signalled() {
            return Ok(());
        }
        // SAFETY: PeekMessage dispatches the reply callback on the owning thread.
        let _ = unsafe { PeekMessageW(&mut MSG::default(), None, 0, 0, PM_NOREMOVE) };
        if CLOSE_ACKNOWLEDGED.swap(false, Ordering::Relaxed) {
            log::line(&format!(
                "switch: Explorer folder HWND {hwnd:#x} acknowledged WM_CLOSE after {} ms",
                started.elapsed().as_millis()
            ));
        }
        match close_state(request.is_ok(), window.exists()) {
            Close::Abandoned => return request,
            Close::Destroyed => {
                log::line(&format!(
                    "switch: Explorer folder HWND {hwnd:#x} destroyed after {} ms",
                    started.elapsed().as_millis()
                ));
                break;
            }
            Close::Pending => {}
        }
        if started.elapsed() >= guard::policy::DESKTOP_OBSERVE_LIMIT
            && watcher_process.ended() && owner_process.ended() && !explorer::file_operations()
        {
            return Err(format!("Explorer folder HWND {hwnd:#x} did not close"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    settled.signal()?;
    let started = Instant::now();
    loop {
        if signals.completed.signalled() {
            return Ok(());
        }
        if !explorer::file_operations()
            && (signals.windows_ready.signalled()
                || (watcher_process.ended() && owner_process.ended() && taskbar::explorer().is_some()))
        {
            return reopen_after_rollback(&folder.path);
        }
        if started.elapsed() >= guard::policy::DESKTOP_OBSERVE_LIMIT
            && watcher_process.ended() && owner_process.ended() && !explorer::file_operations()
        {
            return Err(format!("no desktop became ready to reopen {}", folder.path));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

unsafe extern "system" fn acknowledged(_: HWND, _: u32, _: usize, _: LRESULT) {
    CLOSE_ACKNOWLEDGED.store(true, Ordering::Relaxed);
}

fn window_answers(hwnd: HWND, timeout: Duration) -> bool {
    // SAFETY: WM_NULL has no pointer arguments.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            timeout.as_millis() as u32,
            None,
        )
        .0 != 0
    }
}

fn reopen_after_rollback(path: &str) -> Result<(), String> {
    let _recovery = install::recovery_lock_for(guard::policy::DESKTOP_OBSERVE_LIMIT.as_millis() as u32)?;
    guard::desktop_recovery::ensure_desktop(&install::dir().join("explorust.exe"))?;
    reopen_in_current_shell(path)
}

pub(super) fn reopen_in_current_shell(path: &str) -> Result<(), String> {
    let installed = install::dir();
    // Another switch can succeed after this HWND is gone, while this receipt waits to reopen it.
    if taskbar::explorust(&installed.join("explorust.exe")).is_some() {
        let status = std::process::Command::new(installed.join("explorust-open.exe"))
            .arg(path)
            .status()
            .map_err(|e| format!("could not reopen folder {path} in Files: {e}"))?;
        if !status.success() {
            return Err(format!("could not reopen folder {path} in Files: {status}"));
        }
        log::line(&format!("switch: reopened Explorer folder {path} in Files"));
    } else {
        explorer::reopen_folder(path);
    }
    Ok(())
}

/// Windows can reuse an HWND. The property is attached only to this window instance.
struct TrackedWindow {
    hwnd: HWND,
    property: HSTRING,
}

impl TrackedWindow {
    fn new(hwnd: HWND, name: &str) -> Result<Self, String> {
        let property = HSTRING::from(name);
        // SAFETY: the property is a marker only, and never dereferenced or treated as a handle.
        unsafe { SetPropW(hwnd, &property, Some(HANDLE(1 as *mut _))) }
            .map_err(|e| format!("identify Explorer folder: {e}"))?;
        Ok(Self { hwnd, property })
    }

    fn exists(&self) -> bool {
        // SAFETY: a dead or reused HWND has no marker.
        unsafe { !GetPropW(self.hwnd, &self.property).is_invalid() }
    }

    fn request_close(&self) -> Result<(), String> {
        // SAFETY: WM_CLOSE has no pointer arguments. The send returns before Explorer handles it.
        unsafe { SendMessageCallbackW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0), Some(acknowledged), 0) }
            .map_err(|e| format!("request Explorer folder close: {e}"))
    }
}

impl Drop for TrackedWindow {
    fn drop(&mut self) {
        // SAFETY: remove only our unique marker. Windows may already have removed the window.
        let _ = unsafe { RemovePropW(self.hwnd, &self.property) };
    }
}
