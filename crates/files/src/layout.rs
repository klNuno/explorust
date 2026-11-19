//! Geometry of the chrome (tab strip, navigation bar, command bar) at 100% scale, measured on
//! his File Explorer by `crates/explorer-proto` (`.agents/explorer-result.md`), and what a point
//! in it hits. Everything below `CHROME_H` belongs to Windows' own view.

pub const TABSTRIP_H: f32 = 40.0;
pub const NAVBAR_BOTTOM: f32 = 80.0;
pub const CMDBAR_BOTTOM: f32 = 135.0;
/// Height of the chrome, the command bar's bottom line included.
pub const CHROME_H: f32 = 136.0;

pub const TAB_X: f32 = 9.0;
pub const TAB_Y: f32 = 8.0;
pub const TAB_W: f32 = 237.0;
pub const TAB_MIN_W: f32 = 100.0;
pub const TAB_RADIUS: f32 = 8.0;
pub const TAB_CY: f32 = 24.0;
pub const TAB_ICON_DX: f32 = 8.0;
pub const TAB_TEXT_DX: f32 = 34.0;
pub const TAB_CLOSE_FROM_RIGHT: f32 = 17.5;
pub const NEW_TAB_DX: f32 = 25.0;
/// Caption buttons, width and centers from the right edge: minimize, maximize, close.
pub const CAPTION_W: f32 = 46.0;
pub const CAPTION_CX: [f32; 3] = [115.0, 69.0, 23.0];
pub const CAPTION_CY: f32 = 16.0;

pub const NAV_BTN_CX: [f32; 4] = [28.0, 76.0, 124.0, 172.0];
pub const NAV_BTN_W: f32 = 40.0;
pub const NAV_CY: f32 = 64.0;
pub const BOX_TOP: f32 = 48.0;
pub const BOX_H: f32 = 32.0;
pub const BOX_RADIUS: f32 = 4.0;
/// Address box: left edge, inset of its right edge from the window's right edge.
pub const ADDR: (f32, f32) = (202.0, 315.0);
pub const ADDR_ICON_CX: f32 = 223.0;
pub const CRUMB_FIRST_CHEVRON_CX: f32 = 253.0;
pub const CRUMB_GAP: f32 = 21.0;
/// Search box insets from the right edge: left, right.
pub const SEARCH: (f32, f32) = (306.0, 12.0);
pub const SEARCH_TEXT_PAD: f32 = 15.0;
pub const SEARCH_GLYPH_FROM_RIGHT: f32 = 32.0;

pub const CMD_CY: f32 = 112.0;
pub const CMD_SEPS: [f32; 3] = [122.0, 414.0, 616.0];
pub const CMD_BTN_W: f32 = 40.0;
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
    (Cmd::Cut, '\u{E8C6}', 148.0),
    (Cmd::Copy, '\u{E8C8}', 196.0),
    (Cmd::Paste, '\u{E77F}', 243.0),
    (Cmd::Rename, '\u{E8AC}', 291.0),
    (Cmd::Share, '\u{E72D}', 339.0),
    (Cmd::Delete, '\u{E74D}', 388.0),
];
/// Labelled buttons: command, glyph, glyph cx, label x, chevron cx, left, right.
pub const CMD_LABELLED: [(Cmd, char, f32, f32, f32, f32, f32); 3] = [
    (Cmd::New, '\u{ECC8}', 26.0, 43.0, 101.0, 8.0, 114.0),
    (Cmd::Sort, '\u{E8CB}', 438.0, 455.0, 488.0, 422.0, 504.0),
    (Cmd::View, '\u{E8FD}', 528.0, 544.0, 596.0, 512.0, 608.0),
];
pub const CMD_MORE_CX: f32 = 641.0;
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
pub fn tab_width(w: f32, n: usize) -> f32 {
    let room = w - TAB_X - CAPTION_CX[0] - CAPTION_W / 2.0 - 2.0 * NEW_TAB_DX;
    (room / n.max(1) as f32).clamp(TAB_MIN_W, TAB_W)
}

fn within(v: f32, center: f32, width: f32) -> bool {
    (v - center).abs() <= width / 2.0
}

