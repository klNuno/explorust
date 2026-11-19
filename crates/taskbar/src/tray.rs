//! The notification area's data: `Shell_NotifyIcon` calls as they arrive in `WM_COPYDATA`
//! (dwData 1), the icon list they build, and the callback messages each icon gets for a mouse or
//! keyboard event, following its `NOTIFYICON_VERSION`. Pure logic, tested on any OS.

pub const NIM_ADD: u32 = 0;
pub const NIM_MODIFY: u32 = 1;
pub const NIM_DELETE: u32 = 2;
pub const NIM_SETFOCUS: u32 = 3;
pub const NIM_SETVERSION: u32 = 4;

pub const NIF_MESSAGE: u32 = 0x01;
pub const NIF_ICON: u32 = 0x02;
pub const NIF_TIP: u32 = 0x04;
pub const NIF_STATE: u32 = 0x08;
pub const NIF_INFO: u32 = 0x10;
pub const NIF_GUID: u32 = 0x20;
pub const NIF_SHOWTIP: u32 = 0x80;

pub const NIS_HIDDEN: u32 = 0x01;
pub const NIS_SHAREDICON: u32 = 0x02;

pub const WM_CONTEXTMENU: u32 = 0x007B;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const NIN_SELECT: u32 = 0x0400;
pub const NIN_KEYSELECT: u32 = 0x0401;
pub const NIN_POPUPOPEN: u32 = 0x0406;
pub const NIN_POPUPCLOSE: u32 = 0x0407;

/// `TRAYNOTIFYDATAW.dwSignature`.
pub const SIGNATURE: u32 = 0x3475_3423;

/// One `Shell_NotifyIcon` call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notify {
    pub msg: u32,
    pub hwnd: isize,
    pub uid: u32,
    pub flags: u32,
    pub callback: u32,
    pub hicon: isize,
    pub tip: String,
    pub state: u32,
    pub state_mask: u32,
    pub info: String,
    /// `uTimeout` or `uVersion` (a union).
    pub version: u32,
    pub info_title: String,
    pub info_flags: u32,
    pub guid: Option<[u8; 16]>,
    /// Which layout the sender used: 32 (handles as DWORDs, what shell32 sends) or 64.
    pub layout: u8,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// A NUL-terminated UTF-16 string of at most `chars` units at `o`; empty when out of range.
fn wstr_at(b: &[u8], o: usize, chars: usize) -> String {
    let Some(raw) = b.get(o..(o + chars * 2).min(b.len())) else { return String::new() };
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Field offsets of NOTIFYICONDATAW from its start, per layout.
struct Offsets {
    hwnd: usize,
    uid: usize,
    flags: usize,
    callback: usize,
    hicon: usize,
    tip: usize,
    state: usize,
    info: usize,
    version: usize,
    info_title: usize,
    info_flags: usize,
    guid: usize,
    wide_handles: bool,
}

/// shell32 copies every caller's NOTIFYICONDATAW into this 32/64-neutral shape (956 bytes).
const L32: Offsets = Offsets {
    hwnd: 4, uid: 8, flags: 12, callback: 16, hicon: 20, tip: 24, state: 280, info: 288, version: 800,
    info_title: 804, info_flags: 932, guid: 936, wide_handles: false,
};
/// The native x64 NOTIFYICONDATAW (976 bytes), handles on 8 bytes.
const L64: Offsets = Offsets {
    hwnd: 8, uid: 16, flags: 20, callback: 24, hicon: 32, tip: 40, state: 296, info: 304, version: 816,
    info_title: 820, info_flags: 948, guid: 952, wide_handles: true,
};
#[cfg(test)]
const SIZE32: usize = 956;
const SIZE64: usize = 976;

/// Parses the WM_COPYDATA payload of `Shell_NotifyIcon`: signature, message, NOTIFYICONDATA.
pub fn parse(data: &[u8]) -> Option<Notify> {
    if u32_at(data, 0)? != SIGNATURE {
        return None;
    }
    let msg = u32_at(data, 4)?;
    let nid = data.get(8..)?;
    // shell32 sends the neutral layout (956 bytes, 1476 on Windows 11 26300, which appends
    // fields); a native x64 copy is exactly 976 bytes and says so in its cbSize.
    let native64 = nid.len() == SIZE64 && u32_at(nid, 0) == Some(SIZE64 as u32);
    let (o, layout) = if native64 { (&L64, 64) } else { (&L32, 32) };
    let handle = |off: usize| -> Option<isize> {
        Some(if o.wide_handles { u64_at(nid, off)? as i64 as isize } else { u32_at(nid, off)? as i32 as isize })
    };
    let flags = u32_at(nid, o.flags)?;
    let guid = (flags & NIF_GUID != 0).then(|| nid.get(o.guid..o.guid + 16).and_then(|g| g.try_into().ok())).flatten();
    Some(Notify {
        msg,
        hwnd: handle(o.hwnd)?,
        uid: u32_at(nid, o.uid)?,
        flags,
        callback: u32_at(nid, o.callback).unwrap_or(0),
        hicon: handle(o.hicon).unwrap_or(0),
        tip: wstr_at(nid, o.tip, 128),
        state: u32_at(nid, o.state).unwrap_or(0),
        state_mask: u32_at(nid, o.state + 4).unwrap_or(0),
        info: wstr_at(nid, o.info, 256),
        version: u32_at(nid, o.version).unwrap_or(0),
        info_title: wstr_at(nid, o.info_title, 64),
        info_flags: u32_at(nid, o.info_flags).unwrap_or(0),
        guid,
        layout,
    })
}

/// One icon of the notification area.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub hwnd: isize,
    pub uid: u32,
    pub guid: Option<[u8; 16]>,
    pub callback: u32,
    pub hicon: isize,
    pub tip: String,
    pub hidden: bool,
    pub shared: bool,
    pub show_tip: bool,
    pub version: u32,
    /// Shown beside the clock (promoted) rather than behind the chevron.
    pub promoted: bool,
}

