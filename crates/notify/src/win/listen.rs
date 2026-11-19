//! Reads new toasts from the notification platform through `UserNotificationListener`, on a
//! thread of its own (MTA), and posts each one to the UI thread's command window.

use crate::log;
use super::wnf;
use crate::model::Toast;
use std::ffi::c_void;
use std::collections::HashSet;
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
    let existing = listener
        .GetNotificationsAsync(NotificationKinds::Toast)
        .and_then(|op| op.join())
        .map(|v| v.Size().unwrap_or(0))
        .unwrap_or(0);
    let target_bits = target.0 as isize;
    let handler = TypedEventHandler::new(move |l: windows_core::Ref<UserNotificationListener>, args: windows_core::Ref<windows::UI::Notifications::UserNotificationChangedEventArgs>| {
        let (Some(l), Some(args)) = (l.as_ref(), args.as_ref()) else { return Ok(()) };
        let id = args.UserNotificationId()?;
        let target = HWND(target_bits as _);
        if args.ChangeKind()? == UserNotificationChangedKind::Removed {
            // SAFETY: posting to our UI window; a stale handle only fails the call.
            let _ = unsafe { PostMessageW(Some(target), MSG_REMOVED, WPARAM(id as usize), LPARAM(0)) };
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
/// identity): diffs the toasts of the center by id each time a WNF state name given in
/// `EXPLORUST_NOTIFY_WNF` changes (`wnf.rs`), else once a second. No name that changes on a
/// published toast is known yet, so the default is the 1 s diff (about 7 ms CPU each).
fn watch(listener: &UserNotificationListener, target: HWND) {
    let ids = |l: &UserNotificationListener| -> Vec<(u32, UserNotification)> {
        l.GetNotificationsAsync(NotificationKinds::Toast)
            .and_then(|op| op.join())
            .map(|v| v.into_iter().filter_map(|n| n.Id().ok().map(|id| (id, n))).collect())
            .unwrap_or_default()
    };
    let t0 = std::time::Instant::now();
    let mut seen: HashSet<u32> = ids(listener).into_iter().map(|(id, _)| id).collect();
    let first_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let woken = subscribe();
    match woken {
        true => log::line(&format!("listener: waiting on WNF, {} toasts already in the center, one diff {first_ms:.2} ms", seen.len())),
        false => log::line(&format!("listener: no WNF subscription, polling every {POLL_MS} ms, {} toasts in the center", seen.len())),
    }
    loop {
        if woken {
            std::thread::park();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        }
        // The platform may signal before the toast is readable: one late retry.
        for attempt in 0..2 {
            let t = std::time::Instant::now();
            let now = ids(listener);
            let now_ids: HashSet<u32> = now.iter().map(|(id, _)| *id).collect();
            let mut changed = false;
            for (id, n) in &now {
                if seen.contains(id) {
                    continue;
                }
                changed = true;
                match read(n) {
                    Ok(t) => post_added(target, t),
                    Err(e) => log::line(&format!("listener: notification {id} unreadable: {e}")),
                }
            }
            for id in seen.difference(&now_ids) {
                changed = true;
                // SAFETY: posting to our UI window; a stale handle only fails the call.
                let _ = unsafe { PostMessageW(Some(target), MSG_REMOVED, WPARAM(*id as usize), LPARAM(0)) };
            }
            if woken {
                log::line(&format!("listener: diff {attempt} in {:.1} ms, changed {changed}", t.elapsed().as_secs_f64() * 1000.0));
            }
            seen = now_ids;
            if changed || !woken {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
}

const POLL_MS: u64 = 1000;

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
    let _ = LISTENER.set(std::thread::current());
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
