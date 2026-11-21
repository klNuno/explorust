//! Command menus on the taskbar use the same renderer as the shell view's menus.

use super::actions;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Globalization::GetUserDefaultUILanguage;

pub use ::menu::CommandItem as Node;

pub fn french() -> bool {
    // SAFETY: a plain query.
    crate::winx::is_french(unsafe { GetUserDefaultUILanguage() })
}

pub fn track(owner: HWND, x: i32, y: i32, nodes: &[Node], centered: bool) -> u32 {
    actions::activate(owner);
    ::menu::track_commands(owner, POINT { x, y }, nodes, centered).unwrap_or(0)
}
