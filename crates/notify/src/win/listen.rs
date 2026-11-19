//! Reads new toasts from the notification platform through `UserNotificationListener`, on a
//! thread of its own (MTA), and posts each one to the UI thread's command window.

use crate::log;
use super::{helper, wnf};
use crate::model::Toast;
use std::ffi::c_void;
use crate::seen::Seen;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification, UserNotificationChangedKind};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// lParam = `Box<Toast>` raw pointer, owned by the receiver.
pub const MSG_ADDED: u32 = 0x8000 + 0x201;
/// wParam = the notification id that left the center.
pub const MSG_REMOVED: u32 = 0x8000 + 0x202;
/// From `explorust-notify.exe`: the center changed (wParam = notification id).
pub const MSG_WAKE: u32 = 0x8000 + 0x203;
/// From `explorust-notify.exe`: wParam 1 when it subscribed to the change event, 0 when refused.
pub const MSG_HELPER: u32 = 0x8000 + 0x204;

/// 0 no answer yet, 1 the helper subscribed, 2 it was refused.
static HELPER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// UI thread, on `MSG_WAKE`: the listener diffs the center now.
pub fn wake() {
    if let Some(t) = LISTENER.get() {
        t.unpark();
    }
}

/// UI thread, on `MSG_HELPER`.
pub fn helper_said(ok: bool) {
    HELPER.store(if ok { 1 } else { 2 }, std::sync::atomic::Ordering::Release);
    wake();
}

/// Starts the listener thread; it lives as long as the process (the event keeps it registered).
pub fn spawn(target: HWND) {
    let target = target.0 as isize;
    let spawned = std::thread::Builder::new().name("notify-listener".into()).spawn(move || listen(HWND(target as _)));
    if let Err(e) = spawned {
        log::line(&format!("listener thread not started: {e}"));
    }
}

fn listen(target: HWND) {
    // SAFETY: COM for this thread, MTA: WinRT events arrive on worker threads.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let t0 = std::time::Instant::now();
    let listener = match UserNotificationListener::Current() {
        Ok(l) => l,
        Err(e) => return log::line(&format!("listener: unavailable: {e}")),
    };
    let access = listener.GetAccessStatus().unwrap_or(UserNotificationListenerAccessStatus::Unspecified);
    log::line(&format!("listener: access {:?}", access.0));
    if access != UserNotificationListenerAccessStatus::Allowed {
        match listener.RequestAccessAsync().and_then(|op| op.join()) {
            Ok(a) if a == UserNotificationListenerAccessStatus::Allowed => log::line("listener: access granted"),
            Ok(a) => return log::line(&format!("listener: access denied ({}), turn on notification access for desktop apps", a.0)),
            Err(e) => return log::line(&format!("listener: access request failed: {e}")),
        }
    }
    let at_start: Vec<u32> = listener
        .GetNotificationsAsync(NotificationKinds::Toast)
        .and_then(|op| op.join())
        .map(|v| v.into_iter().filter_map(|n| n.Id().ok()).collect())
        .unwrap_or_default();
    let existing = at_start.len();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Seen::start(at_start)));
    let target_bits = target.0 as isize;
    let handler = TypedEventHandler::new(move |l: windows_core::Ref<UserNotificationListener>, args: windows_core::Ref<windows::UI::Notifications::UserNotificationChangedEventArgs>| {
        let (Some(l), Some(args)) = (l.as_ref(), args.as_ref()) else { return Ok(()) };
        let id = args.UserNotificationId()?;
        let target = HWND(target_bits as _);
        if args.ChangeKind()? == UserNotificationChangedKind::Removed {
            seen.lock().map(|mut s| s.gone(id)).ok();
            // SAFETY: posting to our UI window; a stale handle only fails the call.
            let _ = unsafe { PostMessageW(Some(target), MSG_REMOVED, WPARAM(id as usize), LPARAM(0)) };
            return Ok(());
        }
        if !seen.lock().map(|mut s| s.fresh(id)).unwrap_or(true) {
            return Ok(());
        }
        match l.GetNotification(id).and_then(|n| read(&n)) {
            Ok(toast) => post_added(target, toast),
            Err(e) => log::line(&format!("listener: notification {id} unreadable: {e}")),
        }
        Ok(())
    });
    match listener.NotificationChanged(&handler) {
        Ok(_) => log::line(&format!(
            "listener: subscribed in {:.1} ms, {existing} toasts already in the center",
            t0.elapsed().as_secs_f64() * 1000.0
        )),
        // An unpackaged process gets 0x80070490 here: watch the platform's database instead.
        Err(e) => {
            log::line(&format!("listener: NotificationChanged refused ({e}), polling"));
            return watch(&listener, target);
        }
    }
    // The subscription lives with this thread; park it until the process ends.
    loop {
        std::thread::park();
    }
}

