//! Both sides of `explorust-notify.exe` (see its bin): `run` is the helper process, `start` is
//! called by the listener thread of the notify part when the change event is refused to it.

use crate::log;
use super::listen::{MSG_HELPER, MSG_WAKE};
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::UserNotificationListener;
use windows::UI::Notifications::UserNotificationChangedEventArgs;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{GetCurrentProcess, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
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
    let _sinks = prime();
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
            // Releasing the sinks and the subscription calls into WpnUserService, which kept the
            // process alive after its parent left (g1, notify-6): end it without them.
            // SAFETY: ends this process; nothing is left to flush (the log line is written).
            unsafe { let _ = TerminateProcess(GetCurrentProcess(), 0); }
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

const LEGACY_AUMID: &str = "explorust.notify_m23m5a37cevzy!notify";

/// Starts the helper with package identity: activated through its AUMID, so the package's
/// external location decides which exe runs. Returns its process handle, or None when it is
/// turned off (`EXPLORUST_NOTIFY_HELPER=0`), the package is missing or it did not start.
pub fn start(target: HWND) -> Option<HANDLE> {
    if std::env::var("EXPLORUST_NOTIFY_HELPER").as_deref() == Ok("0") {
        log::line("listener: helper turned off (EXPLORUST_NOTIFY_HELPER=0)");
        return None;
    }
    let args = format!("--parent {} --hwnd {}", std::process::id(), target.0 as isize);
    let t0 = std::time::Instant::now();
    let (pid, aumid) = match activate(&args) {
        Ok(result) => result,
        Err(e) => {
            log::line(&format!("listener: {EXE} not activated: {e}"));
            return None;
        }
    };
    // SAFETY: a handle to wait on the helper; closed by nobody, it lives as long as we watch it.
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) } {
        Ok(h) => {
            log::line(&format!("listener: {EXE} activated as {aumid}, pid {pid}, in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0));
            Some(h)
        }
        Err(e) => {
            log::line(&format!("listener: {EXE} pid {pid} not opened: {e}"));
            None
        }
    }
}

pub fn activate(args: &str) -> windows_core::Result<(u32, &'static str)> {
    use crate::msix_identity::{self, NOTIFY_AUMID};
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{AO_NOERRORUI, ApplicationActivationManager, IApplicationActivationManager};
    use windows_core::HSTRING;
    // SAFETY: plain COM activation and one call with owned strings.
    unsafe {
        let m: IApplicationActivationManager = CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        let args = HSTRING::from(args);
        let start = |aumid| m.ActivateApplication(&HSTRING::from(aumid), &args, AO_NOERRORUI).map(|pid| (pid, aumid));
        start(NOTIFY_AUMID).or_else(|error| {
            if msix_identity::require().is_ok() { Err(error) } else { start(LEGACY_AUMID) }
        })
    }
}

/// Whether the helper process still runs.
pub fn alive(h: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait on our own child handle.
    unsafe { WaitForSingleObject(h, 0) != WAIT_OBJECT_0 }
}

/// WpnUserService (wpncore.dll 10.0.26100.9818) creates the global interface table of its
/// `RunningApps` only in `SetAppSink` and `SetConnectionStatusSink`, when an app registers a push
/// or connection sink. `NotificationChanged` goes to `AppEndpoint::RegisterNotificationSink`, then
/// `RunningApps::AddCallbackForNotificationStatus`, which uses that table unchecked: with explorer
/// and its hosts gone nobody registered a sink first, so the service died on a null read
/// (c0000005 at wpncore+0xd14bb, WER) and the call came back 0x800706BE (step notify-6). Registering
/// a sink first creates the table. What it returns is kept for the helper's life.
fn prime() -> Vec<windows_core::IInspectable> {
    use windows::Networking::PushNotifications::{PushNotificationChannel, PushNotificationChannelManager, PushNotificationReceivedEventArgs};
    use windows::Foundation::EventHandler;
    let mode = std::env::var("EXPLORUST_NOTIFY_PRIME").unwrap_or_else(|_| "both".into());
    let mut keep = Vec::new();
    if mode == "both" || mode == "revoked" {
        let t = std::time::Instant::now();
        let h = EventHandler::<windows::Networking::PushNotifications::PushNotificationChannelsRevokedEventArgs>::new(|_, _| Ok(()));
        let r = PushNotificationChannelManager::ChannelsRevoked(&h);
        log::line(&format!("helper: ChannelsRevoked sink {} in {:.1} ms", r.map(|_| "registered".to_string()).unwrap_or_else(|e| e.to_string()), t.elapsed().as_secs_f64() * 1000.0));
    }
    if mode == "both" || mode == "channel" {
        let t = std::time::Instant::now();
        // WNS may be out of reach: wait 10 s at most on a thread of its own.
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new().name("notify-channel".into()).spawn(move || {
            // SAFETY: COM for this short-lived thread, MTA.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let _ = tx.send(PushNotificationChannelManager::CreatePushNotificationChannelForApplicationAsync().and_then(|op| op.join()));
        });
        let channel = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| Err(windows_core::Error::new(windows_core::HRESULT(0x800705B4u32 as i32), "no channel in 10 s")));
        let r = channel.and_then(|c: PushNotificationChannel| {
            let h = TypedEventHandler::<PushNotificationChannel, PushNotificationReceivedEventArgs>::new(|_, _| Ok(()));
            c.PushNotificationReceived(&h)?;
            keep.push(windows_core::Interface::cast(&c)?);
            Ok(())
        });
        log::line(&format!("helper: push channel sink {} in {:.1} ms", r.map(|_| "registered".to_string()).unwrap_or_else(|e| e.to_string()), t.elapsed().as_secs_f64() * 1000.0));
    }
    keep
}

/// Dev only (`notify-dev --toast <text>` activates the helper with `--toast <text>`): shows a toast
/// as the helper's package, a sender the platform always stores, to test cards and the center.
pub fn toast(text: &str) -> i32 {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    // SAFETY: COM for this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let r = (|| -> windows_core::Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&windows_core::HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>explorust</text><text>{esc}</text></binding></visual></toast>"
        )))?;
        ToastNotificationManager::CreateToastNotifier()?.Show(&ToastNotification::CreateToastNotification(&xml)?)
    })();
    log::line(&format!("helper: toast '{text}': {r:?}"));
    i32::from(r.is_err())
}
