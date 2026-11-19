//! Geometry of the switcher in DIPs, pure so it is tested on any OS: tiles in centered rows, each a
//! title row (app icon, title, close button) over a thumbnail box whose width follows the window's
//! aspect, the panel sized to its rows and centered on the monitor. Rows wrap at 90% of the screen
//! width; when the rows are taller than 80% of the screen, the thumbnails shrink.

/// Panel padding around the tiles.
pub const PAD: f32 = 16.0;
/// Space between tiles, across and down.
pub const GAP: f32 = 8.0;
/// Title row: icon and title above the thumbnail.
pub const TITLE_H: f32 = 32.0;
/// Thumbnail inset from the tile's sides and bottom.
pub const INSET: f32 = 8.0;
/// Thumbnail box height at full size.
pub const THUMB_H: f32 = 158.0;
/// Narrowest and widest thumbnail box, as multiples of its height.
pub const MIN_ASPECT: f32 = 0.6;
pub const MAX_ASPECT: f32 = 2.0;
/// App icon in the title row, and its left margin.
pub const ICON: f32 = 16.0;
pub const ICON_X: f32 = 10.0;
/// Close button, a square at the top right of the title row.
pub const CLOSE: f32 = 28.0;
pub const CLOSE_MARGIN: f32 = 2.0;
/// Corner radius of the panel, the tiles and the selection ring; ring stroke width.
pub const RADIUS: f32 = 8.0;
pub const RING: f32 = 3.0;
/// Smallest thumbnail height the shrinking goes to.
const MIN_THUMB_H: f32 = 48.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
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
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    /// Linear blend from `self` (t = 0) to `to` (t = 1).
    pub fn lerp(&self, to: &Rect, t: f32) -> Rect {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Rect::new(m(self.x, to.x), m(self.y, to.y), m(self.w, to.w), m(self.h, to.h))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// The whole tile, in panel coordinates.
    pub rect: Rect,
    /// Where the thumbnail goes: the source's aspect fitted in the thumbnail box.
    pub thumb: Rect,
}

impl Tile {
    pub fn icon(&self) -> Rect {
        Rect::new(self.rect.x + ICON_X, self.rect.y + (TITLE_H - ICON) / 2.0, ICON, ICON)
    }
    pub fn close(&self) -> Rect {
        Rect::new(self.rect.right() - CLOSE - CLOSE_MARGIN, self.rect.y + (TITLE_H - CLOSE) / 2.0, CLOSE, CLOSE)
    }
    /// The title's box, left of the close button.
    pub fn title(&self) -> Rect {
        let x = self.rect.x + ICON_X + ICON + 8.0;
        Rect::new(x, self.rect.y, (self.close().x - 4.0 - x).max(0.0), TITLE_H)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Panel size.
    pub w: f32,
    pub h: f32,
    pub tiles: Vec<Tile>,
}

impl Layout {
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.tiles.iter().position(|t| t.rect.contains(x, y))
    }
    /// Panel origin that centers it in a work area.
    pub fn origin(&self, area: Rect) -> (f32, f32) {
        (area.x + ((area.w - self.w) / 2.0).round(), area.y + ((area.h - self.h) / 2.0).round())
    }
}

/// Thumbnail box width for a source aspect (width / height) at box height `h`.
fn box_w(aspect: f32, h: f32) -> f32 {
    let a = if aspect.is_finite() && aspect > 0.0 { aspect } else { 16.0 / 10.0 };
    (a.clamp(MIN_ASPECT, MAX_ASPECT) * h).round()
}

/// Lays out one tile per source aspect for a `screen_w` x `screen_h` monitor (DIPs).
pub fn layout(aspects: &[f32], screen_w: f32, screen_h: f32) -> Layout {
    let max_inner = (screen_w * 0.9 - 2.0 * PAD).max(1.0);
    let max_h = screen_h * 0.8;
    let mut th = THUMB_H;
    loop {
        let rows = pack(aspects, th, max_inner);
        let tile_h = TITLE_H + th + INSET;
        let n = rows.len() as f32;
        let total_h = n * tile_h + (n - 1.0).max(0.0) * GAP + 2.0 * PAD;
        if total_h <= max_h || th <= MIN_THUMB_H {
            return place(aspects, &rows, th, total_h);
        }
        th = (th * 0.9).floor().max(MIN_THUMB_H);
    }
}

/// Greedy rows of tile indices that fit in `max_inner`.
fn pack(aspects: &[f32], th: f32, max_inner: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut width = 0.0;
    for (i, &a) in aspects.iter().enumerate() {
        let w = box_w(a, th) + 2.0 * INSET;
        match rows.last_mut() {
            Some(row) if width + GAP + w <= max_inner => {
                row.push(i);
                width += GAP + w;
            }
            _ => {
                rows.push(vec![i]);
                width = w;
            }
        }
    }
    rows
}

