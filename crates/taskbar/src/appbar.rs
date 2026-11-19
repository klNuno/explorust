//! AppBars (`SHAppBarMessage`, WM_COPYDATA dwData 0 on `Shell_TrayWnd`): the command as shell32
//! sends it, the registered bars, where each may dock (QUERYPOS and SETPOS) and the work area
//! they leave. Pure logic, tested on any OS.

pub const ABM_NEW: u32 = 0;
pub const ABM_REMOVE: u32 = 1;
pub const ABM_QUERYPOS: u32 = 2;
pub const ABM_SETPOS: u32 = 3;
pub const ABM_GETSTATE: u32 = 4;
pub const ABM_GETTASKBARPOS: u32 = 5;
pub const ABM_ACTIVATE: u32 = 6;
pub const ABM_GETAUTOHIDEBAR: u32 = 7;
pub const ABM_SETAUTOHIDEBAR: u32 = 8;
pub const ABM_WINDOWPOSCHANGED: u32 = 9;
pub const ABM_SETSTATE: u32 = 10;
pub const ABM_GETAUTOHIDEBAREX: u32 = 11;
pub const ABM_SETAUTOHIDEBAREX: u32 = 12;

pub const ABN_STATECHANGE: u32 = 0;
pub const ABN_POSCHANGED: u32 = 1;
pub const ABN_FULLSCREENAPP: u32 = 2;

pub const ABS_AUTOHIDE: u32 = 1;
pub const ABS_ALWAYSONTOP: u32 = 2;

pub const ABE_LEFT: u32 = 0;
pub const ABE_TOP: u32 = 1;
pub const ABE_RIGHT: u32 = 2;
pub const ABE_BOTTOM: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    /// True when `self` covers all of `other` (a fullscreen window over its monitor).
    pub fn covers(&self, other: &Rect) -> bool {
        self.left <= other.left && self.top <= other.top && self.right >= other.right && self.bottom >= other.bottom
    }
}

/// One `SHAppBarMessage` call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub msg: u32,
    pub hwnd: isize,
    pub callback: u32,
    pub edge: u32,
    pub rc: Rect,
    pub lparam: i64,
    /// Shared memory (`SHAllocShared` in the sender) where results go, and its owner process.
    pub shared: isize,
    pub pid: u32,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Byte offset of the APPBARDATA rect inside the shared memory block, for the result.
pub const SHARED_RC: usize = 16;
pub const SHARED_EDGE: usize = 12;

