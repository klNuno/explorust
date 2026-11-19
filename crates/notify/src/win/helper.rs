//! Both sides of `explorust-notify.exe` (see its bin): `run` is the helper process, `start` is
//! called by the listener thread of the notify part when the change event is refused to it.

use crate::log;
use super::listen::{MSG_HELPER, MSG_WAKE};
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::UserNotificationListener;
use windows::UI::Notifications::UserNotificationChangedEventArgs;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, WaitForSingleObject};
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

/// The helper's application in the sparse package of `crates/notify/package` (family name from
/// its name and unsigned publisher).
pub const AUMID: &str = "explorust.notify_m23m5a37cevzy!notify";

/// Starts the helper with package identity: activated through its AUMID, so the package's
/// external location (the folder of explorust-notify.exe) decides which exe runs. Windows' side by
/// side loader refuses the unsigned publisher in an exe manifest's `msix` element ("The msix
/// publisher must be valid according to MSIX packaging rules", SideBySide event, 26300.9974), so
/// a direct start can never give it the identity. Returns its process handle, or None when it is
/// turned off (`EXPLORUST_NOTIFY_HELPER=0`), the package is missing or it did not start.
pub fn start(target: HWND) -> Option<HANDLE> {
    if std::env::var("EXPLORUST_NOTIFY_HELPER").as_deref() == Ok("0") {
        log::line("listener: helper turned off (EXPLORUST_NOTIFY_HELPER=0)");
        return None;
    }
    let args = format!("--parent {} --hwnd {}", std::process::id(), target.0 as isize);
    let t0 = std::time::Instant::now();
    let pid = match activate(&args) {
        Ok(pid) => pid,
        Err(e) => {
            log::line(&format!("listener: {AUMID} not activated: {e}"));
            return None;
        }
    };
    // SAFETY: a handle to wait on the helper; closed by nobody, it lives as long as we watch it.
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) } {
        Ok(h) => {
            log::line(&format!("listener: {EXE} activated as {AUMID}, pid {pid}, in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0));
            Some(h)
        }
        Err(e) => {
            log::line(&format!("listener: {EXE} pid {pid} not opened: {e}"));
            None
        }
    }
}

fn activate(args: &str) -> windows_core::Result<u32> {
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{AO_NOERRORUI, ApplicationActivationManager, IApplicationActivationManager};
    use windows_core::HSTRING;
    // SAFETY: plain COM activation and one call with owned strings.
    unsafe {
        let m: IApplicationActivationManager = CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        m.ActivateApplication(&HSTRING::from(AUMID), &HSTRING::from(args), AO_NOERRORUI)
    }
}

/// Whether the helper process still runs.
pub fn alive(h: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait on our own child handle.
    unsafe { WaitForSingleObject(h, 0) != WAIT_OBJECT_0 }
}
