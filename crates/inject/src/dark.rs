//! Dark mode and DWM backdrops, as the host process applies them to its own windows. The dark app
//! mode lives behind uxtheme's ordinal exports (the ones Windows' own apps and Notepad++ use):
//! 104 RefreshImmersiveColorPolicyState, 133 AllowDarkModeForWindow, 135 SetPreferredAppMode,
//! 136 FlushMenuThemes.

use windows::Win32::Foundation::HWND;
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::core::{HRESULT, PCSTR, w};

type AllowForWindow = unsafe extern "system" fn(HWND, BOOL) -> BOOL;
type SetAppMode = unsafe extern "system" fn(i32) -> i32;
type Void = unsafe extern "system" fn();

fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    // SAFETY: uxtheme is a system DLL already loaded in every GUI process; an ordinal is passed as
    // the low word of the name pointer, as GetProcAddress documents.
    unsafe {
        let m = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        GetProcAddress(m, PCSTR(ordinal as *const u8))
    }
}

/// The whole process prefers dark (ForceDark = 2): menus, common controls and the dialogs it
/// creates from now on.
pub fn force_dark_app() {
    // SAFETY: the transmutes give each ordinal its known signature on Windows 10 1903 and later.
    unsafe {
        if let Some(f) = uxtheme(135) {
            let f: SetAppMode = std::mem::transmute(f);
            f(2);
        }
        if let Some(f) = uxtheme(104) {
            let f: Void = std::mem::transmute(f);
            f();
        }
        if let Some(f) = uxtheme(136) {
            let f: Void = std::mem::transmute(f);
            f();
        }
    }
}

pub fn allow_dark_window(hwnd: HWND) {
    // SAFETY: as above; a stale hwnd only makes the call fail.
    unsafe {
        if let Some(f) = uxtheme(133) {
            let f: AllowForWindow = std::mem::transmute(f);
            let _ = f(hwnd, true.into());
        }
    }
}

/// Dark caption and the given backdrop on a frame. Returns the backdrop call's result, so the
/// loader can tell whether DWM takes it from another process.
pub fn frame(hwnd: HWND, backdrop: DWM_SYSTEMBACKDROP_TYPE) -> HRESULT {
    let on: i32 = 1;
    // SAFETY: plain value attributes, sizes match.
    unsafe {
        let a = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, 4);
        let b = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as _, 4);
        match (a, b) {
            (Err(e), _) | (_, Err(e)) => e.code(),
            _ => HRESULT(0),
        }
    }
}

