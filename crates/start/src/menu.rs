//! Right-click and power menus: Windows' own popup menus, in dark mode like the rest of Start.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
};
use windows_core::{HSTRING, PCSTR, w};

/// Opts this thread's menus into dark mode (uxtheme ordinals 135 SetPreferredAppMode and 136
/// FlushMenuThemes, what Windows' own apps call).
pub fn dark() {
    // SAFETY: looks up two exports by ordinal; skipped when this build lacks them.
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let set: unsafe extern "system" fn(i32) -> i32 = core::mem::transmute(f);
            set(2); // ForceDark
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = core::mem::transmute(f);
            flush();
        }
    }
}

/// Shows a menu at a screen point and returns the chosen id, 0 when dismissed. An id of 0 in
/// `items` is a separator.
pub fn pick(owner: HWND, x: i32, y: i32, items: &[(u32, &str)], up: bool) -> u32 {
    // SAFETY: a menu built, tracked and destroyed on this thread.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        for (id, label) in items {
            if *id == 0 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            } else {
                let _ = AppendMenuW(menu, MF_STRING, *id as usize, &HSTRING::from(*label));
            }
        }
        let align = if up { TPM_BOTTOMALIGN } else { Default::default() };
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | align;
        let id = TrackPopupMenuEx(menu, flags.0, x, y, owner, None).0 as u32;
        let _ = DestroyMenu(menu);
        id
    }
}
