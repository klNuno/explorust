//! Snap layouts (Win+Z), pure: the layouts Windows 11 offers for the monitor's width, each a set of
//! zones as fractions of the work area, the flyout geometry that draws them as small rounded
//! pictures, and the pixel rectangle a zone snaps a window to.

use crate::layout::Rect;

/// A zone in fractions of the work area: x, y, w, h in 0..1.
pub type Zone = (f32, f32, f32, f32);

/// The layouts for a work area `w` DIPs wide: four on screens under 1920 DIPs, six from 1920 on.
pub fn layouts(w: f32) -> Vec<Vec<Zone>> {
    let t = 1.0 / 3.0;
    let mut v = vec![
        vec![(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)],
        vec![(0.0, 0.0, 2.0 * t, 1.0), (2.0 * t, 0.0, t, 1.0)],
    ];
    if w >= 1920.0 {
        v.push(vec![(0.0, 0.0, t, 1.0), (t, 0.0, t, 1.0), (2.0 * t, 0.0, t, 1.0)]);
    }
    v.push(vec![(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)]);
    v.push(vec![(0.0, 0.0, 0.5, 0.5), (0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)]);
    if w >= 1920.0 {
        v.push(vec![(0.0, 0.0, 0.25, 1.0), (0.25, 0.0, 0.5, 1.0), (0.75, 0.0, 0.25, 1.0)]);
    }
    v
}

/// Flyout padding, picture size and gaps, in DIPs.
pub const PAD: f32 = 12.0;
pub const PIC_W: f32 = 96.0;
pub const PIC_H: f32 = 60.0;
pub const PIC_GAP: f32 = 12.0;
/// Gap between the zones inside a picture.
pub const ZONE_GAP: f32 = 4.0;
pub const PER_ROW: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Flyout {
    pub w: f32,
    pub h: f32,
    /// Per layout, the rectangle of each zone in the flyout.
    pub zones: Vec<Vec<Rect>>,
}

impl Flyout {
    /// (layout, zone) under a point.
    pub fn hit(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        for (l, zs) in self.zones.iter().enumerate() {
            if let Some(z) = zs.iter().position(|r| r.contains(x, y)) {
                return Some((l, z));
            }
        }
        None
    }
}

pub fn flyout(layouts: &[Vec<Zone>]) -> Flyout {
    let n = layouts.len();
    let cols = n.clamp(1, PER_ROW);
    let rows = n.div_ceil(PER_ROW).max(1);
    let w = 2.0 * PAD + cols as f32 * PIC_W + (cols - 1) as f32 * PIC_GAP;
    let h = 2.0 * PAD + rows as f32 * PIC_H + (rows - 1) as f32 * PIC_GAP;
    let half = ZONE_GAP / 2.0;
    let zones = layouts
        .iter()
        .enumerate()
        .map(|(i, zs)| {
            let px = PAD + (i % PER_ROW) as f32 * (PIC_W + PIC_GAP);
            let py = PAD + (i / PER_ROW) as f32 * (PIC_H + PIC_GAP);
            zs.iter()
                .map(|&(x, y, zw, zh)| {
                    let l = px + x * PIC_W + if x > 0.0 { half } else { 0.0 };
                    let t = py + y * PIC_H + if y > 0.0 { half } else { 0.0 };
                    let r = px + (x + zw) * PIC_W - if x + zw < 0.999 { half } else { 0.0 };
                    let b = py + (y + zh) * PIC_H - if y + zh < 0.999 { half } else { 0.0 };
                    Rect::new(l, t, r - l, b - t)
                })
                .collect()
        })
        .collect();
    Flyout { w, h, zones }
}

/// The pixel rectangle (left, top, right, bottom) of `zone` in a work area.
pub fn zone_px(zone: Zone, work: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (l, t, r, b) = work;
    let (w, h) = ((r - l) as f32, (b - t) as f32);
    let x0 = l + (zone.0 * w).round() as i32;
    let y0 = t + (zone.1 * h).round() as i32;
    let x1 = l + ((zone.0 + zone.2) * w).round() as i32;
    let y1 = t + ((zone.1 + zone.3) * h).round() as i32;
    (x0, y0, x1, y1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_follow_screen_width() {
        assert_eq!(layouts(1280.0).len(), 4);
        assert_eq!(layouts(1920.0).len(), 6);
        for l in layouts(2560.0) {
            let area: f32 = l.iter().map(|z| z.2 * z.3).sum();
            assert!((area - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn zones_fill_the_work_area() {
        let work = (0, 0, 1280, 752);
        assert_eq!(zone_px((0.0, 0.0, 0.5, 1.0), work), (0, 0, 640, 752));
        assert_eq!(zone_px((0.5, 0.5, 0.5, 0.5), work), (640, 376, 1280, 752));
        let t = 1.0 / 3.0;
        assert_eq!(zone_px((2.0 * t, 0.0, t, 1.0), work).2, 1280);
    }

    #[test]
    fn flyout_hits_zones() {
        let f = flyout(&layouts(1280.0));
        assert_eq!(f.zones.len(), 4);
        let z = f.zones[3][3];
        assert_eq!(f.hit(z.x + 1.0, z.y + 1.0), Some((3, 3)));
        assert_eq!(f.hit(1.0, 1.0), None);
        assert!(f.w > 0.0 && f.h > 0.0);
    }
}
