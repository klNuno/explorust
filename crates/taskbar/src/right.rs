//! The right end of the bar, measured on explorer's bar at 100% (`captures/step1-fallback-kill.png`):
//! the chevron, the promoted tray icons, the quick settings button (network, volume, battery
//! glyphs), the clock and the show desktop strip. All in DIPs from the bar's left edge.

/// A hit on the right side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Chevron,
    /// The nth promoted icon, left to right.
    Icon(usize),
    QuickSettings,
    Clock,
    ShowDesktop,
}

pub const TRAY_SLOT: f32 = 32.0;
pub const TRAY_ICON: f32 = 16.0;
pub const GLYPH: f32 = 16.0;
pub const GLYPH_STEP: f32 = 30.0;
pub const GLYPH_TOP: f32 = 16.0;
pub const SHOW_DESKTOP_W: f32 = 12.0;
pub const BUTTON_TOP: f32 = 4.0;
pub const BUTTON_H: f32 = 40.0;
/// Gap between the last glyph and the clock text's left edge.
const GLYPH_TO_CLOCK: f32 = 16.0;
/// Gap between the first glyph and the tray slots.
const SLOT_TO_GLYPH: f32 = 8.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Left, right of each button in DIPs.
    pub chevron: Option<(f32, f32)>,
    pub icons: Vec<(f32, f32)>,
    pub quick: (f32, f32),
    /// Left edge of each status glyph box, left to right.
    pub glyphs: Vec<f32>,
    pub clock: (f32, f32),
    pub show_desktop: (f32, f32),
    /// Left edge of the clock text.
    pub clock_text_left: f32,
}

impl Layout {
    /// `clock_text_right` is where the clock text ends, `clock_w` its width.
    pub fn new(width: f32, clock_text_right: f32, clock_w: f32, icons: usize, chevron: bool, glyphs: usize) -> Layout {
        let lc = (clock_text_right - clock_w).round();
        let last = lc - GLYPH_TO_CLOCK - GLYPH;
        let g0 = last - GLYPH_STEP * glyphs.saturating_sub(1) as f32;
        let glyph_lefts: Vec<f32> = (0..glyphs).map(|i| g0 + GLYPH_STEP * i as f32).collect();
        let slots_right = g0 - SLOT_TO_GLYPH;
        let icon_rects: Vec<(f32, f32)> =
            (0..icons).map(|i| (slots_right - TRAY_SLOT * (icons - i) as f32, slots_right - TRAY_SLOT * (icons - i - 1) as f32)).collect();
        let first = slots_right - TRAY_SLOT * icons as f32;
        Layout {
            chevron: chevron.then_some((first - TRAY_SLOT, first)),
            icons: icon_rects,
            quick: (g0 - SLOT_TO_GLYPH, lc - SLOT_TO_GLYPH),
            glyphs: glyph_lefts,
            clock: (lc - SLOT_TO_GLYPH, width - SHOW_DESKTOP_W),
            show_desktop: (width - SHOW_DESKTOP_W, width),
            clock_text_left: lc,
        }
    }

    /// Leftmost x the right side uses, so task buttons can stay clear of it.
    pub fn left(&self) -> f32 {
        self.chevron.map(|c| c.0).or(self.icons.first().map(|i| i.0)).unwrap_or(self.quick.0)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let inside = |r: (f32, f32)| x >= r.0 && x < r.1;
        if y < 0.0 {
            return None;
        }
        if inside(self.show_desktop) {
            return Some(Hit::ShowDesktop);
        }
        if inside(self.clock) {
            return Some(Hit::Clock);
        }
        if inside(self.quick) {
            return Some(Hit::QuickSettings);
        }
        if let Some(i) = self.icons.iter().position(|r| inside(*r)) {
            return Some(Hit::Icon(i));
        }
        if self.chevron.is_some_and(inside) {
            return Some(Hit::Chevron);
        }
        None
    }

