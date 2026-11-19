//! The menu as data, independent of Windows: entries read from an HMENU, how the Windows 11 menu
//! arranges them (top row of icon buttons, hidden verbs, separators, `Show more options`), the
//! glyphs and shortcuts it shows per verb, and the layout in DIPs.

/// One entry read from the HMENU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The command id (`idCmdFirst` included), 0 for separators and submenus.
    pub id: u32,
    /// The text without `&` and without the part after a tab.
    pub label: String,
    /// The letter after `&`, lowercase.
    pub mnemonic: Option<char>,
    /// The shortcut text after a tab, or the one Windows 11 shows for the verb.
    pub shortcut: String,
    /// The canonical verb from `GetCommandString(GCS_VERBW)`, empty when the handler gives none.
    pub verb: String,
    pub kind: Kind,
    pub enabled: bool,
    pub checked: bool,
    pub radio: bool,
    pub default: bool,
    /// The submenu handle (an HMENU as an integer) for `Kind::Submenu`.
    pub submenu: isize,
    /// The item's bitmap as premultiplied BGRA (width, height, pixels), when it has one.
    pub bitmap: Option<(u32, u32, Vec<u8>)>,
    /// The item is drawn by its handler (`MFT_OWNERDRAW`): no text came with it.
    pub owner_draw: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Command,
    Separator,
    Submenu,
    /// `Show more options`: the classic menu.
    More,
}

/// Splits a menu text into its label, its mnemonic and the shortcut after a tab.
pub fn parse_label(raw: &str) -> (String, Option<char>, String) {
    let (text, shortcut) = match raw.split_once('\t') {
        Some((a, b)) => (a, b.trim().to_string()),
        None => (raw, String::new()),
    };
    let mut label = String::with_capacity(text.len());
    let mut mnemonic = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => label.push('&'),
                Some(n) => {
                    if mnemonic.is_none() {
                        mnemonic = n.to_lowercase().next();
                    }
                    label.push(n);
                }
                None => {}
            }
        } else {
            label.push(c);
        }
    }
    // "(&N)" style mnemonics of East Asian menus leave "()" behind.
    let mut label = label;
    if let Some(m) = mnemonic {
        for c in [m, m.to_ascii_uppercase()] {
            label = label.replace(&format!("({c})"), "");
        }
    }
    let label = label.trim().to_string();
    (label, mnemonic, shortcut)
}

/// The top row of Windows 11's file menu, left to right.
pub const TOP_VERBS: [&str; 5] = ["cut", "copy", "rename", "Windows.ModernShare", "delete"];

/// Segoe Fluent Icons glyph of a top-row verb.
pub fn top_glyph(verb: &str) -> char {
    match verb {
        "cut" => '\u{E8C6}',
        "copy" => '\u{E8C8}',
        "rename" => '\u{E8AC}',
        "Windows.ModernShare" => '\u{E72D}',
        "delete" => '\u{E74D}',
        "paste" => '\u{E77F}',
        _ => '\u{E10C}',
    }
}

/// The glyph Windows 11 shows beside a verb, `None` when the item keeps its own bitmap or none.
pub fn glyph(verb: &str) -> Option<char> {
    let v = verb.to_ascii_lowercase();
    Some(match v.as_str() {
        "open" | "opennewprocess" => '\u{E8E5}',
        "opennewwindow" => '\u{E8A7}',
        "opennewtab" => '\u{EC6C}',
        "openas" => '\u{E7AC}',
        "edit" => '\u{E70F}',
        "print" => '\u{E749}',
        "runas" | "runasuser" => '\u{E7EF}',
        "properties" => '\u{E90F}',
        "paste" => '\u{E77F}',
        "pastelink" => '\u{E71B}',
        "undo" => '\u{E7A7}',
        "redo" => '\u{E7A6}',
        "refresh" => '\u{E72C}',
        "copyaspath" => '\u{E8C8}',
        "link" => '\u{E71B}',
        "pintohome" | "pintostartscreen" | "pintotaskbar" => '\u{E718}',
        "unpinfromhome" | "unpinfromstart" => '\u{E77A}',
        "newfolder" => '\u{E8F4}',
        "windows.compresstozip" | "compressto" => '\u{F012}',
        "display" => '\u{E7F4}',
        "personalize" => '\u{E771}',
        "cut" | "copy" | "rename" | "delete" | "windows.modernshare" => top_glyph(verb),
        "find" => '\u{E721}',
        "mount" => '\u{E958}',
        "eject" => '\u{F847}',
        "format" => '\u{EDA2}',
        "setdesktopwallpaper" => '\u{E771}',
        "rotate90" => '\u{E7AD}',
        "rotate270" => '\u{E7AD}',
        _ => return None,
    })
}