impl Icon {
    fn matches(&self, n: &Notify) -> bool {
        match (n.guid, self.guid) {
            (Some(g), Some(m)) => g == m,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => self.hwnd == n.hwnd && self.uid == n.uid,
        }
    }
}

/// What a call changed, for the caller to redraw, load icons or log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added(usize),
    Modified(usize),
    Removed,
    /// SETVERSION or SETFOCUS on a known icon.
    Done(usize),
    Failed,
}

#[derive(Default, Debug)]
pub struct Tray {
    pub icons: Vec<Icon>,
}

impl Tray {
    pub fn find(&self, n: &Notify) -> Option<usize> {
        self.icons.iter().position(|i| i.matches(n))
    }

    /// Applies one call; `promoted` says whether a new icon shows beside the clock.
    pub fn apply(&mut self, n: &Notify, promoted: bool) -> Change {
        let at = self.find(n);
        match (n.msg, at) {
            (NIM_ADD, None) => {
                let mut icon = Icon {
                    hwnd: n.hwnd,
                    uid: n.uid,
                    guid: n.guid,
                    callback: 0,
                    hicon: 0,
                    tip: String::new(),
                    hidden: false,
                    shared: false,
                    show_tip: false,
                    version: 0,
                    promoted,
                };
                update(&mut icon, n);
                self.icons.push(icon);
                Change::Added(self.icons.len() - 1)
            }
            (NIM_MODIFY, Some(i)) => {
                update(&mut self.icons[i], n);
                Change::Modified(i)
            }
            (NIM_DELETE, Some(i)) => {
                self.icons.remove(i);
                Change::Removed
            }
            (NIM_SETVERSION, Some(i)) => {
                self.icons[i].version = n.version;
                Change::Done(i)
            }
            (NIM_SETFOCUS, Some(i)) => Change::Done(i),
            _ => Change::Failed,
        }
    }

    /// Drops the icons of windows that no longer exist.
    pub fn retain(&mut self, alive: impl Fn(isize) -> bool) -> bool {
        let n = self.icons.len();
        self.icons.retain(|i| alive(i.hwnd));
        n != self.icons.len()
    }

    /// Indexes of the icons shown beside the clock, left to right.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.icons.len()).filter(|&i| !self.icons[i].hidden && self.icons[i].promoted).collect()
    }

    /// Indexes of the icons behind the chevron.
    pub fn overflow(&self) -> Vec<usize> {
        (0..self.icons.len()).filter(|&i| !self.icons[i].hidden && !self.icons[i].promoted).collect()
    }
}

