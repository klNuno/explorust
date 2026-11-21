//! Geometry and hit targets of our chrome at 100% scale.

pub const TABSTRIP_H: f32 = 44.0;
pub const NAVBAR_BOTTOM: f32 = 92.0;
pub const CMDBAR_BOTTOM: f32 = 139.0;
/// Height of the chrome, the command bar's bottom line included.
pub const CHROME_H: f32 = CMDBAR_BOTTOM + 1.0;

pub const TAB_X: f32 = 12.0;
pub const TAB_Y: f32 = 8.0;
pub const TAB_W: f32 = 212.0;
pub const TAB_MIN_W: f32 = 108.0;
pub const TAB_RADIUS: f32 = 7.0;
pub const TAB_GAP: f32 = 4.0;
pub const TAB_CY: f32 = 24.0;
pub const TAB_ICON_DX: f32 = 8.0;
pub const TAB_TEXT_DX: f32 = 34.0;
pub const TAB_CLOSE_FROM_RIGHT: f32 = 17.5;
pub const NEW_TAB_DX: f32 = 22.0;
/// Caption buttons, width and centers from the right edge: minimize, maximize, close.
pub const CAPTION_W: f32 = 46.0;
pub const CAPTION_CX: [f32; 3] = [115.0, 69.0, 23.0];
pub const CAPTION_CY: f32 = 20.0;

pub const NAV_BTN_CX: [f32; 4] = [28.0, 68.0, 108.0, 148.0];
pub const NAV_BTN_W: f32 = 36.0;
pub const NAV_CY: f32 = 68.0;
pub const BOX_TOP: f32 = 52.0;
pub const BOX_H: f32 = 32.0;
pub const BOX_RADIUS: f32 = 6.0;
pub const ADDR_X: f32 = 176.0;
pub const ADDR_ICON_CX: f32 = 194.0;
pub const CRUMB_FIRST_CHEVRON_CX: f32 = 218.0;
pub const CRUMB_GAP: f32 = 17.0;
pub const RIGHT_PAD: f32 = 12.0;
pub const SEARCH_TEXT_PAD: f32 = 15.0;
pub const SEARCH_GLYPH_FROM_RIGHT: f32 = 32.0;

pub const CMD_CY: f32 = 116.0;
pub const CMD_SEPS: [f32; 2] = [115.0, 358.0];
pub const CMD_BTN_W: f32 = 34.0;
pub const CMD_H: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    New,
    Cut,
    Copy,
    Paste,
    Rename,
    Share,
    Delete,
    Sort,
    View,
    More,
    Details,
}

/// Icon buttons of the command bar: command, glyph, center x.
pub const CMD_ICONS: [(Cmd, char, f32); 6] = [
    (Cmd::Cut, '\u{E8C6}', 140.0),
    (Cmd::Copy, '\u{E8C8}', 178.0),
    (Cmd::Paste, '\u{E77F}', 216.0),
    (Cmd::Rename, '\u{E8AC}', 254.0),
    (Cmd::Share, '\u{E72D}', 292.0),
    (Cmd::Delete, '\u{E74D}', 330.0),
];
/// Labelled buttons: command, glyph, glyph cx, label x, chevron cx, left, right.
pub const CMD_LABELLED: [(Cmd, char, f32, f32, f32, f32, f32); 3] = [
    (Cmd::New, '\u{E8F4}', 27.0, 44.0, 0.0, 10.0, 107.0),
    (Cmd::Sort, '\u{E8CB}', 379.0, 396.0, 428.0, 362.0, 441.0),
    (Cmd::View, '\u{E8FD}', 460.0, 477.0, 526.0, 443.0, 538.0),
];
pub const CMD_MORE_CX: f32 = 560.0;
/// Details pane toggle, from the right edge: glyph cx, label x, left edge.
pub const CMD_DETAILS: (f32, f32, f32) = (77.0, 61.0, 95.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Caption,
    Min,
    Max,
    Close,
    Tab(usize),
    TabClose(usize),
    NewTab,
    TabsBack,
    TabsForward,
    Back,
    Forward,
    Up,
    Refresh,
    /// The address box outside any crumb: edit mode.
    Address,
    Crumb(usize),
    Search,
    Cmd(Cmd),
}

/// Width of each tab when `n` tabs share a window `w` wide, as Explorer shrinks them.
pub struct TabStrip {
    pub visible: std::ops::Range<usize>,
    pub width: f32,
    pub plus: f32,
    pub left: Option<f32>,
    pub right: Option<f32>,
}