/// The fallback when the platform refuses the change event (any process without package
/// identity): diffs the toasts of the center by id each time `explorust-notify.exe` (which has
/// the identity, `helper.rs`) says the center changed, or a WNF state name given in
/// `EXPLORUST_NOTIFY_WNF` changes (`wnf.rs`), else once a second (about 7 ms CPU each).
fn watch(listener: &UserNotificationListener, target: HWND) {
    let ids = |l: &UserNotificationListener| -> Vec<(u32, UserNotification)> {
        l.GetNotificationsAsync(NotificationKinds::Toast)
            .and_then(|op| op.join())
            .map(|v| v.into_iter().filter_map(|n| n.Id().ok().map(|id| (id, n))).collect())
            .unwrap_or_default()
    };
    let t0 = std::time::Instant::now();
    let mut seen = Seen::start(ids(listener).into_iter().map(|(id, _)| id));
    let first_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = LISTENER.set(std::thread::current());
    let mut helper = start_helper(target);
    let mut woken = helper.is_some() || subscribe();
    match (helper.is_some(), woken) {
        (true, _) => log::line(&format!("listener: waiting on {}, {} toasts already in the center, one diff {first_ms:.2} ms", helper::EXE, seen.len())),
        (false, true) => log::line(&format!("listener: waiting on WNF, {} toasts already in the center, one diff {first_ms:.2} ms", seen.len())),
        (false, false) => log::line(&format!("listener: no WNF subscription, polling every {POLL_MS} ms, {} toasts in the center", seen.len())),
    }
    loop {
        if let Some(h) = helper {
            // A safety net only: the helper wakes this thread on every change.
            std::thread::park_timeout(std::time::Duration::from_secs(HELPER_CHECK_S));
            if !helper::alive(h) {
                log::line(&format!("listener: {} exited, polling every {POLL_MS} ms", helper::EXE));
                helper = None;
                woken = false;
            }
        } else if woken {
            std::thread::park();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        }
        // The platform may signal before the toast is readable: one late retry.
        for attempt in 0..2 {
            let t = std::time::Instant::now();
            let now = ids(listener);
            let diff = seen.update(now.iter().map(|(id, _)| *id));
            let changed = !(diff.added.is_empty() && diff.removed.is_empty() && diff.returned.is_empty());
            for (id, n) in now.iter().filter(|(id, _)| diff.added.contains(id)) {
                match read(n) {
                    Ok(t) => post_added(target, t),
                    Err(e) => log::line(&format!("listener: notification {id} unreadable: {e}")),
                }
            }
            for id in &diff.removed {
                // SAFETY: posting to our UI window; a stale handle only fails the call.
                let _ = unsafe { PostMessageW(Some(target), MSG_REMOVED, WPARAM(*id as usize), LPARAM(0)) };
            }
            if !diff.returned.is_empty() {
                log::line(&format!("listener: {:?} back in the center, already seen: no card", diff.returned));
            }
            if woken {
                log::line(&format!("listener: diff {attempt} in {:.1} ms, changed {changed}", t.elapsed().as_secs_f64() * 1000.0));
            }
            if changed || !woken {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
}

const POLL_MS: u64 = 1000;
const HELPER_CHECK_S: u64 = 60;

/// Starts `explorust-notify.exe` and waits up to 5 s for its answer; its handle when it subscribed.
fn start_helper(target: HWND) -> Option<windows::Win32::Foundation::HANDLE> {
    let h = helper::start(target)?;
    let t0 = std::time::Instant::now();
    while HELPER.load(std::sync::atomic::Ordering::Acquire) == 0 && t0.elapsed().as_secs() < 5 && helper::alive(h) {
        std::thread::park_timeout(std::time::Duration::from_millis(200));
    }
    if HELPER.load(std::sync::atomic::Ordering::Acquire) == 1 {
        log::line(&format!("listener: {} subscribed after {:.1} ms", helper::EXE, t0.elapsed().as_secs_f64() * 1000.0));
        return Some(h);
    }
    log::line(&format!("listener: {} gave no subscription (state {})", helper::EXE, HELPER.load(std::sync::atomic::Ordering::Acquire)));
    if helper::alive(h) {
        // SAFETY: ends our own child, which answered nothing useful.
        unsafe { let _ = windows::Win32::System::Threading::TerminateProcess(h, 1); }
    }
    None
}

/// The WNF state names to wait on, from `EXPLORUST_NOTIFY_WNF` (hex, comma separated). None by
/// default: in g1 (26300, no toast UI running) `notify-dev --wnf-scan` saw no well-known name
/// change when a toast was published, `WNF_SHEL_TOAST_PUBLISHED` included (step notify-3).
fn wnf_names() -> Vec<u64> {
    std::env::var_os("EXPLORUST_NOTIFY_WNF")
        .map(|v| v.to_string_lossy().split(',').filter_map(|h| u64::from_str_radix(h.trim().trim_start_matches("0x"), 16).ok()).collect())
        .unwrap_or_default()
}

static LISTENER: std::sync::OnceLock<std::thread::Thread> = std::sync::OnceLock::new();

unsafe extern "system" fn wnf_changed(name: u64, stamp: u32, _: *const c_void, _: *mut c_void, _: *const c_void, size: u32) -> i32 {
    log::line(&format!("listener: WNF {name:016x} stamp {stamp} ({size} bytes)"));
    if let Some(t) = LISTENER.get() {
        t.unpark();
    }
    0
}

/// Subscribes this thread to the toast WNF names; false when none took.
fn subscribe() -> bool {
    let mut any = false;
    for name in wnf_names() {
        match wnf::subscribe(name, wnf_changed, std::ptr::null_mut()) {
            Ok(()) => any = true,
            Err(st) => log::line(&format!("listener: WNF {name:016x} not subscribed: 0x{st:08x}")),
        }
    }
    any
}

fn post_added(target: HWND, toast: Toast) {
    let raw = Box::into_raw(Box::new(toast));
    // SAFETY: the UI thread takes the box back with Box::from_raw; freed here on failure.
    if unsafe { PostMessageW(Some(target), MSG_ADDED, WPARAM(0), LPARAM(raw as isize)) }.is_err() {
        drop(unsafe { Box::from_raw(raw) });
    }
}

fn read(n: &UserNotification) -> windows_core::Result<Toast> {
    let info = n.AppInfo()?;
    let aumid = info.AppUserModelId().map(|s| s.to_string()).unwrap_or_default();
    let app = info.DisplayInfo().and_then(|d| d.DisplayName()).map(|s| s.to_string()).unwrap_or_default();
    let mut texts = Vec::new();
    let binding = n.Notification()?.Visual()?.GetBinding(&KnownNotificationBindings::ToastGeneric()?);
    if let Ok(b) = binding {
        for t in b.GetTextElements()? {
            texts.push(t.Text()?.to_string());
        }
    }
    Ok(Toast { id: n.Id()?, aumid, app, texts })
}