fn update(icon: &mut Icon, n: &Notify) {
    if n.guid.is_some() {
        icon.hwnd = n.hwnd;
        icon.uid = n.uid;
    }
    if n.flags & NIF_MESSAGE != 0 {
        icon.callback = n.callback;
    }
    if n.flags & NIF_ICON != 0 {
        icon.hicon = n.hicon;
    }
    if n.flags & NIF_TIP != 0 {
        icon.tip = n.tip.clone();
    }
    icon.show_tip = n.flags & NIF_SHOWTIP != 0;
    if n.flags & NIF_STATE != 0 {
        let m = n.state_mask;
        if m & NIS_HIDDEN != 0 {
            icon.hidden = n.state & NIS_HIDDEN != 0;
        }
        if m & NIS_SHAREDICON != 0 {
            icon.shared = n.state & NIS_SHAREDICON != 0;
        }
    }
}

/// A user action on an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Move,
    LeftDown,
    LeftUp,
    LeftDouble,
    RightDown,
    RightUp,
    MiddleDown,
    MiddleUp,
    /// The pointer rested on the icon (true) or left it (false).
    Hover(bool),
    /// Enter or Space with the keyboard focus on the icon.
    Key,
    /// Shift+F10 or the menu key.
    Menu,
}

/// The (wParam, lParam) pairs posted with the icon's callback message for `ev`, as explorer does:
/// version 0 gets the mouse message with wParam = uID; version 3 adds NIN_SELECT after a left
/// click and WM_CONTEXTMENU after a right click; version 4 packs (message, uID) in lParam and the
/// screen point in wParam, and gets NIN_POPUPOPEN and NIN_POPUPCLOSE when it draws its own tip.
pub fn messages(icon: &Icon, ev: Event, x: i32, y: i32) -> Vec<(usize, isize)> {
    let v = icon.version;
    let mut out: Vec<u32> = match ev {
        Event::Move => vec![WM_MOUSEMOVE],
        Event::LeftDown => vec![WM_LBUTTONDOWN],
        Event::LeftUp => vec![WM_LBUTTONUP],
        Event::LeftDouble => vec![WM_LBUTTONDBLCLK],
        Event::RightDown => vec![WM_RBUTTONDOWN],
        Event::RightUp => vec![WM_RBUTTONUP],
        Event::MiddleDown => vec![WM_MBUTTONDOWN],
        Event::MiddleUp => vec![WM_MBUTTONUP],
        Event::Hover(_) | Event::Key | Event::Menu => vec![],
    };
    if v >= 3 {
        match ev {
            Event::LeftUp => out.push(NIN_SELECT),
            Event::RightUp | Event::Menu => out.push(WM_CONTEXTMENU),
            Event::Key => out.push(NIN_KEYSELECT),
            _ => {}
        }
    } else if ev == Event::Menu {
        out.extend([WM_RBUTTONDOWN, WM_RBUTTONUP]);
    } else if ev == Event::Key {
        out.extend([WM_LBUTTONDOWN, WM_LBUTTONUP]);
    }
    if v >= 4 && !icon.show_tip {
        if let Event::Hover(open) = ev {
            out.push(if open { NIN_POPUPOPEN } else { NIN_POPUPCLOSE });
        }
    }
    out.into_iter()
        .map(|m| {
            if v >= 4 {
                let wp = ((x as u16 as u32) | ((y as u16 as u32) << 16)) as usize;
                let lp = (m & 0xffff) | (icon.uid << 16);
                (wp, lp as i32 as isize)
            } else {
                (icon.uid as usize, m as isize)
            }
        })
        .collect()
}

