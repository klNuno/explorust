//! Drawing helpers over the render crate: solid fills, rounded rects, text and glyphs by
//! baseline or center, cached brushes are not needed at this scale.

use render::text::{self, Align, Text};
use render::{Color, Surface};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::*;
use windows_core::Result;

use crate::layout as L;

pub struct Fonts {
    pub ui12: IDWriteTextFormat,
    pub ui14: IDWriteTextFormat,
    pub list: IDWriteTextFormat,
    pub list_right: IDWriteTextFormat,
    pub group: IDWriteTextFormat,
    pub glyph10: IDWriteTextFormat,
    pub glyph12: IDWriteTextFormat,
    pub glyph14: IDWriteTextFormat,
    pub glyph16: IDWriteTextFormat,
}

impl Fonts {
    pub fn new(t: &Text) -> Result<Self> {
        let n = DWRITE_FONT_WEIGHT_NORMAL;
        let list = t.format(L::LIST_FONT, 12.0, n, Align::Leading)?;
        let list_right = t.format(L::LIST_FONT, 12.0, n, Align::Trailing)?;
        for f in [&list, &list_right] {
            ellipsis(t, f)?;
        }
        Ok(Self {
            ui12: t.format(L::UI_FONT, 12.0, n, Align::Leading)?,
            ui14: t.format(L::UI_FONT, 14.0, n, Align::Leading)?,
            list,
            list_right,
            group: t.format(L::LIST_FONT, 15.0, n, Align::Leading)?,
            glyph10: t.format(L::ICON_FONT, 10.0, n, Align::Center)?,
            glyph12: t.format(L::ICON_FONT, 12.0, n, Align::Center)?,
            glyph14: t.format(L::ICON_FONT, 14.0, n, Align::Center)?,
            glyph16: t.format(L::ICON_FONT, 16.0, n, Align::Center)?,
        })
    }
}

fn ellipsis(t: &Text, f: &IDWriteTextFormat) -> Result<()> {
    unsafe {
        let sign = t.factory.CreateEllipsisTrimmingSign(f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trim, &sign)
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub struct Painter<'a> {
    pub s: &'a Surface,
    pub t: &'a Text,
    pub f: &'a Fonts,
}

impl Painter<'_> {
    pub fn fill(&self, r: D2D_RECT_F, c: Color) -> Result<()> {
        let b = self.s.brush(c)?;
        unsafe { self.s.ctx.FillRectangle(&r, &b) };
        Ok(())
    }

    pub fn round(&self, r: D2D_RECT_F, radius: f32, fill: Color, stroke: Option<Color>) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        unsafe {
            self.s.ctx.FillRoundedRectangle(&rr, &self.s.brush(fill)?);
            if let Some(c) = stroke {
                self.s.ctx.DrawRoundedRectangle(&rr, &self.s.brush(c)?, 1.0, None);
            }
        }
        Ok(())
    }

    /// Text with its baseline at `baseline`, laid out in [x0, x1].
    pub fn text(&self, f: &IDWriteTextFormat, s: &str, x0: f32, x1: f32, baseline: f32, c: Color) -> Result<()> {
        let b = self.s.brush(c)?;
        text::draw(&self.s.ctx, self.t, f, s, x0, x1, baseline, &b.into(), false);
        Ok(())
    }

    /// Width of `s` laid out in `f`.
    pub fn measure(&self, f: &IDWriteTextFormat, s: &str) -> f32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(l) = self.t.factory.CreateTextLayout(&w, f, 4000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }

    /// A Segoe Fluent Icons glyph centered on (cx, cy).
    pub fn glyph(&self, f: &IDWriteTextFormat, g: char, cx: f32, cy: f32, size: f32, c: Color) -> Result<()> {
        // Fluent glyphs sit on the em box: the box top is the baseline minus the ascent,
        // and the glyph fills the em square from that top.
        let top = cy - size / 2.0;
        let base = top + self.t.baseline(f);
        self.text(f, &g.to_string(), cx - size, cx + size, base, c)
    }

    pub fn bitmap(&self, b: &ID2D1Bitmap1, x: f32, y: f32, size: f32) {
        unsafe {
            self.s.ctx.DrawBitmap(b, Some(&rect(x, y, size, size)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None)
        };
    }
}
