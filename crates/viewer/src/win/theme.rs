//! Light or dark from the system setting (AppsUseLightTheme), and the palette of each.

use render::{Color, rgba};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_core::w;

pub fn light() -> bool {
    let mut v = 0u32;
    let mut n = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as _),
            Some(&mut n),
        )
    };
    r.is_ok() && v == 1
}

pub struct Palette {
    pub text: Color,
    pub dim: Color,
    pub hover: Color,
    pub press: Color,
    pub plate: Color,
}

pub fn palette(light: bool) -> Palette {
    if light {
        Palette {
            text: rgba(0, 0, 0, 0.9),
            dim: rgba(0, 0, 0, 0.6),
            hover: rgba(0, 0, 0, 0.04),
            press: rgba(0, 0, 0, 0.025),
            plate: rgba(249, 249, 249, 0.85),
        }
    } else {
        Palette {
            text: rgba(255, 255, 255, 1.0),
            dim: rgba(255, 255, 255, 0.79),
            hover: rgba(255, 255, 255, 0.06),
            press: rgba(255, 255, 255, 0.04),
            plate: rgba(44, 44, 44, 0.85),
        }
    }
}
