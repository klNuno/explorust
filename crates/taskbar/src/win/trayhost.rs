//! The tray host on `Shell_TrayWnd`: `Shell_NotifyIcon` calls from WM_COPYDATA (dwData 1), which
//! icons are promoted beside the clock (the user's `NotifyIconSettings`), icon handles turned into
//! images, and user input forwarded to each icon's window with its callback message.

use super::log;
use crate::tray::{self, Change, Event, Icon, Notify, Tray};
use std::collections::HashMap;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    SelectObject,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, DI_NORMAL, DrawIconEx, GetWindowThreadProcessId, HICON, IsWindow, PostMessageW,
    RegisterWindowMessageW, SendNotifyMessageW, HWND_BROADCAST,
};
use windows_core::{PCWSTR, PWSTR, w};

pub struct TrayHost {
    pub tray: Tray,
    /// Lower-case exe file name -> IsPromoted, from `HKCU\Control Panel\NotifyIconSettings`.
    promoted: HashMap<String, bool>,
    logged_raw: bool,
}

impl TrayHost {
    pub fn new() -> TrayHost {
        TrayHost { tray: Tray::default(), promoted: read_promoted(), logged_raw: false }
    }

    /// One `Shell_NotifyIcon` payload: (the call's result for the sender, redraw).
    pub fn on_copydata(&mut self, data: &[u8]) -> (bool, bool) {
        if !self.logged_raw {
            self.logged_raw = true;
            let hex = |r: std::ops::Range<usize>| data.get(r).map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>()).unwrap_or_default();
            log::line(&format!("tray payload {} bytes: head {} at 808 {} at 940 {}", data.len(), hex(0..36), hex(808..816), hex(940..968)));
        }
        let Some(n) = tray::parse(data) else {
            log::line(&format!("tray: unreadable payload of {} bytes", data.len()));
            return (false, false);
        };
        let promoted = self.is_promoted(n.hwnd);
        let change = self.tray.apply(&n, promoted);
        log_call(&n, change, promoted, data.len());
        if n.flags & tray::NIF_INFO != 0 && !(n.info.is_empty() && n.info_title.is_empty()) {
            log::line(&format!(
                "balloon {:#x}/{} {}: [{}] {}",
                n.hwnd, n.uid, tray::balloon_kind(n.info_flags), n.info_title, n.info
            ));
        }
        (change != Change::Failed, matches!(change, Change::Added(_) | Change::Modified(_) | Change::Removed))
    }

    fn is_promoted(&mut self, hwnd: isize) -> bool {
        let Some(exe) = exe_of(HWND(hwnd as _)) else { return false };
        let name = file_name(&exe);
        if !self.promoted.contains_key(&name) {
            self.promoted = read_promoted();
        }
        self.promoted.get(&name).copied().unwrap_or(false)
    }

    /// Drops icons whose window died without NIM_DELETE; true when one went.
    pub fn prune(&mut self) -> bool {
        // SAFETY: IsWindow only checks a handle.
        self.tray.retain(|h| unsafe { IsWindow(Some(HWND(h as _))) }.as_bool())
    }
}

fn log_call(n: &Notify, change: Change, promoted: bool, len: usize) {
    let name = match n.msg {
        tray::NIM_ADD => "ADD",
        tray::NIM_MODIFY => "MODIFY",
        tray::NIM_DELETE => "DELETE",
        tray::NIM_SETFOCUS => "SETFOCUS",
        tray::NIM_SETVERSION => "SETVERSION",
        _ => "?",
    };
    let exe = exe_of(HWND(n.hwnd as _)).map(|e| file_name(&e)).unwrap_or_default();
    log::line(&format!(
        "tray {name} {exe} hwnd {:#x} uid {} flags {:#x} cb {:#x} icon {:#x} v{} layout {}/{len} tip [{}] promoted {promoted}: {change:?}",
        n.hwnd, n.uid, n.flags, n.callback, n.hicon, n.version, n.layout, n.tip
    ));
}