fn place(aspects: &[f32], rows: &[Vec<usize>], th: f32, total_h: f32) -> Layout {
    let tile_w = |i: usize| box_w(aspects[i], th) + 2.0 * INSET;
    let row_w = |row: &Vec<usize>| row.iter().map(|&i| tile_w(i)).sum::<f32>() + GAP * (row.len().max(1) - 1) as f32;
    let inner = rows.iter().map(row_w).fold(0.0, f32::max);
    let tile_h = TITLE_H + th + INSET;
    let mut tiles = vec![Tile { rect: Rect::default(), thumb: Rect::default() }; aspects.len()];
    for (r, row) in rows.iter().enumerate() {
        let mut x = PAD + ((inner - row_w(row)) / 2.0).round();
        let y = PAD + r as f32 * (tile_h + GAP);
        for &i in row {
            let w = tile_w(i);
            let bw = w - 2.0 * INSET;
            let a = if aspects[i].is_finite() && aspects[i] > 0.0 { aspects[i] } else { 16.0 / 10.0 };
            // Fit the source in the box, centered, bottom-aligned rows of equal height.
            let (fw, fh) = if a >= bw / th { (bw, (bw / a).round()) } else { ((th * a).round(), th) };
            let thumb = Rect::new(x + INSET + ((bw - fw) / 2.0).round(), y + TITLE_H + ((th - fh) / 2.0).round(), fw, fh);
            tiles[i] = Tile { rect: Rect::new(x, y, w, tile_h), thumb };
            x += w + GAP;
        }
    }
    Layout { w: inner + 2.0 * PAD, h: total_h, tiles }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_row_centered() {
        let l = layout(&[16.0 / 9.0, 1.0, 4.0 / 3.0], 1280.0, 800.0);
        assert_eq!(l.tiles.len(), 3);
        assert!(l.tiles.iter().all(|t| t.rect.y == PAD));
        assert_eq!(l.h, 2.0 * PAD + TITLE_H + THUMB_H + INSET);
        let last = &l.tiles[2].rect;
        assert_eq!(last.right() + PAD, l.w);
        // Tiles do not overlap and keep the gap.
        assert_eq!(l.tiles[1].rect.x, l.tiles[0].rect.right() + GAP);
        let (x, y) = l.origin(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert_eq!(x, ((1280.0 - l.w) / 2.0).round());
        assert_eq!(y, ((800.0 - l.h) / 2.0).round());
    }

    #[test]
    fn thumbnail_keeps_aspect_inside_its_box() {
        let l = layout(&[3.0], 1280.0, 800.0);
        let t = &l.tiles[0];
        // Wider than the widest box: full box width, shorter than the box and centered.
        assert_eq!(t.thumb.w, MAX_ASPECT * THUMB_H);
        assert_eq!(t.thumb.h, (t.thumb.w / 3.0).round());
        assert!(t.thumb.y > t.rect.y + TITLE_H);
        let tall = layout(&[0.3], 1280.0, 800.0);
        assert_eq!(tall.tiles[0].thumb.h, THUMB_H);
        assert_eq!(tall.tiles[0].thumb.w, (THUMB_H * 0.3).round());
    }

    #[test]
    fn rows_wrap_and_shrink() {
        let many = vec![16.0 / 9.0; 12];
        let l = layout(&many, 1280.0, 800.0);
        assert!(l.w <= 1280.0 * 0.9 + 0.5);
        let rows: std::collections::BTreeSet<i32> = l.tiles.iter().map(|t| t.rect.y as i32).collect();
        assert!(rows.len() > 1);
        let lots = vec![16.0 / 9.0; 60];
        let s = layout(&lots, 1280.0, 800.0);
        assert!(s.tiles[0].rect.h < TITLE_H + THUMB_H + INSET);
        assert!(s.h <= 800.0 * 0.8 || s.tiles[0].thumb.h <= MIN_THUMB_H);
    }

    #[test]
    fn hits_and_parts() {
        let l = layout(&[1.5, 1.5], 1280.0, 800.0);
        let t = &l.tiles[1];
        assert_eq!(l.hit(t.rect.x + 1.0, t.rect.y + 1.0), Some(1));
        assert_eq!(l.hit(1.0, 1.0), None);
        assert!(t.close().right() <= t.rect.right());
        assert!(t.title().right() < t.close().x);
        assert!(t.rect.contains(t.icon().x, t.icon().y));
    }
}