/// Parses the WM_COPYDATA payload: APPBARDATA3264 (cbSize, hWnd, uCallbackMessage, uEdge, rc,
/// lParam on 8 bytes), then dwMessage, the shared memory handle and the sender's process id. The
/// handle is a DWORD at 44 in the 56-byte form and 8 bytes at 48 in the 64-byte one.
pub fn parse(data: &[u8]) -> Option<Message> {
    let lp = u32_at(data, 32)? as u64 | (u32_at(data, 36)? as u64) << 32;
    let (shared, pid) = if data.len() >= 64 {
        let lo = u32_at(data, 48)? as u64 | (u32_at(data, 52)? as u64) << 32;
        (lo as i64 as isize, u32_at(data, 56)?)
    } else {
        (u32_at(data, 44)? as i32 as isize, u32_at(data, 48)?)
    };
    Some(Message {
        hwnd: u32_at(data, 4)? as i32 as isize,
        callback: u32_at(data, 8)?,
        edge: u32_at(data, 12)?,
        rc: Rect::new(u32_at(data, 16)? as i32, u32_at(data, 20)? as i32, u32_at(data, 24)? as i32, u32_at(data, 28)? as i32),
        lparam: lp as i64,
        msg: u32_at(data, 40)?,
        shared,
        pid,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub hwnd: isize,
    pub callback: u32,
    pub edge: u32,
    /// Its docked rect once it called SETPOS.
    pub rc: Option<Rect>,
}

#[derive(Default, Debug)]
pub struct AppBars {
    pub bars: Vec<Bar>,
    /// The autohide bar of each edge (left, top, right, bottom).
    pub autohide: [Option<isize>; 4],
}

impl AppBars {
    pub fn add(&mut self, hwnd: isize, callback: u32) -> bool {
        if self.bars.iter().any(|b| b.hwnd == hwnd) {
            return false;
        }
        self.bars.push(Bar { hwnd, callback, edge: ABE_TOP, rc: None });
        true
    }

    pub fn remove(&mut self, hwnd: isize) -> bool {
        for a in &mut self.autohide {
            if *a == Some(hwnd) {
                *a = None;
            }
        }
        let n = self.bars.len();
        self.bars.retain(|b| b.hwnd != hwnd);
        n != self.bars.len()
    }

    /// The space left on `monitor` by the taskbar and the docked bars before `hwnd` (all when
    /// `hwnd` is not registered).
    fn free(&self, hwnd: Option<isize>, monitor: Rect, taskbar: Rect) -> Rect {
        let mut a = monitor;
        a.bottom = a.bottom.min(taskbar.top);
        for b in self.bars.iter().take_while(|b| Some(b.hwnd) != hwnd) {
            let Some(r) = b.rc else { continue };
            if self.autohide.contains(&Some(b.hwnd)) {
                continue;
            }
            match b.edge {
                ABE_LEFT => a.left = a.left.max(r.right),
                ABE_TOP => a.top = a.top.max(r.bottom),
                ABE_RIGHT => a.right = a.right.min(r.left),
                _ => a.bottom = a.bottom.min(r.top),
            }
        }
        a
    }

    /// QUERYPOS: `rc` moved inside the free space, keeping its thickness, against `edge`.
    pub fn query(&self, hwnd: isize, edge: u32, rc: Rect, monitor: Rect, taskbar: Rect) -> Rect {
        let registered = self.bars.iter().any(|b| b.hwnd == hwnd);
        let f = self.free(registered.then_some(hwnd), monitor, taskbar);
        let mut r = rc;
        match edge {
            ABE_LEFT | ABE_RIGHT => {
                let w = (rc.right - rc.left).max(0);
                if edge == ABE_LEFT {
                    r.left = r.left.max(f.left);
                    r.right = r.left + w;
                } else {
                    r.right = r.right.min(f.right);
                    r.left = r.right - w;
                }
                r.top = r.top.max(f.top);
                r.bottom = r.bottom.min(f.bottom);
            }
            _ => {
                let h = (rc.bottom - rc.top).max(0);
                if edge == ABE_TOP {
                    r.top = r.top.max(f.top);
                    r.bottom = r.top + h;
                } else {
                    r.bottom = r.bottom.min(f.bottom);
                    r.top = r.bottom - h;
                }
                r.left = r.left.max(f.left);
                r.right = r.right.min(f.right);
            }
        }
        r
    }

    /// SETPOS: docks the bar where QUERYPOS puts it. None when `hwnd` is not registered.
    pub fn set(&mut self, hwnd: isize, edge: u32, rc: Rect, monitor: Rect, taskbar: Rect) -> Option<Rect> {
        let r = self.query(hwnd, edge, rc, monitor, taskbar);
        let b = self.bars.iter_mut().find(|b| b.hwnd == hwnd)?;
        b.edge = edge;
        b.rc = Some(r);
        Some(r)
    }

    /// The work area: the monitor less the taskbar and every docked, not autohidden bar.
    pub fn work_area(&self, monitor: Rect, taskbar: Rect) -> Rect {
        self.free(None, monitor, taskbar)
    }

    /// SETAUTOHIDEBAR: registers (`on`) or drops the autohide bar of `edge`; false when another
    /// bar already holds that edge.
    pub fn set_autohide(&mut self, hwnd: isize, edge: u32, on: bool) -> bool {
        let Some(slot) = self.autohide.get_mut(edge as usize) else { return false };
        match (on, *slot) {
            (true, Some(h)) if h != hwnd => false,
            (true, _) => {
                *slot = Some(hwnd);
                true
            }
            (false, Some(h)) if h == hwnd => {
                *slot = None;
                true
            }
            (false, _) => false,
        }
    }

    pub fn autohide_bar(&self, edge: u32) -> Option<isize> {
        self.autohide.get(edge as usize).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect { left: 0, top: 0, right: 1280, bottom: 800 };
    const TB: Rect = Rect { left: 0, top: 752, right: 1280, bottom: 800 };

    #[test]
    fn parses_both_payload_sizes() {
        let mut b = vec![0u8; 64];
        let put = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut b, 0, 48);
        put(&mut b, 4, 0x2_01f8);
        put(&mut b, 8, 0x8100);
        put(&mut b, 12, ABE_TOP);
        put(&mut b, 24, 1280);
        put(&mut b, 28, 60);
        put(&mut b, 32, 1);
        put(&mut b, 40, ABM_SETPOS);
        put(&mut b, 48, 0x3c4);
        put(&mut b, 56, 4242);
        let m = parse(&b).unwrap();
        assert_eq!((m.msg, m.hwnd, m.callback, m.edge), (ABM_SETPOS, 0x201f8, 0x8100, ABE_TOP));
        assert_eq!((m.rc, m.lparam, m.shared, m.pid), (Rect::new(0, 0, 1280, 60), 1, 0x3c4, 4242));
        let mut s = b[..56].to_vec();
        put(&mut s, 44, 0x3c8);
        put(&mut s, 48, 77);
        let m = parse(&s).unwrap();
        assert_eq!((m.shared, m.pid), (0x3c8, 77));
        assert!(parse(&b[..30]).is_none());
    }

    #[test]
    fn docks_and_shrinks_the_work_area() {
        let mut a = AppBars::default();
        assert!(a.add(1, 0x8100));
        assert!(!a.add(1, 0x8100));
        assert_eq!(a.work_area(MON, TB), Rect::new(0, 0, 1280, 752));
        let top = a.set(1, ABE_TOP, Rect::new(0, 0, 1280, 60), MON, TB).unwrap();
        assert_eq!(top, Rect::new(0, 0, 1280, 60));
        assert_eq!(a.work_area(MON, TB), Rect::new(0, 60, 1280, 752));
        // A second bar on the same edge stacks below the first; a left bar stops above the taskbar.
        a.add(2, 0x8101);
        assert_eq!(a.query(2, ABE_TOP, Rect::new(0, 0, 1280, 40), MON, TB), Rect::new(0, 60, 1280, 100));
        a.add(3, 0x8102);
        assert_eq!(a.query(3, ABE_LEFT, Rect::new(0, 0, 200, 800), MON, TB), Rect::new(0, 60, 200, 752));
        // A bottom bar sits on the taskbar.
        assert_eq!(a.query(2, ABE_BOTTOM, Rect::new(0, 740, 1280, 800), MON, TB), Rect::new(0, 692, 1280, 752));
        assert!(a.remove(1));
        assert_eq!(a.work_area(MON, TB), Rect::new(0, 0, 1280, 752));
        assert_eq!(a.set(9, ABE_TOP, Rect::new(0, 0, 1, 1), MON, TB), None);
    }

    #[test]
    fn autohide_holds_one_bar_per_edge() {
        let mut a = AppBars::default();
        a.add(1, 0);
        a.add(2, 0);
        assert!(a.set_autohide(1, ABE_LEFT, true));
        assert!(!a.set_autohide(2, ABE_LEFT, true));
        assert_eq!(a.autohide_bar(ABE_LEFT), Some(1));
        a.set(1, ABE_LEFT, Rect::new(0, 0, 4, 800), MON, TB);
        assert_eq!(a.work_area(MON, TB), Rect::new(0, 0, 1280, 752), "autohide bars do not reserve space");
        assert!(!a.set_autohide(2, ABE_LEFT, false));
        assert!(a.set_autohide(1, ABE_LEFT, false));
        assert_eq!(a.autohide_bar(ABE_LEFT), None);
        assert!(!a.set_autohide(1, 9, true));
    }

    #[test]
    fn fullscreen_cover() {
        assert!(Rect::new(-8, -8, 1288, 808).covers(&MON));
        assert!(!Rect::new(0, 0, 1280, 752).covers(&MON));
    }
}