pub fn tab_strip(w: f32, n: usize, active: usize) -> TabStrip {
    let room = w - TAB_X - 3.0 * CAPTION_W - 2.0 * NEW_TAB_DX;
    let overflow = n as f32 * TAB_MIN_W > room;
    let room = room - if overflow { 64.0 } else { 0.0 };
    let count = ((room / TAB_MIN_W) as usize).clamp(1, n.max(1));
    let width = (room / count as f32).min(TAB_W);
    let start = (active + 1).saturating_sub(count);
    let end = (start + count).min(n);
    let plus = TAB_X + (end - start) as f32 * width + NEW_TAB_DX;
    TabStrip {
        visible: start..end,
        width,
        plus,
        left: (start > 0).then_some(plus + 30.0),
        right: (end < n).then_some(plus + 60.0),
    }
}

pub fn search_left(w: f32) -> f32 {
    w - (w * 0.28).clamp(140.0, 292.0) - RIGHT_PAD
}

pub fn compact_commands(w: f32) -> bool {
    w < 680.0
}

pub fn command_more_x(w: f32) -> f32 {
    if compact_commands(w) { 462.0 } else { CMD_MORE_CX }
}

pub fn label_bounds(w: f32, cmd: Cmd, center: f32, left: f32, right: f32) -> (f32, f32, f32) {
    if compact_commands(w) {
        match cmd {
            Cmd::Sort => (380.0, 363.0, 397.0),
            Cmd::View => (422.0, 405.0, 439.0),
            _ => (center, left, right),
        }
    } else {
        (center, left, right)
    }
}

fn within(v: f32, center: f32, width: f32) -> bool {
    (v - center).abs() <= width / 2.0
}

/// What the point (x, y) hits in a window `w` wide with `tabs` tabs; `crumbs` are the x spans of
/// the breadcrumb's items as last drawn.
pub fn hit(x: f32, y: f32, w: f32, tabs: usize, active: usize, crumbs: &[(f32, f32)]) -> Hit {
    if x < 0.0 || y < 0.0 || x >= w || y >= CHROME_H {
        return Hit::None;
    }
    if y < TABSTRIP_H {
        for (i, h) in [Hit::Min, Hit::Max, Hit::Close].into_iter().enumerate() {
            if y < TABSTRIP_H - 8.0 && within(x, w - CAPTION_CX[i], CAPTION_W) {
                return h;
            }
        }
        let strip = tab_strip(w, tabs, active);
        if y >= TAB_Y && y < TABSTRIP_H - 4.0 {
            for (slot, i) in strip.visible.enumerate() {
                let x0 = TAB_X + slot as f32 * strip.width;
                if x >= x0 && x < x0 + strip.width - TAB_GAP {
                    let close = x0 + strip.width - TAB_GAP - TAB_CLOSE_FROM_RIGHT;
                    return if within(x, close, 24.0) { Hit::TabClose(i) } else { Hit::Tab(i) };
                }
            }
            if within(x, strip.plus, 32.0) {
                return Hit::NewTab;
            }
            if strip.left.is_some_and(|x0| within(x, x0, 28.0)) {
                return Hit::TabsBack;
            }
            if strip.right.is_some_and(|x0| within(x, x0, 28.0)) {
                return Hit::TabsForward;
            }
        }
        return Hit::Caption;
    }
    if y < NAVBAR_BOTTOM {
        if y < BOX_TOP || y >= BOX_TOP + BOX_H {
            return Hit::None;
        }
        for (i, h) in [Hit::Back, Hit::Forward, Hit::Up, Hit::Refresh].into_iter().enumerate() {
            if within(x, NAV_BTN_CX[i], NAV_BTN_W) {
                return h;
            }
        }
        let search = search_left(w);
        if x >= ADDR_X && x < search - 8.0 {
            if let Some(i) = crumbs.iter().position(|&(a, b)| x >= a && x < b) {
                return Hit::Crumb(i);
            }
            return Hit::Address;
        }
        if x >= search && x < w - RIGHT_PAD {
            return Hit::Search;
        }
        return Hit::None;
    }
    if !within(y, CMD_CY, CMD_H) {
        return Hit::None;
    }
    for (c, _, cx) in CMD_ICONS {
        if within(x, cx, CMD_BTN_W) {
            return Hit::Cmd(c);
        }
    }
    for (c, _, cx, _, _, l, r) in CMD_LABELLED {
        let (_, l, r) = label_bounds(w, c, cx, l, r);
        if x >= l && x < r {
            return Hit::Cmd(c);
        }
    }
    if within(x, command_more_x(w), CMD_BTN_W) {
        return Hit::Cmd(Cmd::More);
    }
    let details_width = if compact_commands(w) { 40.0 } else { CMD_DETAILS.2 };
    if x >= w - details_width && x < w - 8.0 {
        return Hit::Cmd(Cmd::Details);
    }
    Hit::None
}

/// Baseline that centers Segoe text of `size` px on `cy`.
pub fn baseline(cy: f32, size: f32) -> f32 {
    (cy + size * 0.36).round()
}