/// What the point (x, y) hits in a window `w` wide with `tabs` tabs; `crumbs` are the x spans of
/// the breadcrumb's items as last drawn.
pub fn hit(x: f32, y: f32, w: f32, tabs: usize, crumbs: &[(f32, f32)]) -> Hit {
    if x < 0.0 || y < 0.0 || x >= w || y >= CHROME_H {
        return Hit::None;
    }
    if y < TABSTRIP_H {
        for (i, h) in [Hit::Min, Hit::Max, Hit::Close].into_iter().enumerate() {
            if y < TABSTRIP_H - 8.0 && within(x, w - CAPTION_CX[i], CAPTION_W) {
                return h;
            }
        }
        let tw = tab_width(w, tabs);
        if y >= TAB_Y {
            for i in 0..tabs {
                let x0 = TAB_X + i as f32 * tw;
                if x >= x0 && x < x0 + tw {
                    let close = x0 + tw - TAB_CLOSE_FROM_RIGHT;
                    return if within(x, close, 24.0) { Hit::TabClose(i) } else { Hit::Tab(i) };
                }
            }
            let plus = TAB_X + tabs as f32 * tw + NEW_TAB_DX;
            if within(x, plus, 36.0) {
                return Hit::NewTab;
            }
        }
        return Hit::Caption;
    }
    if y < NAVBAR_BOTTOM {
        if y < BOX_TOP - 4.0 {
            return Hit::None;
        }
        for (i, h) in [Hit::Back, Hit::Forward, Hit::Up, Hit::Refresh].into_iter().enumerate() {
            if within(x, NAV_BTN_CX[i], NAV_BTN_W) {
                return h;
            }
        }
        if x >= ADDR.0 && x < w - ADDR.1 {
            if let Some(i) = crumbs.iter().position(|&(a, b)| x >= a && x < b) {
                return Hit::Crumb(i);
            }
            return Hit::Address;
        }
        if x >= w - SEARCH.0 && x < w - SEARCH.1 {
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
    for (c, _, _, _, _, l, r) in CMD_LABELLED {
        if x >= l && x < r {
            return Hit::Cmd(c);
        }
    }
    if within(x, CMD_MORE_CX, CMD_BTN_W) {
        return Hit::Cmd(Cmd::More);
    }
    if x >= w - CMD_DETAILS.2 && x < w - 8.0 {
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
        assert_eq!(hit(W - 23.0, 16.0, W, 1, &[]), Hit::Close);
        assert_eq!(hit(W - 69.0, 10.0, W, 1, &[]), Hit::Max);
        assert_eq!(hit(W - 115.0, 2.0, W, 1, &[]), Hit::Min);
        assert_eq!(hit(700.0, 20.0, W, 1, &[]), Hit::Caption);
    }

    #[test]
    fn tabs_and_new_tab() {
        assert_eq!(hit(100.0, 24.0, W, 1, &[]), Hit::Tab(0));
        assert_eq!(hit(228.5, 24.0, W, 1, &[]), Hit::TabClose(0));
        assert_eq!(hit(271.0, 24.0, W, 1, &[]), Hit::NewTab);
        assert_eq!(hit(300.0, 24.0, W, 2, &[]), Hit::Tab(1));
        assert_eq!(hit(100.0, 4.0, W, 1, &[]), Hit::Caption);
    }

    #[test]
    fn tabs_shrink_but_not_below_the_minimum() {
        assert_eq!(tab_width(W, 1), TAB_W);
        assert!(tab_width(W, 6) < TAB_W);
        assert_eq!(tab_width(W, 40), TAB_MIN_W);
    }

    #[test]
    fn nav_bar() {
        assert_eq!(hit(28.0, 64.0, W, 1, &[]), Hit::Back);
        assert_eq!(hit(172.0, 64.0, W, 1, &[]), Hit::Refresh);
        assert_eq!(hit(300.0, 64.0, W, 1, &[(264.0, 320.0)]), Hit::Crumb(0));
        assert_eq!(hit(600.0, 64.0, W, 1, &[(264.0, 320.0)]), Hit::Address);
        assert_eq!(hit(W - 100.0, 64.0, W, 1, &[]), Hit::Search);
    }

    #[test]
    fn command_bar() {
        assert_eq!(hit(148.0, 112.0, W, 1, &[]), Hit::Cmd(Cmd::Cut));
        assert_eq!(hit(388.0, 112.0, W, 1, &[]), Hit::Cmd(Cmd::Delete));
        assert_eq!(hit(450.0, 112.0, W, 1, &[]), Hit::Cmd(Cmd::Sort));
        assert_eq!(hit(560.0, 112.0, W, 1, &[]), Hit::Cmd(Cmd::View));
        assert_eq!(hit(W - 60.0, 112.0, W, 1, &[]), Hit::Cmd(Cmd::Details));
        assert_eq!(hit(800.0, 112.0, W, 1, &[]), Hit::None);
        assert_eq!(hit(800.0, 200.0, W, 1, &[]), Hit::None);
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