/// Balloon icon kind (`NIIF_*` low bits), for the log.
pub fn balloon_kind(flags: u32) -> &'static str {
    match flags & 0x0f {
        0 => "none",
        1 => "info",
        2 => "warning",
        3 => "error",
        4 => "user",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_wstr(b: &mut [u8], o: usize, s: &str) {
        for (i, u) in s.encode_utf16().enumerate() {
            b[o + i * 2..o + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
    }

    fn payload32(msg: u32, hwnd: u32, uid: u32, flags: u32) -> Vec<u8> {
        let mut b = vec![0u8; 8 + 1476];
        put_u32(&mut b, 0, SIGNATURE);
        put_u32(&mut b, 4, msg);
        put_u32(&mut b, 8, SIZE32 as u32);
        put_u32(&mut b, 8 + 4, hwnd);
        put_u32(&mut b, 8 + 8, uid);
        put_u32(&mut b, 8 + 12, flags);
        put_u32(&mut b, 8 + 16, 0x8123);
        put_u32(&mut b, 8 + 20, 0xfff0_0010);
        put_wstr(&mut b, 8 + 24, "Test tip");
        put_u32(&mut b, 8 + 800, 4);
        b
    }

    #[test]
    fn parses_the_32_bit_layout() {
        let b = payload32(NIM_ADD, 0x0002_01f8, 7, NIF_MESSAGE | NIF_ICON | NIF_TIP);
        let n = parse(&b).unwrap();
        assert_eq!(n.layout, 32);
        assert_eq!((n.msg, n.hwnd, n.uid, n.callback), (NIM_ADD, 0x201f8, 7, 0x8123));
        assert_eq!(n.hicon, 0xfff0_0010u32 as i32 as isize, "handles are sign-extended");
        assert_eq!(n.tip, "Test tip");
        assert_eq!(n.version, 4);
        assert!(parse(&b[..4]).is_none());
        let mut bad = b.clone();
        put_u32(&mut bad, 0, 1);
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn parses_the_64_bit_layout() {
        let mut b = vec![0u8; 8 + SIZE64];
        put_u32(&mut b, 0, SIGNATURE);
        put_u32(&mut b, 4, NIM_MODIFY);
        put_u32(&mut b, 8, SIZE64 as u32);
        b[8 + 8..8 + 16].copy_from_slice(&0x1234_5678u64.to_le_bytes());
        put_u32(&mut b, 8 + 16, 3);
        put_u32(&mut b, 8 + 20, NIF_TIP | NIF_GUID);
        put_wstr(&mut b, 8 + 40, "x64");
        b[8 + 952] = 0xab;
        let n = parse(&b).unwrap();
        assert_eq!((n.layout, n.hwnd, n.uid, n.tip.as_str()), (64, 0x1234_5678, 3, "x64"));
        assert_eq!(n.guid.unwrap()[0], 0xab);
    }

    #[test]
    fn add_modify_delete_and_version() {
        let mut t = Tray::default();
        let add = parse(&payload32(NIM_ADD, 10, 1, NIF_MESSAGE | NIF_ICON | NIF_TIP)).unwrap();
        assert_eq!(t.apply(&add, true), Change::Added(0));
        assert_eq!(t.apply(&add, true), Change::Failed, "a second ADD of the same icon fails");
        let mut m = add.clone();
        m.msg = NIM_MODIFY;
        m.flags = NIF_STATE;
        m.state = NIS_HIDDEN;
        m.state_mask = NIS_HIDDEN;
        assert_eq!(t.apply(&m, true), Change::Modified(0));
        assert!(t.visible().is_empty());
        m.state = 0;
        t.apply(&m, true);
        assert_eq!(t.visible(), vec![0]);
        let mut v = add.clone();
        v.msg = NIM_SETVERSION;
        v.version = 4;
        assert_eq!(t.apply(&v, true), Change::Done(0));
        assert_eq!(t.icons[0].version, 4);
        let other = parse(&payload32(NIM_ADD, 11, 1, NIF_ICON)).unwrap();
        t.apply(&other, false);
        assert_eq!(t.overflow(), vec![1]);
        let mut d = add.clone();
        d.msg = NIM_DELETE;
        assert_eq!(t.apply(&d, true), Change::Removed);
        assert_eq!(t.apply(&d, true), Change::Failed);
        assert!(t.retain(|h| h != 11));
        assert!(t.icons.is_empty());
    }

    #[test]
    fn callbacks_follow_the_version() {
        let mut i = Icon { hwnd: 1, uid: 5, guid: None, callback: 0x8001, hicon: 0, tip: String::new(), hidden: false, shared: false, show_tip: false, version: 0, promoted: true };
        assert_eq!(messages(&i, Event::LeftUp, 0, 0), vec![(5, WM_LBUTTONUP as isize)]);
        assert_eq!(messages(&i, Event::Hover(true), 0, 0), vec![]);
        i.version = 3;
        assert_eq!(messages(&i, Event::RightUp, 0, 0), vec![(5, WM_RBUTTONUP as isize), (5, WM_CONTEXTMENU as isize)]);
        i.version = 4;
        let m = messages(&i, Event::LeftUp, 1100, 770);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], (1100 | (770 << 16), (WM_LBUTTONUP | 5 << 16) as isize));
        assert_eq!(m[1].1, (NIN_SELECT | 5 << 16) as isize);
        assert_eq!(messages(&i, Event::Hover(true), 1, 2)[0].1, (NIN_POPUPOPEN | 5 << 16) as isize);
        i.show_tip = true;
        assert!(messages(&i, Event::Hover(true), 1, 2).is_empty());
        let neg = messages(&i, Event::Move, -5, 10);
        assert_eq!(neg[0].0 & 0xffff, 0xfffb);
    }
}