/// The crumbs that fit in `room` px when the chevrons and gaps take `gap` px each: Explorer drops
/// the leading ones first. Returns the index of the first crumb shown.
pub fn first_fitting(widths: &[f32], gap: f32, room: f32) -> usize {
    let mut used = gap;
    for i in (0..widths.len()).rev() {
        used += widths[i] + 2.0 * gap;
        if used > room {
            return (i + 1).min(widths.len().saturating_sub(1));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1240.0;

    #[test]
    fn caption_buttons() {
        assert_eq!(hit(W - 23.0, 16.0, W, 1, 0, &[]), Hit::Close);
        assert_eq!(hit(W - 69.0, 10.0, W, 1, 0, &[]), Hit::Max);
        assert_eq!(hit(W - 115.0, 2.0, W, 1, 0, &[]), Hit::Min);
        assert_eq!(hit(700.0, 20.0, W, 1, 0, &[]), Hit::Caption);
    }

    #[test]
    fn tabs_and_new_tab() {
        assert_eq!(hit(100.0, 24.0, W, 1, 0, &[]), Hit::Tab(0));
        assert_eq!(hit(202.5, 24.0, W, 1, 0, &[]), Hit::TabClose(0));
        assert_eq!(hit(246.0, 24.0, W, 1, 0, &[]), Hit::NewTab);
        assert_eq!(hit(300.0, 24.0, W, 2, 0, &[]), Hit::Tab(1));
        assert_eq!(hit(100.0, 4.0, W, 1, 0, &[]), Hit::Caption);
    }

    #[test]
    fn tabs_shrink_but_not_below_the_minimum() {
        assert_eq!(tab_strip(W, 1, 0).width, TAB_W);
        assert!(tab_strip(W, 6, 0).width < TAB_W);
        assert!(tab_strip(W, 40, 0).width >= TAB_MIN_W);
    }

    #[test]
    fn tab_strip_keeps_new_tab_clear_of_caption_at_any_tab_count() {
        let w = 520.0;
        let tabs = 40;
        let strip = tab_strip(w, tabs, tabs - 1);
        assert!(strip.visible.contains(&(tabs - 1)));
        assert!(strip.plus + 16.0 < w - 3.0 * CAPTION_W);
        assert_eq!(hit(strip.plus, TAB_CY, w, tabs, tabs - 1, &[]), Hit::NewTab);
    }

    #[test]
    fn nav_bar() {
        assert_eq!(hit(28.0, 64.0, W, 1, 0, &[]), Hit::Back);
        assert_eq!(hit(148.0, 68.0, W, 1, 0, &[]), Hit::Refresh);
        assert_eq!(hit(300.0, 64.0, W, 1, 0, &[(264.0, 320.0)]), Hit::Crumb(0));
        assert_eq!(hit(600.0, 64.0, W, 1, 0, &[(264.0, 320.0)]), Hit::Address);
        assert_eq!(hit(W - 100.0, 64.0, W, 1, 0, &[]), Hit::Search);
    }

    #[test]
    fn command_bar() {
        assert_eq!(hit(148.0, 112.0, W, 1, 0, &[]), Hit::Cmd(Cmd::Cut));
        assert_eq!(hit(292.0, 116.0, W, 1, 0, &[]), Hit::Cmd(Cmd::Share));
        assert_eq!(hit(330.0, 116.0, W, 1, 0, &[]), Hit::Cmd(Cmd::Delete));
        assert_eq!(hit(413.0, 116.0, W, 1, 0, &[]), Hit::Cmd(Cmd::Sort));
        assert_eq!(hit(498.0, 116.0, W, 1, 0, &[]), Hit::Cmd(Cmd::View));
        assert_eq!(hit(462.0, 116.0, 520.0, 1, 0, &[]), Hit::Cmd(Cmd::More));
        assert_eq!(hit(496.0, 116.0, 520.0, 1, 0, &[]), Hit::Cmd(Cmd::Details));
        assert_eq!(hit(W - 60.0, 112.0, W, 1, 0, &[]), Hit::Cmd(Cmd::Details));
        assert_eq!(hit(800.0, 112.0, W, 1, 0, &[]), Hit::None);
        assert_eq!(hit(800.0, 200.0, W, 1, 0, &[]), Hit::None);
    }

    #[test]
    fn crumbs_drop_from_the_left() {
        assert_eq!(first_fitting(&[50.0, 50.0, 50.0], 20.0, 1000.0), 0);
        // 20 + 90 + 90 = 200 fits two, the third would need 290.
        assert_eq!(first_fitting(&[50.0, 50.0, 50.0], 20.0, 200.0), 1);
        // Nothing fits: the last one still shows.
        assert_eq!(first_fitting(&[50.0, 50.0, 50.0], 20.0, 10.0), 2);
    }
}