/// The shortcut Windows 11 shows for a verb when the handler's text has none.
pub fn shortcut(verb: &str, default: bool) -> &'static str {
    match verb.to_ascii_lowercase().as_str() {
        "properties" => "Alt+Enter",
        "copyaspath" => "Ctrl+Shift+C",
        "paste" => "Ctrl+V",
        "undo" => "Ctrl+Z",
        "redo" => "Ctrl+Y",
        "refresh" => "F5",
        _ if default => "Enter",
        _ => "",
    }
}

/// Verbs the Windows 11 menu leaves to `Show more options` (their place in the top row aside).
pub fn hidden(verb: &str) -> bool {
    matches!(
        verb.to_ascii_lowercase().as_str(),
        "cut" | "copy" | "rename" | "delete" | "windows.modernshare" | "link" | "sharing" | "restore previous versions"
            | "previousversions" | "sendto" | "includeinlibrary" | "pintohomefile" | "pintotaskbar"
    )
}

/// The Windows 11 menu built from the classic entries: the top-row commands (verb, id, enabled)
/// and the list, with hidden verbs dropped, separators tidied and `Show more options` last.
pub fn arrange(entries: &[Entry], top_row: bool, more_label: &str) -> (Vec<(String, u32)>, Vec<Entry>) {
    let mut top = Vec::new();
    if top_row {
        for v in TOP_VERBS {
            if let Some(e) = entries.iter().find(|e| e.kind == Kind::Command && e.verb.eq_ignore_ascii_case(v) && e.enabled) {
                top.push((v.to_string(), e.id));
            }
        }
    }
    let mut list: Vec<Entry> = Vec::new();
    for e in entries {
        if e.kind == Kind::Command && (hidden(&e.verb) && (top_row || !e.verb.eq_ignore_ascii_case("rename"))) {
            continue;
        }
        if e.kind == Kind::Command && e.label.is_empty() && !e.owner_draw {
            continue;
        }
        let mut e = e.clone();
        if e.kind == Kind::Command && e.shortcut.is_empty() {
            e.shortcut = shortcut(&e.verb, e.default).to_string();
        }
        list.push(e);
    }
    let mut list = tidy(list);
    if !list.is_empty() {
        list.push(Entry { kind: Kind::Separator, ..Default::default() });
    }
    list.push(Entry {
        kind: Kind::More,
        label: more_label.to_string(),
        shortcut: "Shift+F10".to_string(),
        enabled: true,
        ..Default::default()
    });
    (top, list)
}

/// Drops leading, trailing and doubled separators.
pub fn tidy(list: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(list.len());
    for e in list {
        if e.kind == Kind::Separator && out.last().is_none_or(|l| l.kind == Kind::Separator) {
            continue;
        }
        out.push(e);
    }
    while out.last().is_some_and(|l| l.kind == Kind::Separator) {
        out.pop();
    }
    out
}

/// `Show more options` in the UI language (a LANGID's primary language, 0x0C French).
pub fn more_label(langid: u16) -> &'static str {
    if langid & 0x3FF == 0x0C { "Afficher d\u{2019}autres options" } else { "Show more options" }
}

/// Metrics of the Windows 11 menu at 100% scale, in DIPs.
pub mod metrics {
    /// Space above the first row and below the last.
    pub const PAD: f32 = 4.0;
    pub const ITEM_H: f32 = 32.0;
    pub const SEP_H: f32 = 9.0;
    pub const TOP_H: f32 = 48.0;
    pub const TOP_BUTTON: f32 = 40.0;
    /// Inset of the hover highlight from the menu's sides.
    pub const INSET: f32 = 4.0;
    pub const ICON_X: f32 = 16.0;
    pub const ICON: f32 = 16.0;
    pub const TEXT_X: f32 = 48.0;
    pub const RIGHT: f32 = 16.0;
    pub const CHEVRON_W: f32 = 24.0;
    pub const SHORTCUT_GAP: f32 = 40.0;
    pub const MIN_W: f32 = 220.0;
    pub const MAX_W: f32 = 560.0;
    pub const RADIUS: f32 = 4.0;
}

