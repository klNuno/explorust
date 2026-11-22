//! Keep each WM_CLOSE receipt alive through rollback, even if the watcher exits.

use super::{
    explorer,
    reopen_list::Folder,
    watcher::{Signals, read_snapshot},
};
use guard::proc::{Child, Event, wait_any};
use guard::switch_events::{SWITCH_EXE, switch_event};
use guard::{account, install, log, reopen, taskbar};
use std::cell::{Cell, RefCell};
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, MSG, PM_NOREMOVE, PeekMessageW, RemovePropW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SendMessageCallbackW, SendMessageTimeoutW, SetPropW, WM_CLOSE, WM_NULL,
};
use windows::core::HSTRING;

const ACTIVE_CLOSES: &str = r"Local\explorust-switch-folder-closes";

pub fn pending() -> bool {
    Event::open_for_wait(ACTIVE_CLOSES).is_ok()
}

fn receipt_name(stamp: u64, hwnd: isize) -> String {
    switch_event(stamp, &format!("folder-{hwnd}"))
}

pub fn close(owner: u32, stamp: u64, folder: &Folder, budget: Duration) -> Result<bool, String> {
    let hwnd = folder.hwnd;
    if !explorer::folder_windows().contains(&(hwnd, folder.owner)) {
        return Ok(true);
    }
    let settled = Event::named(&receipt_name(stamp, hwnd))?;
    let watcher = std::process::id();
    let closer = Child::spawn(
        &install::dir().join(SWITCH_EXE),
        &format!("--close-folder {watcher} {owner} {stamp} {hwnd}"),
    )?;
    wait_any(&[settled.0, closer.handle], Some(budget.as_millis() as u32));
    // Even on timeout, the close process owns the request and can reopen this folder later.
    Ok(!explorer::folder_windows().contains(&(hwnd, folder.owner)))
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
    let _pending = Event::named(ACTIVE_CLOSES)?;
    let settled = Event::open_for_signal(&receipt_name(stamp, hwnd))?;
    let folder = read_snapshot(stamp)?
        .into_iter()
        .find(|folder| folder.hwnd == hwnd)
        .ok_or("folder not in the switch snapshot")?;
    if !explorer::folder_windows().contains(&(hwnd, folder.owner)) {
        return Ok(());
    }
    let window = TrackedWindow::new(HWND(hwnd as *mut _), &receipt_name(stamp, hwnd))?;
    if !window.answers() || !window.exists() {
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
    window.request_close()?;
    log::line(&format!(
        "switch: requested close of Explorer folder HWND {hwnd:#x} in pid {}",
        folder.owner
    ));
    // The callback runs on this thread from PeekMessageW. A reply with the original HWND
    // still alive settles the request, so a later user close cannot trigger a reopen.
    loop {
        if signals.completed.signalled() {
            return Ok(());
        }
        match window.close_reply() {
            Some(false) => return Ok(()),
            Some(true) => break,
            None if !window.exists() => break,
            None => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    settled.signal()?;
    loop {
        if signals.completed.signalled() {
            return Ok(());
        }
        if signals.windows_ready.signalled()
            || (watcher_process.ended() && owner_process.ended() && taskbar::explorer().is_some())
        {
            explorer::reopen_folder(&folder.path);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

thread_local! {
    static TRACKED_PROPERTY: RefCell<Option<HSTRING>> = const { RefCell::new(None) };
    static REPLY_CLOSED: Cell<Option<bool>> = const { Cell::new(None) };
}

unsafe extern "system" fn replied(hwnd: HWND, _: u32, _: usize, _: LRESULT) {
    TRACKED_PROPERTY.with_borrow(|property| {
        if let Some(property) = property {
            // SAFETY: this reads the marker even if the original HWND has already gone.
            REPLY_CLOSED.set(Some(unsafe { GetPropW(hwnd, property).is_invalid() }));
        }
    });
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

    fn answers(&self) -> bool {
        // SAFETY: WM_NULL has no pointer arguments. A suspended window never gets a close.
        unsafe {
            SendMessageTimeoutW(
                self.hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                500,
                None,
            )
            .0 != 0
        }
    }

    fn request_close(&self) -> Result<(), String> {
        TRACKED_PROPERTY.set(Some(self.property.clone()));
        // SAFETY: WM_CLOSE has no pointer arguments, and the callback depends only on thread-local state.
        unsafe { SendMessageCallbackW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0), Some(replied), 0) }
            .map_err(|e| format!("request Explorer folder close: {e}"))
    }

    fn close_reply(&self) -> Option<bool> {
        // SAFETY: PeekMessageW dispatches the callback on the sending thread.
        let _ = unsafe { PeekMessageW(&mut MSG::default(), None, 0, 0, PM_NOREMOVE) };
        REPLY_CLOSED.get()
    }
}

impl Drop for TrackedWindow {
    fn drop(&mut self) {
        // SAFETY: remove only our unique marker. Windows may already have removed the window.
        let _ = unsafe { RemovePropW(self.hwnd, &self.property) };
    }
}
