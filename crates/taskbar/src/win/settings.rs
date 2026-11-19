//! Reads the bar's settings from Windows' own registry values (never writes them).

use super::log;
use crate::settings::{Raw, Settings};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};
use windows_core::{PCWSTR, w};

const ADVANCED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");

fn dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: reads a DWORD into a local.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut len)) }
        .ok()
        .ok()
        .map(|_| v)
}

fn binary(path: PCWSTR, name: PCWSTR) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    let mut len = buf.len() as u32;
    // SAFETY: reads at most `len` bytes into the buffer.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)) }
        .ok()
        .ok()?;
    buf.truncate(len as usize);
    Some(buf)
}

pub fn read() -> Settings {
    let raw = Raw {
        taskbar_al: dword(ADVANCED, w!("TaskbarAl")),
        glom: dword(ADVANCED, w!("TaskbarGlomLevel")),
        mm_enabled: dword(ADVANCED, w!("MMTaskbarEnabled")),
        mm_mode: dword(ADVANCED, w!("MMTaskbarMode")),
        mm_glom: dword(ADVANCED, w!("MMTaskbarGlomLevel")),
        small_icons: dword(ADVANCED, w!("TaskbarSmallIcons")),
        icon_size: dword(ADVANCED, w!("IconSizePreference")),
        stuck_rects: binary(w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3"), w!("Settings")),
    };
    let s = Settings::from_raw(&raw);
    log::line(&format!(
        "settings: TaskbarAl {:?} Glom {:?} MM {:?}/{:?}/{:?} small {:?}/{:?} StuckRects3[8] {:?} -> {s:?}",
        raw.taskbar_al, raw.glom, raw.mm_enabled, raw.mm_mode, raw.mm_glom, raw.small_icons, raw.icon_size,
        raw.stuck_rects.as_ref().and_then(|b| b.get(8).copied())
    ));
    s
}
