//! Pure view math: fit, zoom about a point, rotation and pan limits.

/// Where the image sits in the window: its center, in window pixels, and its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
    /// Quarter turns clockwise, 0..4.
    pub turns: u8,
    /// True while the image follows the window (fit); zoom or pan turns it off.
    pub fit: bool,
}

pub const MIN_SCALE: f32 = 0.02;
pub const MAX_SCALE: f32 = 32.0;

impl View {
    /// Size of the image on screen before scaling, once rotated.
    pub fn turned(w: f32, h: f32, turns: u8) -> (f32, f32) {
        if turns % 2 == 1 { (h, w) } else { (w, h) }
    }

    /// Fits an image of `w` x `h` into the area, never upscaling past 1:1.
    pub fn fitted(w: f32, h: f32, turns: u8, area: (f32, f32, f32, f32)) -> View {
        let (ax, ay, aw, ah) = area;
        let (tw, th) = Self::turned(w, h, turns);
        let scale = if tw <= 0.0 || th <= 0.0 { 1.0 } else { (aw / tw).min(ah / th).min(1.0) };
        View { cx: ax + aw / 2.0, cy: ay + ah / 2.0, scale, turns, fit: true }
    }

    /// Zooms by `factor` keeping the image point under (px, py) in place.
    pub fn zoom_at(self, factor: f32, px: f32, py: f32) -> View {
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let k = scale / self.scale;
        View { cx: px + (self.cx - px) * k, cy: py + (self.cy - py) * k, scale, turns: self.turns, fit: false }
    }

    pub fn rotate(self, w: f32, h: f32, area: (f32, f32, f32, f32), clockwise: bool) -> View {
        let turns = if clockwise { (self.turns + 1) % 4 } else { (self.turns + 3) % 4 };
        if self.fit { Self::fitted(w, h, turns, area) } else { View { turns, ..self } }
    }

    /// Image rectangle on screen before rotation (left, top, right, bottom) around the center.
    pub fn rect(&self, w: f32, h: f32) -> (f32, f32, f32, f32) {
        let (hw, hh) = (w * self.scale / 2.0, h * self.scale / 2.0);
        (self.cx - hw, self.cy - hh, self.cx + hw, self.cy + hh)
    }
}

/// Extensions the viewer opens when Windows has the codec (lower case, no dot).
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff", "heic", "heif", "avif", "jxr", "wdp",
];

pub fn is_image(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Index of the next or previous entry, wrapping, as Photos does.
pub fn step(len: usize, at: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward { (at + 1) % len } else { (at + len - 1) % len }
}

/// "100%" label of a scale.
pub fn percent(scale: f32) -> String {
    format!("{}%", (scale * 100.0).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_upscales_and_centers() {
        let v = View::fitted(100.0, 50.0, 0, (0.0, 0.0, 1000.0, 800.0));
        assert_eq!(v.scale, 1.0);
        assert_eq!((v.cx, v.cy), (500.0, 400.0));
        let v = View::fitted(4000.0, 3000.0, 0, (0.0, 0.0, 800.0, 600.0));
        assert_eq!(v.scale, 0.2);
    }

    #[test]
    fn rotated_fit_swaps_sides() {
        let v = View::fitted(4000.0, 2000.0, 1, (0.0, 0.0, 1000.0, 1000.0));
        assert_eq!(v.scale, 0.25);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let v = View { cx: 100.0, cy: 100.0, scale: 1.0, turns: 0, fit: true };
        let z = v.zoom_at(2.0, 150.0, 100.0);
        // The point 50 px right of center was under the cursor; it is now 100 px right of the new center.
        assert_eq!(z.cx, 50.0);
        assert!(!z.fit);
        assert_eq!(z.scale, 2.0);
    }

    #[test]
    fn steps_wrap_and_extensions_match() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert!(is_image("a.JPG") && is_image("b.webp") && !is_image("c.txt") && !is_image("jpg"));
        assert_eq!(percent(0.256), "26%");
    }
}
