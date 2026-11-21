//! DWM backdrop under a window. The taskbar look comes from an accent blur that stays on
//! while the window is inactive, which the documented system backdrops do not.

use windows::Win32::Foundation::HWND;
use windows_core::BOOL;
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute, DWMWA_BORDER_COLOR,
    DWMWA_COLOR_NONE,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_core::{s, w};

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_abgr: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionData {
    attrib: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

pub enum Backdrop {
    /// Blurred backdrop with a tint, as the acrylic taskbar.
    Acrylic { tint_abgr: u32 },
    /// No blur, the tint alone over what is behind.
    Transparent { tint_abgr: u32 },
}

/// Applies the backdrop and square corners with no DWM border.
pub fn apply(hwnd: HWND, backdrop: Backdrop) {
    unsafe {
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &none as *const _ as _, 4);
        let (state, tint) = match backdrop {
            Backdrop::Acrylic { tint_abgr } => (4, tint_abgr),
            Backdrop::Transparent { tint_abgr } => (2, tint_abgr),
        };
        accent(hwnd, state, tint);
    }
}

pub(crate) fn accent(hwnd: HWND, state: u32, tint: u32) {
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return };
        let set: unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL = core::mem::transmute(f);
        let mut policy = AccentPolicy { state, flags: 2, gradient_abgr: tint, animation_id: 0 };
        let mut data = CompositionData { attrib: 19, data: &mut policy as *mut _ as _, size: size_of::<AccentPolicy>() };
        let _ = set(hwnd, &mut data);
    }
}