/// Where each row sits, in DIPs from the menu's top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    /// Top row buttons, (x, y, w, h).
    pub top: Vec<(f32, f32, f32, f32)>,
    /// Per entry, (y, h).
    pub rows: Vec<(f32, f32)>,
}

/// Lays out `n_top` top buttons and the entries, given each entry's measured label and shortcut
/// widths.
pub fn layout(n_top: usize, entries: &[Entry], widths: &[(f32, f32)]) -> Layout {
    use metrics::*;
    let mut width = MIN_W;
    for (e, &(lw, sw)) in entries.iter().zip(widths) {
        let mut w = TEXT_X + lw + RIGHT;
        if sw > 0.0 {
            w += SHORTCUT_GAP + sw;
        }
        if e.kind == Kind::Submenu {
            w += CHEVRON_W;
        }
        width = width.max(w);
    }
    let width = width.min(MAX_W).ceil();
    let mut y = PAD;
    let mut top = Vec::new();
    if n_top > 0 {
        let total = n_top as f32 * TOP_BUTTON;
        let gap = ((width - 2.0 * INSET - total) / (n_top as f32 + 1.0)).max(0.0);
        for i in 0..n_top {
            let x = INSET + gap + i as f32 * (TOP_BUTTON + gap);
            top.push((x.round(), y + (TOP_H - TOP_BUTTON) / 2.0 - 2.0, TOP_BUTTON, TOP_BUTTON - 4.0));
        }
        y += TOP_H - 4.0;
        // The line under the top row.
        y += SEP_H - 4.0;
    }
    let mut rows = Vec::with_capacity(entries.len());
    for e in entries {
        let h = if e.kind == Kind::Separator { SEP_H } else { ITEM_H };
        rows.push((y, h));
        y += h;
    }
    Layout { width, height: (y + PAD).ceil(), top, rows }
}

/// What a point in the menu hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Top(usize),
    Row(usize),
}

pub fn hit(l: &Layout, entries: &[Entry], x: f32, y: f32) -> Option<Hit> {
    if x < 0.0 || x >= l.width || y < 0.0 || y >= l.height {
        return None;
    }
    for (i, &(bx, by, bw, bh)) in l.top.iter().enumerate() {
        if x >= bx && x < bx + bw && y >= by && y < by + bh {
            return Some(Hit::Top(i));
        }
    }
    for (i, &(ry, rh)) in l.rows.iter().enumerate() {
        if y >= ry && y < ry + rh {
            return (entries[i].kind != Kind::Separator).then_some(Hit::Row(i));
        }
    }
    None
}

/// The next selectable row from `from` in direction `step` (+1 or -1), wrapping around.
pub fn next_row(entries: &[Entry], from: Option<usize>, step: i32) -> Option<usize> {
    let n = entries.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(f) => f as i32,
        None if step > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + step).rem_euclid(n);
        let e = &entries[i as usize];
        if e.kind != Kind::Separator && e.enabled {
            return Some(i as usize);
        }
    }
    None
}

/// The rows whose mnemonic is `c`, else whose label starts with it.
pub fn by_mnemonic(entries: &[Entry], c: char) -> Vec<usize> {
    let c = c.to_lowercase().next().unwrap_or(c);
    let m: Vec<usize> =
        entries.iter().enumerate().filter(|(_, e)| e.enabled && e.mnemonic == Some(c)).map(|(i, _)| i).collect();
    if !m.is_empty() {
        return m;
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.enabled && e.kind != Kind::Separator && e.label.to_lowercase().starts_with(c))
        .map(|(i, _)| i)
        .collect()
}

/// Where a menu of size (w, h) opens at point (x, y) inside the work area (l, t, r, b): below and
/// right of the point, flipped left or up when it does not fit.
pub fn place(x: i32, y: i32, w: i32, h: i32, area: (i32, i32, i32, i32)) -> (i32, i32) {
    let (l, t, r, b) = area;
    let px = if x + w > r { (x - w).max(l) } else { x.max(l) };
    let py = if y + h > b { (y - h).max(t) } else { y.max(t) };
    (px, py)
}

