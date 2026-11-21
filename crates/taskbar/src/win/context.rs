//! Context actions for taskbar space, clock, system controls and explorust's tray icon.

use super::actions::{self, Launch};
use super::menu::{self, Node};
use super::log;
use crate::tray::{NIF_ICON, NIF_TIP, NIM_ADD, Notify};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
};

const ICON_UID: u32 = 0x6578;
const PNG_SIZE: usize = include_bytes!("../../res/explorust-tray.png").len();

#[repr(align(4))]
struct IconResource([u8; PNG_SIZE]);

static LOGO: IconResource = IconResource(*include_bytes!("../../res/explorust-tray.png"));

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
type Entry = (&'static str, &'static str, Action);

const TASKBAR: &[Entry] = &[
    ("Task Manager", "Gestionnaire des tâches", Open("taskmgr.exe")),
    ("Taskbar settings", "Paramètres de la barre des tâches", Open("ms-settings:taskbar")),
];
const CLOCK: &[Entry] = &[
    ("Adjust date and time", "Ajuster la date et l’heure", Open("ms-settings:dateandtime")),
    ("Notification settings", "Paramètres des notifications", Open("ms-settings:notifications")),
];
const QUICK_SETTINGS: &[Entry] = &[
    ("Open volume mixer", "Ouvrir le mélangeur de volume", Open("ms-settings:apps-volume")),
    ("Sound settings", "Paramètres audio", Open("ms-settings:sound")),
];
const DESKTOP: &[Entry] = &[("Show desktop", "Afficher le bureau", Desktop)];
const EXPLORUST: &[Entry] = &[
    ("Return to Windows Explorer", "Revenir à l’explorateur Windows", Guard("--fallback")),
    ("explorust settings", "Paramètres d’explorust", Settings),
    ("Uninstall", "Désinstaller", Guard("--uninstall")),
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
        .map(|(i, &(en, fr, _))| Node::Item(i as u32 + 1, if french { fr } else { en }.into()))
        .collect();
    let id = menu::track(owner, x, y, &nodes, TPM_LEFTALIGN | TPM_BOTTOMALIGN);
    log::line(&format!("context menu {kind:?}: picked {id}"));
    let Some(&(_, _, action)) = id.checked_sub(1).and_then(|i| entries.get(i as usize)) else { return };
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
    pub fn new() -> Option<Self> {
        // SAFETY: this is an embedded 32×32 PNG icon resource and the returned handle is owned.
        let icon = unsafe { CreateIconFromResourceEx(&LOGO.0, true, 0x0003_0000, 32, 32, LR_DEFAULTCOLOR) }.ok()?;
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
