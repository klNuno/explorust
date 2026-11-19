//! Start's geometry in DIPs (scaled by the monitor's DPI when drawn), after Windows 11's dark Start
//! at 100%: 642x726, search box on top, a 6x3 pinned grid per page, Recommended in two columns,
//! the user and power buttons in the bottom band.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

pub const W: f32 = 642.0;
pub const H: f32 = 726.0;
pub const RADIUS: f32 = 8.0;
/// Windows' taskbar height and the gap Start keeps above it.
pub const TASKBAR_H: f32 = 48.0;
pub const GAP: f32 = 12.0;
/// The slide-up distance and duration of the open animation.
pub const SLIDE: f32 = 120.0;
pub const OPEN_MS: f32 = 250.0;

pub const SEARCH: Rect = Rect::new(32.0, 32.0, 578.0, 32.0);
pub const SEARCH_RADIUS: f32 = 16.0;

/// Section headers ("Pinned", "Recommended", "All apps"): left x and text baseline.
pub const HEADER_X: f32 = 64.0;
pub const PINNED_BASELINE: f32 = 120.0;
/// The "All >" / "< Back" / "More >" pill on the right of a header, as a box.
pub fn header_button(baseline: f32, width: f32) -> Rect {
    Rect::new(W - 65.0 - width, baseline - 15.0, width, 22.0)
}
pub const ALL_BTN_W: f32 = 48.0;
pub const BACK_BTN_W: f32 = 64.0;

pub const COLS: usize = 6;
pub const ROWS: usize = 3;
pub const PER_PAGE: usize = COLS * ROWS;
pub const TILE_W: f32 = 96.0;
pub const TILE_H: f32 = 84.0;
pub const GRID_X: f32 = 32.0;
pub const GRID_Y: f32 = 140.0;
pub const TILE_ICON: f32 = 32.0;
pub const TILE_ICON_TOP: f32 = 12.0;
/// Top of the label box (up to two lines, as Windows wraps long names).
pub const TILE_LABEL_TOP: f32 = 48.0;

/// Box of the tile at slot `i` (0..PER_PAGE) on a page.
pub fn tile(i: usize) -> Rect {
    let (c, r) = (i % COLS, i / COLS);
    Rect::new(GRID_X + c as f32 * TILE_W, GRID_Y + r as f32 * TILE_H, TILE_W, TILE_H)
}

pub fn pages(pins: usize) -> usize {
    pins.div_ceil(PER_PAGE).max(1)
}

/// Page dots on the right of the grid: centre of dot `p` of `n`.
pub fn page_dot(p: usize, n: usize) -> (f32, f32) {
    let mid = GRID_Y + ROWS as f32 * TILE_H / 2.0;
    let top = mid - (n as f32 - 1.0) * 6.5;
    (W - 16.0, top + p as f32 * 13.0)
}

pub const REC_BASELINE: f32 = 440.0;
pub const REC_COLS: usize = 2;
pub const REC_ROWS: usize = 3;
pub const REC_W: f32 = 266.0;
pub const REC_H: f32 = 58.0;
pub const REC_X: f32 = 52.0;
pub const REC_Y: f32 = 459.0;
pub const REC_ICON: f32 = 32.0;

pub fn rec(i: usize) -> Rect {
    let (c, r) = (i % REC_COLS, i / REC_COLS);
    Rect::new(REC_X + c as f32 * (REC_W + 14.0), REC_Y + r as f32 * REC_H, REC_W, REC_H)
}

/// The bottom band with the user and power buttons.
pub const BAND: Rect = Rect::new(0.0, H - 66.0, W, 66.0);
pub const USER: Rect = Rect::new(52.0, H - 53.0, 200.0, 40.0);
pub const POWER: Rect = Rect::new(549.0, H - 53.0, 40.0, 40.0);
pub const AVATAR: f32 = 32.0;

/// All apps and search results: a scrolled list between the header and the band.
pub const LIST: Rect = Rect::new(0.0, 136.0, W, H - 66.0 - 136.0 - 8.0);
pub const ROW_H: f32 = 40.0;
pub const LETTER_H: f32 = 36.0;
pub const LIST_X: f32 = 52.0;
pub const LIST_ICON: f32 = 24.0;

