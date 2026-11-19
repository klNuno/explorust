//! Geometry of Task View in DIPs, pure: the open windows of the current desktop as large tiles
//! centered above the desktops strip, and the strip at the bottom (one tile per desktop with its
//! name over a picture of the desktop, then the New desktop button), as Windows 11 lays it out.

use crate::layout::{self as L, Layout, Rect};

/// Height of the desktops strip at the bottom, its tiles' picture height and the gap between them.
pub const STRIP_H: f32 = 176.0;
pub const DESK_THUMB_H: f32 = 100.0;
pub const DESK_GAP: f32 = 16.0;
/// Name row above a desktop's picture.
pub const DESK_LABEL_H: f32 = 32.0;
pub const DESK_PAD: f32 = 8.0;
/// Window thumbnails in Task View are taller than in Alt+Tab.
pub const WIN_THUMB_H: f32 = 240.0;
/// Space above the window tiles.
pub const TOP: f32 = 24.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeskTile {
    pub rect: Rect,
    pub thumb: Rect,
    pub label: Rect,
    pub close: Rect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Strip {
    pub desks: Vec<DeskTile>,
    pub new_desk: Rect,
}

impl Strip {
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.desks.iter().position(|d| d.rect.contains(x, y))
    }
}

/// The strip for `n` desktops on a `w` x `h` monitor; pictures keep the monitor's aspect.
pub fn strip(n: usize, w: f32, h: f32) -> Strip {
    let aspect = if h > 0.0 { w / h } else { 16.0 / 10.0 };
    let mut th = DESK_THUMB_H;
    let mut tw = (th * aspect).round();
    let count = n + 1;
    // Shrink when the tiles do not fit in 90% of the width.
    let total = |tw: f32| count as f32 * (tw + 2.0 * DESK_PAD) + (count - 1) as f32 * DESK_GAP;
    while total(tw) > w * 0.9 && th > 40.0 {
        th = (th * 0.9).floor();
        tw = (th * aspect).round();
    }
    let tile_w = tw + 2.0 * DESK_PAD;
    let tile_h = DESK_LABEL_H + th + DESK_PAD;
    let mut x = ((w - total(tw)) / 2.0).round();
    let y = (h - STRIP_H + (STRIP_H - tile_h) / 2.0).round();
    let mut desks = Vec::with_capacity(n);
    for _ in 0..n {
        let rect = Rect::new(x, y, tile_w, tile_h);
        desks.push(DeskTile {
            rect,
            thumb: Rect::new(x + DESK_PAD, y + DESK_LABEL_H, tw, th),
            label: Rect::new(x + DESK_PAD, y, tw - L::CLOSE, DESK_LABEL_H),
            close: Rect::new(rect.right() - L::CLOSE - 2.0, y + 2.0, L::CLOSE, L::CLOSE),
        });
        x += tile_w + DESK_GAP;
    }
    Strip { desks, new_desk: Rect::new(x, y, tile_w, tile_h) }
}

/// The window tiles, centered in the area above the strip.
pub fn windows(aspects: &[f32], w: f32, h: f32) -> Layout {
    let area_h = (h - STRIP_H - TOP).max(1.0);
    // `layout_with` keeps the tiles under 80% of the height it is given.
    let mut l = L::layout_with(aspects, w, area_h / 0.8, WIN_THUMB_H);
    let (ox, oy) = l.origin(Rect::new(0.0, TOP, w, area_h));
    for t in &mut l.tiles {
        t.rect.x += ox;
        t.rect.y += oy;
        t.thumb.x += ox;
        t.thumb.y += oy;
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_is_centered_at_the_bottom() {
        let s = strip(2, 1280.0, 800.0);
        assert_eq!(s.desks.len(), 2);
        let left = s.desks[0].rect.x;
        let right = 1280.0 - s.new_desk.right();
        assert!((left - right).abs() <= 1.0, "{left} {right}");
        assert!(s.new_desk.bottom() <= 800.0 && s.desks[0].rect.y >= 800.0 - STRIP_H);
        assert_eq!(s.hit(s.desks[1].rect.x + 1.0, s.desks[1].rect.y + 1.0), Some(1));
    }

    #[test]
    fn many_desktops_shrink_to_fit() {
        let s = strip(9, 1280.0, 800.0);
        assert!(s.new_desk.right() <= 1280.0 * 0.95 + 1.0);
        assert!(s.desks[0].rect.x >= 0.0);
    }

    #[test]
    fn windows_stay_above_the_strip() {
        let l = windows(&[1.6, 1.0, 0.8, 1.6, 1.6], 1280.0, 800.0);
        for t in &l.tiles {
            assert!(t.rect.y >= TOP && t.rect.bottom() <= 800.0 - STRIP_H + 1.0, "{:?}", t.rect);
            assert!(t.rect.x >= 0.0 && t.rect.right() <= 1280.0);
        }
    }
}
