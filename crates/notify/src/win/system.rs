//! What Windows decides around a toast: the user's notification settings and Do Not Disturb, the
//! notification sound, the light or dark theme, and activating the sender on a click.

use crate::log;
use crate::model::{Settings, UserState};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Notifications::INotificationActivationCallback;
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_core::{GUID, HSTRING, PCWSTR, w};

const NOTIF: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";

fn dword(key: &str, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: reads one DWORD into a local of the given size.
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut size))
    };
    r.is_ok().then_some(v)
}

fn string(key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: reads a string into a local buffer of the given byte size.
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size))
    };
    if r.is_err() {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// The user's settings for toasts from `aumid`.
pub fn settings(aumid: &str) -> Settings {
    let app = format!(r"{NOTIF}\{aumid}");
    Settings {
        toasts_on: dword(r"Software\Microsoft\Windows\CurrentVersion\PushNotifications", "ToastEnabled") != Some(0),
        dnd: dword(NOTIF, "NOC_GLOBAL_SETTING_TOASTS_ENABLED") == Some(0),
        app_on: dword(&app, "Enabled") != Some(0),
        app_banner: dword(&app, "ShowBanner") != Some(0),
        app_urgent: dword(&app, "AllowUrgentNotifications") == Some(1),
    }
}

pub fn user_state() -> UserState {
    // SAFETY: plain query.
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => UserState::from_quns(s.0),
        Err(_) => UserState::Unknown,
    }
}

/// Windows' "Notification" sound (Sound control panel, `Notification.Default`); nothing when the
/// user set it to (None) or `EXPLORUST_MUTE` is set (tests in the guest).
pub fn play_sound() {
    if std::env::var_os("EXPLORUST_MUTE").is_some() {
        return;
    }
    if string(r"AppEvents\Schemes\Apps\.Default\Notification.Default\.Current", "").is_some_and(|s| s.is_empty()) {
        return;
    }
    // SAFETY: plays a registered alias asynchronously.
    let _ = unsafe { PlaySoundW(w!("Notification.Default"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT) };
}

/// True when apps should look dark (Windows' toasts follow the system theme).
pub fn dark() -> bool {
    dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "SystemUsesLightTheme") == Some(0)
}

/// The toast activator COM class a Win32 sender registered for its AUMID, if any.
fn activator(aumid: &str) -> Option<GUID> {
    let s = string(&format!(r"Software\Classes\AppUserModelId\{aumid}"), "CustomActivator")?;
    GUID::try_from(s.trim_matches(|c| c == '{' || c == '}')).ok()
}

/// Activates the sender as a click on Windows' toast body does: through its toast activator
/// (`INotificationActivationCallback`) when it registered one, else by launching the app from
/// `shell:AppsFolder`, which also brings a packaged app forward. The toast's launch arguments are
/// not exposed by `UserNotificationListener`, so the activator gets an empty string.
pub fn activate(aumid: &str) {
    let t0 = std::time::Instant::now();
    if let Some(clsid) = activator(aumid) {
        // SAFETY: an out-of-process COM call on this thread's apartment.
        let r = unsafe {
            CoCreateInstance::<_, INotificationActivationCallback>(&clsid, None, CLSCTX_LOCAL_SERVER)
                .and_then(|cb| cb.Activate(&HSTRING::from(aumid), w!(""), &[]))
        };
        match r {
            Ok(()) => return log::line(&format!("activate {aumid}: activator {clsid:?} in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0)),
            Err(e) => log::line(&format!("activate {aumid}: activator failed ({e}), launching")),
        }
    }
    let target = HSTRING::from(format!(r"shell:AppsFolder\{aumid}"));
    // SAFETY: ShellExecute of a shell namespace path.
    let h = unsafe { ShellExecuteW(None, w!("open"), &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    log::line(&format!("activate {aumid}: shell:AppsFolder launch -> {} in {:.1} ms", h.0 as isize, t0.elapsed().as_secs_f64() * 1000.0));
}