/// Search results: the list on the left, the preview of the selected result on the right.
pub const RESULTS: Rect = Rect::new(24.0, 84.0, 300.0, H - 66.0 - 84.0 - 12.0);
pub const PREVIEW: Rect = Rect::new(336.0, 84.0, W - 24.0 - 336.0, H - 66.0 - 84.0 - 12.0);
pub const RES_HEADER_H: f32 = 34.0;
pub const RES_ROW_H: f32 = 40.0;
pub const RES_BEST_H: f32 = 64.0;
pub const PREVIEW_ICON: f32 = 64.0;
/// Top of the first action row in the preview, and the height of each.
pub const ACTIONS_Y: f32 = PREVIEW.y + 200.0;
pub const ACTION_H: f32 = 36.0;

pub fn action(i: usize) -> Rect {
    Rect::new(PREVIEW.x + 8.0, ACTIONS_Y + i as f32 * ACTION_H, PREVIEW.w - 16.0, ACTION_H - 2.0)
}

/// Where the menu sits on a monitor: centred above the taskbar, in physical pixels.
pub fn place(mon_left: i32, mon_right: i32, mon_bottom: i32, scale: f32) -> (i32, i32, i32, i32) {
    let w = (W * scale).round() as i32;
    let h = (H * scale).round() as i32;
    let x = mon_left + (mon_right - mon_left - w) / 2;
    let y = mon_bottom - ((TASKBAR_H + GAP) * scale).round() as i32 - h;
    (x, y, w, h)
}

/// What the pointer is over in the main view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Search,
    AllApps,
    Back,
    Tile(usize),
    PageDot(usize),
    Rec(usize),
    Row(usize),
    /// An action of the search preview panel.
    Action(usize),
    User,
    Power,
}

pub fn hit_main(x: f32, y: f32, pins_on_page: usize, pages: usize, recs: usize) -> Hit {
    if SEARCH.contains(x, y) {
        return Hit::Search;
    }
    if header_button(PINNED_BASELINE, ALL_BTN_W).contains(x, y) {
        return Hit::AllApps;
    }
    for i in 0..pins_on_page {
        if tile(i).contains(x, y) {
            return Hit::Tile(i);
        }
    }
    if pages > 1 {
        for p in 0..pages {
            let (cx, cy) = page_dot(p, pages);
            if (x - cx).abs() <= 8.0 && (y - cy).abs() <= 8.0 {
                return Hit::PageDot(p);
            }
        }
    }
    for i in 0..recs.min(REC_COLS * REC_ROWS) {
        if rec(i).contains(x, y) {
            return Hit::Rec(i);
        }
    }
    hit_band(x, y)
}

pub fn hit_band(x: f32, y: f32) -> Hit {
    if USER.contains(x, y) {
        Hit::User
    } else if POWER.contains(x, y) {
        Hit::Power
    } else {
        Hit::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_centred_and_fits_above_recommended() {
        assert_eq!(GRID_X + COLS as f32 * TILE_W + GRID_X, W - 2.0);
        assert!(tile(PER_PAGE - 1).bottom() < REC_BASELINE - 14.0);
        assert!(rec(REC_COLS * REC_ROWS - 1).bottom() <= BAND.y);
    }

    #[test]
    fn hits() {
        let t = tile(7);
        assert_eq!(hit_main(t.x + 10.0, t.y + 10.0, 18, 1, 0), Hit::Tile(7));
        assert_eq!(hit_main(t.x + 10.0, t.y + 10.0, 7, 1, 0), Hit::None);
        assert_eq!(hit_main(SEARCH.x + 5.0, SEARCH.y + 5.0, 0, 1, 0), Hit::Search);
        assert_eq!(hit_main(POWER.x + 5.0, POWER.y + 5.0, 0, 1, 0), Hit::Power);
        let (cx, cy) = page_dot(1, 2);
        assert_eq!(hit_main(cx, cy, 18, 2, 0), Hit::PageDot(1));
        let r = rec(3);
        assert_eq!(hit_main(r.x + 1.0, r.y + 1.0, 0, 1, 6), Hit::Rec(3));
    }

    #[test]
    fn results_fit_beside_the_preview() {
        assert!(RESULTS.right() < PREVIEW.x);
        assert!(PREVIEW.right() <= W - 24.0);
        assert!(action(5).bottom() <= PREVIEW.bottom());
        assert!(RESULTS.bottom() <= BAND.y);
    }

    #[test]
    fn placement_on_a_1280x800_screen() {
        assert_eq!(place(0, 1280, 800, 1.0), (319, 14, 642, 726));
        assert_eq!(pages(0), 1);
        assert_eq!(pages(19), 2);
    }
}
