//! Drawing: the image under its transform, the command bar (Fluent glyphs, hover and press
//! plates with Windows' 4 px radius), browse arrows at the sides, the zoom level.

use super::{App, log, theme};
use crate::view::{View, percent};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Matrix3x2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    ZoomIn,
    ZoomOut,
    Rotate,
    Delete,
    Copy,
    OpenWith,
    Full,
    Prev,
    Next,
}

const BAR: [(Button, char); 7] = [
    (Button::ZoomIn, '\u{E8A3}'),
    (Button::ZoomOut, '\u{E71F}'),
    (Button::Rotate, '\u{E7AD}'),
    (Button::Delete, '\u{E74D}'),
    (Button::Copy, '\u{E8C8}'),
    (Button::OpenWith, '\u{E7AC}'),
    (Button::Full, '\u{E740}'),
];

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

impl App {
    /// Buttons and their rectangles: the bar's buttons right-aligned, the arrows mid-height.
    fn buttons(&self) -> Vec<(Button, char, D2D_RECT_F)> {
        let k = self.k;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let mut out = Vec::new();
        if !self.fullscreen() {
            let (bw, top) = (40.0 * k, 6.0 * k);
            let mut x = w - 12.0 * k - bw * BAR.len() as f32;
            for (b, g) in BAR {
                out.push((b, g, rect(x, top, x + bw, top + 36.0 * k)));
                x += bw;
            }
        }
        if self.list.len() > 1 {
            let (bw, bh) = (40.0 * k, 64.0 * k);
            let a = self.area();
            let cy = a.1 + a.3 / 2.0;
            out.push((Button::Prev, '\u{E76B}', rect(12.0 * k, cy - bh / 2.0, 12.0 * k + bw, cy + bh / 2.0)));
            out.push((Button::Next, '\u{E76C}', rect(w - 12.0 * k - bw, cy - bh / 2.0, w - 12.0 * k, cy + bh / 2.0)));
        }
        let _ = h;
        out
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Button> {
        self.buttons().into_iter().find(|(_, _, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(b, _, _)| b)
    }

    pub fn paint(&mut self) {
        let p = theme::palette(self.light);
        let s = &self.surface;
        let ctx = &s.ctx;
        s.begin();
        unsafe {
            if self.fullscreen() {
                if let Ok(b) = s.brush(render::rgba(0, 0, 0, 1.0)) {
                    ctx.FillRectangle(&rect(0.0, 0.0, self.size.0 as f32, self.size.1 as f32), &b);
                }
            }
            if let Some((bmp, _)) = self.frames.get(self.frame) {
                let (w, h) = self.img;
                let View { cx, cy, scale, turns, .. } = self.view;
                let (sin, cos) = match turns % 4 {
                    1 => (1.0, 0.0),
                    2 => (0.0, -1.0),
                    3 => (-1.0, 0.0),
                    _ => (0.0, 1.0),
                };
                let (m11, m12, m21, m22) = (scale * cos, scale * sin, -scale * sin, scale * cos);
                let m = Matrix3x2 {
                    M11: m11,
                    M12: m12,
                    M21: m21,
                    M22: m22,
                    M31: cx - w / 2.0 * m11 - h / 2.0 * m21,
                    M32: cy - w / 2.0 * m12 - h / 2.0 * m22,
                };
                // The image stays under the command bar, as in Photos.
                let a = self.area();
                ctx.PushAxisAlignedClip(&rect(a.0, a.1, a.0 + a.2, a.1 + a.3), D2D1_ANTIALIAS_MODE_ALIASED);
                ctx.SetTransform(&m);
                let mode = if scale < 1.0 { D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC } else if scale >= 3.0 { D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR } else { D2D1_INTERPOLATION_MODE_LINEAR };
                ctx.DrawBitmap(bmp, Some(&rect(0.0, 0.0, w, h)), 1.0, mode, None, None);
                ctx.SetTransform(&Matrix3x2::identity());
                ctx.PopAxisAlignedClip();
            }
            let k = self.k;
            let (Ok(text), Ok(dim), Ok(hover), Ok(press), Ok(plate)) =
                (s.brush(p.text), s.brush(p.dim), s.brush(p.hover), s.brush(p.press), s.brush(p.plate))
            else {
                let _ = s.end();
                return;
            };
            if let Some(e) = &self.error {
                let a = self.area();
                render::text::draw(ctx, &self.text, &self.ui, e, a.0 + 24.0 * k, a.2 - 24.0 * k, a.1 + a.3 / 2.0, &dim, false);
            }
            if !self.fullscreen() {
                let name = self.path.rsplit('\\').next().unwrap_or("");
                let label = if self.list.len() > 1 { format!("{name}    {}/{}", self.at + 1, self.list.len()) } else { name.to_string() };
                render::text::draw(ctx, &self.text, &self.ui, &label, 16.0 * k, self.size.0 as f32 - 380.0 * k, 29.0 * k, &text, false);
                if !self.frames.is_empty() {
                    let zx = self.size.0 as f32 - 12.0 * k - 40.0 * k * BAR.len() as f32 - 70.0 * k;
                    render::text::draw(ctx, &self.text, &self.ui, &percent(self.view.scale), zx, zx + 64.0 * k, 29.0 * k, &dim, true);
                }
            }
            for (b, g, r) in self.buttons() {
                let side = matches!(b, Button::Prev | Button::Next);
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0 * k, radiusY: 4.0 * k };
                if side {
                    ctx.FillRoundedRectangle(&rr, &plate);
                }
                if self.pressed == Some(b) {
                    ctx.FillRoundedRectangle(&rr, &press);
                } else if self.hover == Some(b) {
                    ctx.FillRoundedRectangle(&rr, &hover);
                }
                let glyph = if b == Button::Full && self.fullscreen() { '\u{E73F}' } else { g };
                let brush = if self.pressed == Some(b) { &dim } else { &text };
                let mid = (r.top + r.bottom) / 2.0 + 6.0 * k;
                render::text::draw(ctx, &self.text, &self.glyphs, &glyph.to_string(), r.left, r.right, mid, brush, false);
            }
        }
        let _ = s.end();
        if self.first_frame && !self.frames.is_empty() {
            self.first_frame = false;
            log::line(&format!("first frame on screen {:.1} ms after process start", log::since_start()));
        }
    }
}
