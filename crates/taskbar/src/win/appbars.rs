//! The AppBar host on `Shell_TrayWnd`: `SHAppBarMessage` calls from WM_COPYDATA (dwData 0),
//! results written back into the sender's shared memory, ABN_ notifications, and the work area
//! the taskbar and the docked bars leave.

use super::log;
use crate::appbar::{self as ab, AppBars, Message, Rect};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Shell::{SHLockShared, SHUnlockShared};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW};

pub struct AppBarHost {
    pub bars: AppBars,
    /// The fullscreen state last told to the bars.
    pub fullscreen: bool,
    logged_raw: bool,
}

/// What the bar must do after a call.
#[derive(Default)]
pub struct Outcome {
    pub result: isize,
    pub work_area: bool,
}

pub fn to_rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

impl AppBarHost {
    pub fn new() -> AppBarHost {
        AppBarHost { bars: AppBars::default(), fullscreen: false, logged_raw: false }
    }

    pub fn on_copydata(&mut self, data: &[u8], monitor: Rect, taskbar: Rect) -> Outcome {
        if !self.logged_raw {
            self.logged_raw = true;
            let hex: Vec<String> = data.iter().take(72).map(|b| format!("{b:02x}")).collect();
            log::line(&format!("appbar payload {} bytes: {}", data.len(), hex.join("")));
        }
        let Some(m) = ab::parse(data) else {
            log::line(&format!("appbar: unreadable payload of {} bytes", data.len()));
            return Outcome::default();
        };
        let out = self.handle(&m, monitor, taskbar);
        log::line(&format!(
            "appbar {} hwnd {:#x} cb {:#x} edge {} rc {},{},{},{} lp {} pid {}: {}",
            name(m.msg), m.hwnd, m.callback, m.edge, m.rc.left, m.rc.top, m.rc.right, m.rc.bottom, m.lparam, m.pid, out.result
        ));
        out
    }

    fn handle(&mut self, m: &Message, monitor: Rect, taskbar: Rect) -> Outcome {
        let mut out = Outcome::default();
        match m.msg {
            ab::ABM_NEW => out.result = self.bars.add(m.hwnd, m.callback) as isize,
            ab::ABM_REMOVE => {
                if self.bars.remove(m.hwnd) {
                    out.result = 1;
                    out.work_area = true;
                    self.notify_all(m.hwnd, ab::ABN_POSCHANGED, 0);
                }
            }
            ab::ABM_QUERYPOS => {
                let r = self.bars.query(m.hwnd, m.edge, m.rc, monitor, taskbar);
                out.result = write_rect(m, r, None) as isize;
            }
            ab::ABM_SETPOS => {
                if let Some(r) = self.bars.set(m.hwnd, m.edge, m.rc, monitor, taskbar) {
                    out.result = write_rect(m, r, None) as isize;
                    out.work_area = true;
                    self.notify_all(m.hwnd, ab::ABN_POSCHANGED, 0);
                }
            }
            ab::ABM_GETSTATE => out.result = ab::ABS_ALWAYSONTOP as isize,
            ab::ABM_SETSTATE => {
                // The bar does not autohide yet: it stays always on top.
                out.result = 1;
            }
            ab::ABM_GETTASKBARPOS => out.result = write_rect(m, taskbar, Some(ab::ABE_BOTTOM)) as isize,
            ab::ABM_ACTIVATE | ab::ABM_WINDOWPOSCHANGED => out.result = 1,
            ab::ABM_GETAUTOHIDEBAR | ab::ABM_GETAUTOHIDEBAREX => {
                out.result = self.bars.autohide_bar(m.edge).unwrap_or(0);
            }
            ab::ABM_SETAUTOHIDEBAR | ab::ABM_SETAUTOHIDEBAREX => {
                out.result = self.bars.set_autohide(m.hwnd, m.edge, m.lparam != 0) as isize;
                out.work_area = out.result != 0;
            }
            _ => {}
        }
        out
    }

    /// Posts `code` to every bar but `except`.
    pub fn notify_all(&self, except: isize, code: u32, lp: isize) {
        for b in self.bars.bars.iter().filter(|b| b.hwnd != except && b.callback != 0) {
            // SAFETY: posting to a registered bar's window.
            let _ = unsafe { PostMessageW(Some(HWND(b.hwnd as _)), b.callback, WPARAM(code as usize), LPARAM(lp)) };
        }
    }

    /// Tells the bars when a window starts or stops covering the monitor (ABN_FULLSCREENAPP).
    pub fn set_fullscreen(&mut self, on: bool) -> bool {
        if on == self.fullscreen {
            return false;
        }
        self.fullscreen = on;
        log::line(&format!("fullscreen app: {on}"));
        self.notify_all(0, ab::ABN_FULLSCREENAPP, on as isize);
        true
    }

    /// Drops bars whose window died without ABM_REMOVE; true when one went.
    pub fn prune(&mut self) -> bool {
        let dead: Vec<isize> = self
            .bars
            .bars
            .iter()
            .map(|b| b.hwnd)
            // SAFETY: IsWindow only checks a handle.
            .filter(|h| !unsafe { IsWindow(Some(HWND(*h as _))) }.as_bool())
            .collect();
        for h in &dead {
            self.bars.remove(*h);
            log::line(&format!("appbar {h:#x} gone"));
        }
        !dead.is_empty()
    }
}

/// Writes `rc` (and the edge) into the APPBARDATA the sender shared; false when it cannot.
fn write_rect(m: &Message, rc: Rect, edge: Option<u32>) -> bool {
    if m.shared == 0 {
        return false;
    }
    // SAFETY: maps the sender's SHAllocShared block into this process, writes inside the
    // APPBARDATA3264 it holds, unmaps it.
    unsafe {
        let p = SHLockShared(HANDLE(m.shared as _), m.pid) as *mut u8;
        if p.is_null() {
            log::line(&format!("appbar: shared memory {:#x} of {} not mapped", m.shared, m.pid));
            return false;
        }
        let r = [rc.left, rc.top, rc.right, rc.bottom];
        std::ptr::copy_nonoverlapping(r.as_ptr() as *const u8, p.add(ab::SHARED_RC), 16);
        if let Some(e) = edge {
            std::ptr::copy_nonoverlapping(&e as *const u32 as *const u8, p.add(ab::SHARED_EDGE), 4);
        }
        let _ = SHUnlockShared(p as _);
    }
    true
}

fn name(msg: u32) -> &'static str {
    match msg {
        ab::ABM_NEW => "NEW",
        ab::ABM_REMOVE => "REMOVE",
        ab::ABM_QUERYPOS => "QUERYPOS",
        ab::ABM_SETPOS => "SETPOS",
        ab::ABM_GETSTATE => "GETSTATE",
        ab::ABM_GETTASKBARPOS => "GETTASKBARPOS",
        ab::ABM_ACTIVATE => "ACTIVATE",
        ab::ABM_GETAUTOHIDEBAR => "GETAUTOHIDEBAR",
        ab::ABM_SETAUTOHIDEBAR => "SETAUTOHIDEBAR",
        ab::ABM_WINDOWPOSCHANGED => "WINDOWPOSCHANGED",
        ab::ABM_SETSTATE => "SETSTATE",
        ab::ABM_GETAUTOHIDEBAREX => "GETAUTOHIDEBAREX",
        ab::ABM_SETAUTOHIDEBAREX => "SETAUTOHIDEBAREX",
        _ => "?",
    }
}
