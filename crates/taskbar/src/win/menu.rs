//! Windows' own popup menus for the Win+X menu and the jump list, in dark mode like Windows'
//! menus (uxtheme ordinals 135 SetPreferredAppMode and 136 FlushMenuThemes, what Start calls too).

use super::{actions, log};
use std::sync::Once;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCSTR, w};

pub enum Node {
    Item(u32, String),
    /// A grayed section title.
    Header(String),
    Separator,
    Sub(String, Vec<Node>),
}

static DARK: Once = Once::new();

/// Opts the process's menus into dark mode once.
pub fn dark() {
    DARK.call_once(|| {
        // SAFETY: looks up two uxtheme exports by ordinal; skipped when this build lacks them.
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
    });
}

/// French when the user's UI language is French.
pub fn french() -> bool {
    // SAFETY: a plain query.
    crate::winx::is_french(unsafe { GetUserDefaultUILanguage() })
}

unsafe fn build(nodes: &[Node]) -> HMENU {
    // SAFETY: menus built on this thread; a submenu is owned by its parent once appended.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return HMENU::default() };
        for n in nodes {
            match n {
                Node::Item(id, label) => {
                    let _ = AppendMenuW(menu, MF_STRING, *id as usize, &HSTRING::from(label.as_str()));
                }
                Node::Header(label) => {
                    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(label.as_str()));
                }
                Node::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                Node::Sub(label, items) => {
                    let sub = build(items);
                    let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, &HSTRING::from(label.as_str()));
                }
            }
        }
        menu
    }
}

/// Shows `nodes` at screen (x, y) and returns the chosen id, 0 when dismissed. The owner takes
/// the foreground first, as TrackPopupMenu needs to close on a click elsewhere.
pub fn track(owner: HWND, x: i32, y: i32, nodes: &[Node], align: TRACK_POPUP_MENU_FLAGS) -> u32 {
    dark();
    let fg = actions::activate(owner);
    // SAFETY: a menu built, tracked and destroyed on this thread.
    unsafe {
        let menu = build(nodes);
        if menu.is_invalid() {
            return 0;
        }
        log::line(&format!("menu at {x},{y}: {} entries, owner foreground {fg}", nodes.len()));
        let id = TrackPopupMenuEx(menu, (align | TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NOANIMATION).0, x, y, owner, None).0 as u32;
        let _ = DestroyMenu(menu);
        // Lets the menu's foreground grab settle (documented TrackPopupMenu remark).
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        id
    }
}
