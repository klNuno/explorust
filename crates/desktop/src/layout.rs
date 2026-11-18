//! Pure geometry of the desktop: where the wallpaper goes for each fit style, and the work areas
//! handed to the icon view, in the coordinates of the Progman window.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { x: left, y: top, w: right - left, h: bottom - top }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let l = self.x.max(o.x);
        let t = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r <= l || b <= t { Rect::default() } else { Rect::from_edges(l, t, r, b) }
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
}

/// Windows' wallpaper positions (Settings > Personalization > Background > "Choose a fit").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Center,
    Tile,
    Stretch,
    Fit,
    Fill,
    Span,
}

impl Fit {
    /// From `HKCU\Control Panel\Desktop` `WallpaperStyle` and `TileWallpaper`.
    pub fn from_registry(style: u32, tile: u32) -> Fit {
        match style {
            0 if tile == 1 => Fit::Tile,
            0 => Fit::Center,
            2 => Fit::Stretch,
            6 => Fit::Fit,
            22 => Fit::Span,
            _ => Fit::Fill,
        }
    }
}

/// The rectangle the image of size `img` covers inside `area` (it may overflow it for Fill and
/// Center; the caller clips). For Tile it is the first tile, at the area's corner.
pub fn place(fit: Fit, img: (i32, i32), area: Rect) -> Rect {
    let (iw, ih) = (img.0.max(1), img.1.max(1));
    let centered = |w: i32, h: i32| {
        Rect::new(area.x + (area.w - w).div_euclid(2), area.y + (area.h - h).div_euclid(2), w, h)
    };
    match fit {
        Fit::Stretch => area,
        Fit::Tile => Rect::new(area.x, area.y, iw, ih),
        Fit::Center => centered(iw, ih),
        Fit::Fit | Fit::Fill | Fit::Span => {
            let sx = area.w as f64 / iw as f64;
            let sy = area.h as f64 / ih as f64;
            let s = if fit == Fit::Fit { sx.min(sy) } else { sx.max(sy) };
            let w = ((iw as f64 * s).round() as i32).max(1);
            let h = ((ih as f64 * s).round() as i32).max(1);
            centered(w, h)
        }
    }
}

/// The areas the wallpaper is laid out in: one per monitor, or the whole virtual screen for Span.
pub fn wallpaper_areas(fit: Fit, monitors: &[Rect], virtual_screen: Rect) -> Vec<Rect> {
    if fit == Fit::Span || monitors.is_empty() { vec![virtual_screen] } else { monitors.to_vec() }
}

/// Work areas in the client coordinates of a window placed at `origin`, primary monitor first
/// (the icon view fills the first one first).
pub fn work_areas(mut works: Vec<(Rect, bool)>, origin: (i32, i32)) -> Vec<Rect> {
    works.sort_by_key(|(_, primary)| !*primary);
    works.into_iter().map(|(r, _)| r.offset(-origin.0, -origin.1)).collect()
}

/// FOLDERFLAGS of the desktop view: Windows' saved `FFlags` of `Shell\Bags\1\Desktop` keeps the
/// user's auto-arrange and align-to-grid; the desktop flags are forced; `HideIcons` adds NOICONS.
pub fn view_flags(saved: Option<u32>, hide_icons: bool) -> u32 {
    const AUTOARRANGE: u32 = 0x1;
    const SNAPTOGRID: u32 = 0x4;
    const DESKTOP: u32 = 0x20;
    const TRANSPARENT: u32 = 0x100;
    const NOCLIENTEDGE: u32 = 0x200;
    const NOSCROLL: u32 = 0x400;
    const NOICONS: u32 = 0x1000;
    // Windows' default for a new profile: align to grid, no auto-arrange.
    let user = saved.unwrap_or(SNAPTOGRID) & (AUTOARRANGE | SNAPTOGRID);
    let mut f = user | DESKTOP | TRANSPARENT | NOCLIENTEDGE | NOSCROLL;
    if hide_icons {
        f |= NOICONS;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_styles() {
        assert_eq!(Fit::from_registry(10, 0), Fit::Fill);
        assert_eq!(Fit::from_registry(0, 1), Fit::Tile);
        assert_eq!(Fit::from_registry(0, 0), Fit::Center);
        assert_eq!(Fit::from_registry(2, 0), Fit::Stretch);
        assert_eq!(Fit::from_registry(6, 0), Fit::Fit);
        assert_eq!(Fit::from_registry(22, 0), Fit::Span);
    }

    #[test]
    fn fill_covers_and_centers() {
        // 1920x1080 on 1280x800: scale 0.7407, 1422x800, cropped left and right.
        let r = place(Fit::Fill, (1920, 1080), Rect::new(0, 0, 1280, 800));
        assert_eq!(r, Rect::new(-71, 0, 1422, 800));
    }

    #[test]
    fn fit_letterboxes() {
        let r = place(Fit::Fit, (1920, 1080), Rect::new(0, 0, 1280, 800));
        assert_eq!(r, Rect::new(0, 40, 1280, 720));
    }

    #[test]
    fn center_keeps_size_on_a_second_monitor() {
        let r = place(Fit::Center, (100, 50), Rect::new(1280, 0, 1000, 500));
        assert_eq!(r, Rect::new(1730, 225, 100, 50));
    }

    #[test]
    fn span_uses_the_virtual_screen() {
        let mons = [Rect::new(0, 0, 1280, 800), Rect::new(1280, 0, 1280, 800)];
        let v = Rect::new(0, 0, 2560, 800);
        assert_eq!(wallpaper_areas(Fit::Span, &mons, v), vec![v]);
        assert_eq!(wallpaper_areas(Fit::Fill, &mons, v), mons.to_vec());
    }

    #[test]
    fn work_areas_primary_first_in_client_coords() {
        let w = work_areas(
            vec![(Rect::new(-1920, 0, 1920, 1040), false), (Rect::new(0, 0, 1280, 752), true)],
            (-1920, 0),
        );
        assert_eq!(w, vec![Rect::new(1920, 0, 1280, 752), Rect::new(0, 0, 1920, 1040)]);
    }

    #[test]
    fn flags_keep_user_arrangement() {
        // The guest's saved value 0x40200224: align to grid, no auto-arrange.
        assert_eq!(view_flags(Some(0x4020_0224), false), 0x4 | 0x20 | 0x100 | 0x200 | 0x400);
        assert_eq!(view_flags(Some(0x1), true) & 0x1001, 0x1001);
    }

    #[test]
    fn intersect_clips() {
        let a = Rect::new(-71, 0, 1422, 800);
        assert_eq!(a.intersect(&Rect::new(0, 0, 1280, 800)), Rect::new(0, 0, 1280, 800));
        assert!(a.intersect(&Rect::new(2000, 0, 10, 10)).is_empty());
    }
}
