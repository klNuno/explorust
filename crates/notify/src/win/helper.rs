//! Both sides of `explorust-notify.exe` (see its bin): `run` is the helper process, `start` is
//! called by the listener thread of the notify part when the change event is refused to it.

use crate::log;
use super::listen::{MSG_HELPER, MSG_WAKE};
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::UserNotificationListener;
use windows::UI::Notifications::UserNotificationChangedEventArgs;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const EXE: &str = "explorust-notify.exe";

/// The helper process: subscribes, says so, wakes `hwnd` on each change, exits with `parent`.
pub fn run(parent: u32, hwnd: isize) -> i32 {
    // SAFETY: COM for this thread, MTA: the event arrives on worker threads.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let target = HWND(hwnd as _);
    // SAFETY: waits on the parent only; the handle is closed below.
    let Ok(parent_handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }) else {
        log::line(&format!("helper: parent {parent} not found"));
        return 2;
    };
    let t0 = std::time::Instant::now();
    let subscribed = UserNotificationListener::Current().and_then(|l| {
        let handler = TypedEventHandler::new(move |_: windows_core::Ref<UserNotificationListener>, args: windows_core::Ref<UserNotificationChangedEventArgs>| {
            let id = args.as_ref().and_then(|a| a.UserNotificationId().ok()).unwrap_or(0);
            // SAFETY: posting to the part's window; a stale handle only fails the call.
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), MSG_WAKE, WPARAM(id as usize), LPARAM(0)) };
            Ok(())
        });
        l.NotificationChanged(&handler).map(|_| l)
    });
    let code = match subscribed {
        Ok(_listener) => {
            log::line(&format!("helper: NotificationChanged subscribed in {:.1} ms (pid {})", t0.elapsed().as_secs_f64() * 1000.0, std::process::id()));
            // SAFETY: posting to the part's window.
            let _ = unsafe { PostMessageW(Some(target), MSG_HELPER, WPARAM(1), LPARAM(0)) };
            // SAFETY: a plain wait on our own handle; the listener stays alive meanwhile.
            unsafe { WaitForSingleObject(parent_handle, INFINITE) };
            log::line("helper: parent gone, exiting");
            0
        }
        Err(e) => {
            log::line(&format!("helper: NotificationChanged refused ({e})"));
            // SAFETY: posting to the part's window.
            let _ = unsafe { PostMessageW(Some(target), MSG_HELPER, WPARAM(0), LPARAM(0)) };
            1
        }
    };
    // SAFETY: closes the handle opened above.
    unsafe { let _ = CloseHandle(parent_handle); }
    code
}

/// Starts the helper next to the current exe. Returns its process handle, or None when it is
/// missing, turned off (`EXPLORUST_NOTIFY_HELPER=0`) or did not start.
pub fn start(target: HWND) -> Option<HANDLE> {
    if std::env::var("EXPLORUST_NOTIFY_HELPER").as_deref() == Ok("0") {
        log::line("listener: helper turned off (EXPLORUST_NOTIFY_HELPER=0)");
        return None;
    }
    let exe = std::env::current_exe().ok()?.with_file_name(EXE);
    if !exe.exists() {
        log::line(&format!("listener: no {EXE} beside this exe"));
        return None;
    }
    let child = std::process::Command::new(&exe)
        .args(["--parent", &std::process::id().to_string(), "--hwnd", &(target.0 as isize).to_string()])
        .spawn();
    match child {
        Ok(c) => {
            use std::os::windows::io::IntoRawHandle;
            Some(HANDLE(c.into_raw_handle() as _))
        }
        Err(e) => {
            log::line(&format!("listener: {EXE} not started: {e}"));
            None
        }
    }
}

/// Whether the helper process still runs.
pub fn alive(h: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait on our own child handle.
    unsafe { WaitForSingleObject(h, 0) != WAIT_OBJECT_0 }
}