    /// The (left, right) of a hit's button.
    pub fn span(&self, h: Hit) -> Option<(f32, f32)> {
        match h {
            Hit::Chevron => self.chevron,
            Hit::Icon(i) => self.icons.get(i).copied(),
            Hit::QuickSettings => Some(self.quick),
            Hit::Clock => Some(self.clock),
            Hit::ShowDesktop => Some(self.show_desktop),
        }
    }
}

/// Overflow flyout grid: icons per row and the cell size, in DIPs.
pub const FLYOUT_COLS: usize = 5;
pub const FLYOUT_CELL: f32 = 40.0;
pub const FLYOUT_PAD: f32 = 12.0;

/// The flyout's size for `n` icons, (width, height) in DIPs.
pub fn flyout_size(n: usize) -> (f32, f32) {
    let cols = n.clamp(1, FLYOUT_COLS);
    let rows = n.max(1).div_ceil(FLYOUT_COLS);
    (cols as f32 * FLYOUT_CELL + 2.0 * FLYOUT_PAD, rows as f32 * FLYOUT_CELL + 2.0 * FLYOUT_PAD)
}

/// The icon cell under (x, y) in the flyout.
pub fn flyout_hit(n: usize, x: f32, y: f32) -> Option<usize> {
    let (cx, cy) = (x - FLYOUT_PAD, y - FLYOUT_PAD);
    if cx < 0.0 || cy < 0.0 || cx >= FLYOUT_COLS as f32 * FLYOUT_CELL {
        return None;
    }
    let i = (cy / FLYOUT_CELL) as usize * FLYOUT_COLS + (cx / FLYOUT_CELL) as usize;
    (i < n).then_some(i)
}

/// Top-left of cell `i` in the flyout.
pub fn flyout_cell(i: usize) -> (f32, f32) {
    (FLYOUT_PAD + (i % FLYOUT_COLS) as f32 * FLYOUT_CELL, FLYOUT_PAD + (i / FLYOUT_COLS) as f32 * FLYOUT_CELL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_explorer_at_100_percent() {
        // Clock ink 1200..1260 in the capture, one promoted icon, network and volume.
        let l = Layout::new(1280.0, 1261.0, 61.0, 1, true, 2);
        assert_eq!(l.glyphs, vec![1138.0, 1168.0]);
        assert_eq!(l.icons, vec![(1098.0, 1130.0)]);
        // Icon drawn 16 px centered in its slot: 1106..1122 as in the capture.
        assert_eq!((l.icons[0].0 + l.icons[0].1) / 2.0 - TRAY_ICON / 2.0, 1106.0);
        // Chevron centered at 1082 (ink 1077..1086).
        let c = l.chevron.unwrap();
        assert_eq!((c.0 + c.1) / 2.0, 1082.0);
        assert_eq!(l.hit(1110.0, 20.0), Some(Hit::Icon(0)));
        assert_eq!(l.hit(1150.0, 20.0), Some(Hit::QuickSettings));
        assert_eq!(l.hit(1230.0, 20.0), Some(Hit::Clock));
        assert_eq!(l.hit(1275.0, 20.0), Some(Hit::ShowDesktop));
        assert_eq!(l.hit(1080.0, 20.0), Some(Hit::Chevron));
        assert_eq!(l.hit(900.0, 20.0), None);
        assert_eq!(l.left(), 1066.0);
    }

    #[test]
    fn battery_shifts_left() {
        let l = Layout::new(1280.0, 1261.0, 61.0, 0, false, 3);
        assert_eq!(l.glyphs, vec![1108.0, 1138.0, 1168.0]);
        assert_eq!(l.chevron, None);
        assert_eq!(l.left(), 1100.0);
    }

    #[test]
    fn flyout_grid() {
        assert_eq!(flyout_size(3), (144.0, 64.0));
        assert_eq!(flyout_size(7), (224.0, 104.0));
        assert_eq!(flyout_hit(7, 13.0, 13.0), Some(0));
        assert_eq!(flyout_hit(7, 13.0 + 40.0, 13.0 + 40.0), Some(6));
        assert_eq!(flyout_hit(7, 13.0 + 80.0, 13.0 + 40.0), None);
        assert_eq!(flyout_cell(6), (52.0, 52.0));
    }
}
