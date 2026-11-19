//! The taskband side of `ITaskbarList3` (see `crate::taskband` for the messages): the bar window
//! is published as `TaskbandHWND` on `Shell_TrayWnd`, and the messages apps send it are turned
//! into per-window progress, overlay badges and tooltips.

use super::{log, trayhost};
use crate::taskband::{self as tb, Progress};
use std::collections::HashMap;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{SHLockShared, SHUnlockShared};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, MSGFLT_ALLOW, PostMessageW, RegisterWindowMessageW, SetPropW,
};
use windows_core::w;

/// What a message changed, for the bar.
pub enum Change {
    None,
    Redraw,
    /// A window's badge: its image at the overlay size, None to remove it.
    Overlay(isize, Option<shelldata::Image>),
    /// SetActiveAlt or ActivateTab: show this window as the active one.
    Active(isize),
}

#[derive(Default)]
pub struct Taskband {
    pub progress: HashMap<isize, Progress>,
    /// Windows with a badge, and its accessible description.
    pub overlays: HashMap<isize, String>,
    pub tooltips: HashMap<isize, String>,
    pub fullscreen: HashMap<isize, bool>,
    /// The badge images, kept to redraw them after a DPI or display change makes a new scene.
    pub images: HashMap<isize, shelldata::Image>,
    button_created: u32,
}

/// Overlay badge size in DIPs.
pub const OVERLAY: f32 = 16.0;

impl Taskband {
    /// Publishes `hwnd` as the taskband of `tray` and lets lower-integrity apps reach it.
    pub fn publish(tray: HWND, hwnd: HWND, shellhook: u32) -> Taskband {
        // SAFETY: a window property and message filters on our own windows.
        let ok = unsafe {
            for m in tb::ALL.into_iter().chain([shellhook]) {
                let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
            }
            SetPropW(tray, w!("TaskbandHWND"), Some(HANDLE(hwnd.0))).is_ok()
        };
        // SAFETY: registers a message name.
        let button_created = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
        log::line(&format!("taskband published on Shell_TrayWnd: {ok}, TaskbarButtonCreated {button_created:#x}"));
        Taskband { button_created, ..Default::default() }
    }

    /// Tells a window its button exists, as explorer does: apps wait for it before calling ITaskbarList3.
    pub fn button_created(&self, h: isize) {
        // SAFETY: posting to an app window; a stale handle only fails.
        let _ = unsafe { PostMessageW(Some(HWND(h as _)), self.button_created, WPARAM(0), LPARAM(0)) };
    }

    /// Forgets a window that is gone.
    pub fn forget(&mut self, h: isize) -> bool {
        let a = self.progress.remove(&h).is_some();
        let b = self.overlays.remove(&h).is_some();
        self.tooltips.remove(&h);
        self.fullscreen.remove(&h);
        self.images.remove(&h);
        a || b
    }

    /// One taskband message; None when `msg` is not one.
    pub fn on_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM, overlay_px: i32) -> Option<Change> {
        let h = wp.0 as isize;
        let c = match msg {
            tb::MSG_PROGRESS_VALUE => {
                let p = self.progress.entry(h).or_default();
                p.set_value(lp.0 as u32);
                log::line(&format!("progress {h:#x}: {:?} {:.3}", p.state, p.value));
                Change::Redraw
            }
            tb::MSG_PROGRESS_STATE => {
                let p = self.progress.entry(h).or_default();
                p.set_state(lp.0 as u32);
                log::line(&format!("progress {h:#x}: state {:#x} = {:?}", lp.0, p.state));
                if !p.shown() {
                    self.progress.remove(&h);
                }
                Change::Redraw
            }
            tb::MSG_OVERLAY => {
                let img = trayhost::icon_image(lp.0, overlay_px);
                log::line(&format!("overlay {h:#x}: icon {:#x}, {}", lp.0, if img.is_some() { "drawn" } else { "removed" }));
                match &img {
                    Some(i) => {
                        self.overlays.entry(h).or_default();
                        self.images.insert(h, i.clone());
                    }
                    None => {
                        self.overlays.remove(&h);
                        self.images.remove(&h);
                    }
                }
                Change::Overlay(h, img)
            }
            tb::MSG_OVERLAY_TEXT | tb::MSG_TOOLTIP => {
                let s = shared_string(lp.0).unwrap_or_default();
                log::line(&format!("{} {h:#x}: [{s}]", if msg == tb::MSG_TOOLTIP { "thumbnail tooltip" } else { "overlay text" }));
                if msg == tb::MSG_TOOLTIP {
                    self.tooltips.insert(h, s);
                } else {
                    self.overlays.insert(h, s);
                }
                Change::None
            }
            tb::MSG_CLIP => {
                let r = shared_bytes(lp.0, 16).map(|b| {
                    let i = |k: usize| i32::from_le_bytes([b[k], b[k + 1], b[k + 2], b[k + 3]]);
                    format!("{},{} {},{}", i(0), i(4), i(8), i(12))
                });
                log::line(&format!("thumbnail clip {h:#x}: {}", r.unwrap_or_else(|| "none".into())));
                Change::None
            }
            tb::MSG_THUMB_ADD | tb::MSG_THUMB_UPDATE | tb::MSG_THUMB_IMAGES => {
                let hex = shared_bytes(lp.0, 48).map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>());
                log::line(&format!("thumbnail toolbar {msg:#x} {h:#x}: {}", hex.unwrap_or_default()));
                Change::None
            }
            tb::MSG_FULLSCREEN => {
                let w = lp.0 as isize;
                log::line(&format!("fullscreen {w:#x}: {}", wp.0 != 0));
                self.fullscreen.insert(w, wp.0 != 0);
                Change::None
            }
            tb::MSG_ACTIVE_ALT => {
                log::line(&format!("active alt {:#x}", lp.0));
                Change::Active(lp.0)
            }
            tb::MSG_REGISTER_TAB | tb::MSG_UNREGISTER_TAB | tb::MSG_TAB_ORDER | tb::MSG_TAB_ACTIVE | tb::MSG_TAB_PROPERTIES => {
                log::line(&format!("tab message {msg:#x}: {:#x} {:#x}", wp.0, lp.0));
                Change::None
            }
            _ => return None,
        };
        Some(c)
    }
}

/// Up to `max` bytes of an `SHAllocShared` block sent to us, bounded by its mapping.
fn shared_bytes(handle: isize, max: usize) -> Option<Vec<u8>> {
    if handle == 0 {
        return None;
    }
    // SAFETY: the handle was duplicated into this process by SHAllocShared for this message; the
    // read is bounded by the mapped region's size, and the view is unlocked before returning.
    unsafe {
        let p = SHLockShared(HANDLE(handle as _), GetCurrentProcessId());
        if p.is_null() {
            return None;
        }
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let n = if VirtualQuery(Some(p), &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) != 0 {
            (mbi.RegionSize - (p as usize - mbi.BaseAddress as usize)).min(max)
        } else {
            0
        };
        let out = std::slice::from_raw_parts(p as *const u8, n).to_vec();
        let _ = SHUnlockShared(p);
        Some(out)
    }
}

fn shared_string(handle: isize) -> Option<String> {
    let b = shared_bytes(handle, 8192)?;
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(tb::wide_string(&units))
}
