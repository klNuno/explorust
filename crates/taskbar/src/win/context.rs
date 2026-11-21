//! Context actions for taskbar space, clock, system controls and explorust's tray icon.

use super::actions::{self, Launch};
use super::menu::{self, Node};
use super::log;
use crate::tray::{NIF_ICON, NIF_TIP, NIM_ADD, Notify};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR,
};

const ICON_UID: u32 = 0x6578;
const LOGO: &[u8] = include_bytes!("../../../settings/res/explorust.ico");

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Taskbar,
    Clock,
    QuickSettings,
    ShowDesktop,
    Explorust,
}

#[derive(Clone, Copy)]
enum Action {
    Open(&'static str),
    Guard(&'static str),
    Settings,
    Desktop,
}

use Action::*;
type Entry = (&'static str, &'static str, Action, char);

const TASKBAR: &[Entry] = &[
    ("Task Manager", "Gestionnaire des tâches", Open("taskmgr.exe"), '\u{E9D9}'),
    ("Taskbar settings", "Paramètres de la barre des tâches", Open("ms-settings:taskbar"), '\u{E713}'),
];
const CLOCK: &[Entry] = &[
    ("Adjust date and time", "Ajuster la date et l’heure", Open("ms-settings:dateandtime"), '\u{E823}'),
    ("Notification settings", "Paramètres des notifications", Open("ms-settings:notifications"), '\u{E7ED}'),
];
const QUICK_SETTINGS: &[Entry] = &[
    ("Open volume mixer", "Ouvrir le mélangeur de volume", Open("ms-settings:apps-volume"), '\u{E767}'),
    ("Sound settings", "Paramètres audio", Open("ms-settings:sound"), '\u{E713}'),
];
const DESKTOP: &[Entry] = &[("Show desktop", "Afficher le bureau", Desktop, '\u{E7F4}')];
const EXPLORUST: &[Entry] = &[
    ("Return to Windows Explorer", "Revenir à l’explorateur Windows", Guard("--fallback"), '\u{E8DA}'),
    ("explorust settings", "Paramètres d’explorust", Settings, '\u{E713}'),
    ("Uninstall", "Désinstaller", Guard("--uninstall"), '\u{E74D}'),
];

pub fn show(owner: HWND, x: i32, y: i32, kind: Kind) {
    let entries = match kind {
        Kind::Taskbar => TASKBAR,
        Kind::Clock => CLOCK,
        Kind::QuickSettings => QUICK_SETTINGS,
        Kind::ShowDesktop => DESKTOP,
        Kind::Explorust => EXPLORUST,
    };
    let french = menu::french();
    let nodes: Vec<_> = entries.iter().enumerate()
        .map(|(i, &(en, fr, _, glyph))| Node::Item(i as u32 + 1, if french { fr } else { en }.into(), glyph))
        .collect();
    let id = menu::track(owner, x, y, &nodes, false);
    log::line(&format!("context menu {kind:?}: picked {id}"));
    let Some(&(_, _, action, _)) = id.checked_sub(1).and_then(|i| entries.get(i as usize)) else { return };
    match action {
        Open(uri) => actions::launch(Launch::Path(uri.into())),
        Guard(arg) => {
            if let Some(path) = installed("explorust-guard.exe") {
                actions::launch(Launch::Command(path, arg));
            }
        }
        Settings => {
            if let Some(path) = installed("explorust-settings.exe") {
                actions::launch(Launch::Path(path));
            }
        }
        Desktop => {
            let ok = bus::post(bus::Command::ShowDesktop, 0);
            log::line(&format!("context show desktop: posted {ok}"));
        }
    }
}

fn installed(name: &str) -> Option<String> {
    let path = std::env::current_exe().ok()?.with_file_name(name);
    if !path.is_file() {
        log::line(&format!("context menu: {name} is not installed"));
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

/// The local icon uses the same overflow layout, icon cache and tooltip path as other icons.
pub struct Logo(HICON);

impl Logo {
    pub fn new(hwnd: HWND) -> Option<Self> {
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96);
        let px = (16 * dpi).div_ceil(96) as i32;
        let count = u16::from_le_bytes(LOGO[4..6].try_into().ok()?) as usize;
        let entry = LOGO[6..].chunks_exact(16).take(count).find(|e| e[0] as i32 >= px || e[0] == 0)?;
        let size = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
        // CreateIconFromResourceEx requires DWORD-aligned image data.
        let mut image = vec![0u32; size.div_ceil(4)];
        let bytes = unsafe { std::slice::from_raw_parts_mut(image.as_mut_ptr().cast::<u8>(), size) };
        bytes.copy_from_slice(LOGO.get(offset..offset.checked_add(size)?)?);
        let icon = unsafe { CreateIconFromResourceEx(bytes, true, 0x0003_0000, px, px, LR_DEFAULTCOLOR) }.ok()?;
        Some(Self(icon))
    }

    pub fn registration(&self, owner: HWND) -> Notify {
        Notify {
            msg: NIM_ADD,
            uid: ICON_UID,
            hwnd: owner.0 as isize,
            flags: NIF_ICON | NIF_TIP,
            hicon: self.0.0 as isize,
            tip: "explorust".into(),
            ..Default::default()
        }
    }

    pub fn matches(&self, handle: isize) -> bool {
        self.0.0 as isize == handle
    }
}

impl Drop for Logo {
    fn drop(&mut self) {
        // SAFETY: created by CreateIconFromResourceEx without LR_SHARED.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}