/// Where a submenu of size (w, h) opens beside its parent row (row rect in screen pixels).
pub fn place_sub(row: (i32, i32, i32, i32), w: i32, h: i32, area: (i32, i32, i32, i32), overlap: i32) -> (i32, i32) {
    let (rl, rt, rr, _) = row;
    let (l, t, r, b) = area;
    let x = if rr - overlap + w <= r { rr - overlap } else { (rl + overlap - w).max(l) };
    let y = (rt).min(b - h).max(t);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: u32, label: &str, verb: &str) -> Entry {
        let (label, mnemonic, shortcut) = parse_label(label);
        Entry { id, label, mnemonic, shortcut, verb: verb.into(), enabled: true, ..Default::default() }
    }

    fn sep() -> Entry {
        Entry { kind: Kind::Separator, ..Default::default() }
    }

    #[test]
    fn labels() {
        assert_eq!(parse_label("&Open"), ("Open".into(), Some('o'), String::new()));
        assert_eq!(parse_label("Undo &Rename\tCtrl+Z"), ("Undo Rename".into(), Some('r'), "Ctrl+Z".into()));
        assert_eq!(parse_label("Tom && Jerry"), ("Tom & Jerry".into(), None, String::new()));
        assert_eq!(parse_label("新建(&W)"), ("新建".into(), Some('w'), String::new()));
    }

    #[test]
    fn arrange_file_menu() {
        let mut open = cmd(1, "&Open", "open");
        open.default = true;
        let entries = vec![
            open,
            sep(),
            cmd(2, "Cu&t", "cut"),
            cmd(3, "&Copy", "copy"),
            sep(),
            cmd(4, "Create &shortcut", "link"),
            cmd(5, "&Delete", "delete"),
            cmd(6, "Rena&me", "rename"),
            sep(),
            cmd(7, "P&roperties", "properties"),
        ];
        let (top, list) = arrange(&entries, true, "Show more options");
        assert_eq!(top, vec![("cut".into(), 2), ("copy".into(), 3), ("rename".into(), 6), ("delete".into(), 5)]);
        let labels: Vec<&str> = list.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Open", "", "Properties", "", "Show more options"]);
        assert_eq!(list[0].shortcut, "Enter");
        assert_eq!(list[2].shortcut, "Alt+Enter");
        assert_eq!(list[4].kind, Kind::More);
    }

    #[test]
    fn background_keeps_its_verbs() {
        let entries = vec![cmd(1, "&Paste", "paste"), sep(), sep(), cmd(2, "&Undo Rename\tCtrl+Z", "undo")];
        let (top, list) = arrange(&entries, false, "Show more options");
        assert!(top.is_empty());
        assert_eq!(list.len(), 5);
        assert_eq!(list[1].kind, Kind::Separator);
        assert_eq!(list[2].shortcut, "Ctrl+Z");
    }

    #[test]
    fn layout_and_hits() {
        let entries = vec![cmd(1, "Open", "open"), sep(), cmd(2, "Properties", "properties")];
        let l = layout(2, &entries, &[(40.0, 0.0), (0.0, 0.0), (70.0, 40.0)]);
        assert_eq!(l.width, metrics::MIN_W);
        assert_eq!(l.top.len(), 2);
        let (y0, h0) = l.rows[0];
        assert_eq!(hit(&l, &entries, 100.0, y0 + h0 / 2.0), Some(Hit::Row(0)));
        let (y1, h1) = l.rows[1];
        assert_eq!(hit(&l, &entries, 100.0, y1 + h1 / 2.0), None);
        let (bx, by, _, _) = l.top[1];
        assert_eq!(hit(&l, &entries, bx + 1.0, by + 1.0), Some(Hit::Top(1)));
        let wide = layout(0, &entries, &[(600.0, 0.0), (0.0, 0.0), (70.0, 200.0)]);
        assert_eq!(wide.width, metrics::MAX_W);
    }

    #[test]
    fn keyboard() {
        let mut off = cmd(3, "Off", "x");
        off.enabled = false;
        let entries = vec![cmd(1, "&Open", "open"), sep(), off, cmd(2, "&Properties", "properties")];
        assert_eq!(next_row(&entries, None, 1), Some(0));
        assert_eq!(next_row(&entries, Some(0), 1), Some(3));
        assert_eq!(next_row(&entries, Some(3), 1), Some(0));
        assert_eq!(next_row(&entries, None, -1), Some(3));
        assert_eq!(by_mnemonic(&entries, 'P'), vec![3]);
    }

    #[test]
    fn placement() {
        let area = (0, 0, 1280, 760);
        assert_eq!(place(100, 100, 300, 400, area), (100, 100));
        assert_eq!(place(1200, 700, 300, 400, area), (900, 300));
        assert_eq!(place_sub((100, 100, 400, 132), 250, 300, area, 4), (396, 100));
        assert_eq!(place_sub((1000, 600, 1270, 632), 250, 300, area, 4), (754, 460));
    }
}