fn exe_of(h: HWND) -> Option<String> {
    let mut pid = 0u32;
    // SAFETY: reads the owner process id of a window.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid != 0).then(|| shelldata::items::process_path(pid)).flatten()
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

/// Reads every `NotifyIconSettings` entry: its exe file name and IsPromoted.
fn read_promoted() -> HashMap<String, bool> {
    let mut out = HashMap::new();
    // SAFETY: registry reads into locals; the key is closed before returning.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Control Panel\NotifyIconSettings"), Some(0), KEY_READ, &mut key).is_err() {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 64];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            let sub = PCWSTR(name.as_ptr());
            let mut path = [0u16; 520];
            let mut plen = (path.len() * 2) as u32;
            if RegGetValueW(key, sub, w!("ExecutablePath"), RRF_RT_REG_SZ, None, Some(path.as_mut_ptr().cast()), Some(&mut plen)).is_err() {
                continue;
            }
            let n = path.iter().position(|c| *c == 0).unwrap_or(0);
            let exe = file_name(&String::from_utf16_lossy(&path[..n]));
            let mut v = 0u32;
            let mut vlen = 4u32;
            let promoted = RegGetValueW(key, sub, w!("IsPromoted"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut vlen)).is_ok() && v != 0;
            let e = out.entry(exe).or_insert(false);
            *e |= promoted;
        }
        let _ = RegCloseKey(key);
    }
    out
}

/// Posts the callback messages of `ev` to the icon's window; a click first lets its process take
/// the foreground, as explorer does, so its menu or window can come up.
pub fn forward(icon: &Icon, ev: Event, x: i32, y: i32) {
    if icon.callback == 0 {
        return;
    }
    let h = HWND(icon.hwnd as _);
    if matches!(ev, Event::LeftUp | Event::RightUp | Event::LeftDouble | Event::Key | Event::Menu | Event::MiddleUp) {
        let mut pid = 0u32;
        // SAFETY: plain calls on a window handle; a stale one only fails.
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
            let _ = AllowSetForegroundWindow(pid);
        }
    }
    let msgs = tray::messages(icon, ev, x, y);
    for (wp, lp) in &msgs {
        // SAFETY: posting to the icon's window.
        let _ = unsafe { PostMessageW(Some(h), icon.callback, WPARAM(*wp), LPARAM(*lp)) };
    }
    if !matches!(ev, Event::Move) {
        let names: Vec<String> = msgs.iter().map(|(_, lp)| format!("{:#x}", if icon.version >= 4 { lp & 0xffff } else { *lp })).collect();
        log::line(&format!("tray input {ev:?} -> {:#x}/{} v{} cb {:#x}: {}", icon.hwnd, icon.uid, icon.version, icon.callback, names.join(" ")));
    }
}

/// Tells running apps the tray exists, so they add their icons again.
pub fn broadcast_created() {
    // SAFETY: registers a message name and broadcasts it without waiting.
    unsafe {
        let m = RegisterWindowMessageW(w!("TaskbarCreated"));
        let ok = SendNotifyMessageW(HWND_BROADCAST, m, WPARAM(0), LPARAM(0)).is_ok();
        log::line(&format!("TaskbarCreated {m:#x} broadcast: {ok}"));
    }
}

/// An icon handle (from any process of the session) drawn at `px` into straight-alpha BGRA.
pub fn icon_image(hicon: isize, px: i32) -> Option<shelldata::Image> {
    if hicon == 0 {
        return None;
    }
    let draw = |flags| -> Option<Vec<u8>> {
        // SAFETY: a DIB section we create, draw into and free on every path.
        unsafe {
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: px,
                biHeight: -px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bmp.into());
            let ok = DrawIconEx(dc, 0, 0, HICON(hicon as _), px, px, 0, None, flags).is_ok();
            let out = ok.then(|| std::slice::from_raw_parts(bits as *const u8, (px * px * 4) as usize).to_vec());
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            out
        }
    };
    let mut bgra = draw(DI_NORMAL)?;
    if bgra.chunks_exact(4).all(|p| p[3] == 0) {
        // An icon without alpha: opaque wherever it drew something (its mask is applied by GDI).
        let mask = draw(windows::Win32::UI::WindowsAndMessaging::DI_MASK)?;
        for (p, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            p[3] = if m[0] == 0 { 255 } else { 0 };
        }
    } else {
        for p in bgra.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
    Some(shelldata::Image { width: px as u32, height: px as u32, bgra })
}
