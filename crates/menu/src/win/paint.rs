//! Drawing one menu level: dark flyout colours, Segoe UI Variable text, Segoe Fluent Icons glyphs,
//! and the handler's bitmaps.

use crate::model::{Entry, Hit, Kind, Layout, metrics::*, top_glyph};
use render::text::{Align, Text};
use render::style::{self, Material, Part};
use render::{Surface, rgba};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_core::Result;
use windows_numerics::{Matrix3x2, Vector2};

pub struct Fonts {
    pub text: Text,
    label: IDWriteTextFormat,
    shortcut: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    chevron: IDWriteTextFormat,
}

fn centered(f: IDWriteTextFormat) -> Result<IDWriteTextFormat> {
    // SAFETY: setters on a format this thread made.
    unsafe { f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)? };
    Ok(f)
}

impl Fonts {
    pub fn new() -> Result<Self> {
        let text = Text::new()?;
        let ui = "Segoe UI Variable Text";
        let icons = "Segoe Fluent Icons";
        let label = centered(text.format(ui, 14.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?)?;
        let shortcut = centered(text.format(ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Trailing)?)?;
        let glyph = centered(text.format(icons, 16.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?)?;
        let chevron = centered(text.format(icons, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?)?;
        // SAFETY: trimming setter on our format.
        unsafe {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            let _ = label.SetTrimming(&trim, None);
        }
        Ok(Self { text, label, shortcut, glyph, chevron })
    }

    fn width(&self, format: &IDWriteTextFormat, s: &str) -> f32 {
        if s.is_empty() {
            return 0.0;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: a throwaway layout to measure the string.
        unsafe {
            let Ok(l) = self.text.factory.CreateTextLayout(&w, format, 4096.0, 64.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace.ceil()
        }
    }

    /// Label and shortcut widths per entry, for `model::layout`.
    pub fn measure(&self, entries: &[Entry]) -> Vec<(f32, f32)> {
        entries
            .iter()
            .map(|e| match e.kind {
                Kind::Separator => (0.0, 0.0),
                _ => (self.width(&self.label, &e.label), self.width(&self.shortcut, &e.shortcut)),
            })
            .collect()
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

fn text(ctx: &ID2D1DeviceContext, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: drawing inside BeginDraw/EndDraw.
    unsafe { ctx.DrawText(&w, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL) };
}

/// What one level shows.
pub struct View<'a> {
    pub entries: &'a [Entry],
    pub top: &'a [(String, u32)],
    pub layout: &'a Layout,
    pub hover: Option<Hit>,
    /// The row whose submenu is open, drawn highlighted.
    pub open: Option<usize>,
    pub pressed: bool,
    pub bitmaps: &'a [Option<ID2D1Bitmap1>],
}

pub fn draw(s: &Surface, f: &Fonts, v: &View, scale: f32) -> Result<()> {
    let ctx = &s.ctx;
    let white = s.brush(rgba(255, 255, 255, 1.0))?;
    let grey = s.brush(rgba(255, 255, 255, 0.36))?;
    let shortcut = s.brush(rgba(255, 255, 255, 0.6))?;
    let hover = s.brush(rgba(255, 255, 255, if v.pressed { 0.04 } else { 0.065 }))?;
    let line = s.brush(rgba(255, 255, 255, 0.084))?;
    let fill = match style::current(Part::Menus).material {
        Material::Windows => Some(s.brush(rgba(44, 44, 44, 1.0))?),
        Material::Oled => Some(s.brush(rgba(0, 0, 0, 1.0))?),
        _ => None,
    };
    s.begin();
    // SAFETY: drawing calls between begin and end on this thread's device context.
    unsafe {
        ctx.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        let l = v.layout;
        if let Some(fill) = &fill {
            let bounds = D2D1_ROUNDED_RECT {
                rect: rect(0.0, 0.0, l.width, l.height),
                radiusX: CORNER_RADIUS,
                radiusY: CORNER_RADIUS,
            };
            ctx.FillRoundedRectangle(&bounds, fill);
        }
        for (i, ((verb, _), &(x, y, w, h))) in v.top.iter().zip(&l.top).enumerate() {
            if v.hover == Some(Hit::Top(i)) {
                ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: RADIUS, radiusY: RADIUS }, &hover);
            }
            text(ctx, &top_glyph(verb).to_string(), &f.glyph, rect(x, y, w, h), &white);
        }
        if !l.top.is_empty() {
            let y = l.rows.first().map(|r| r.0).unwrap_or(l.height) - (SEP_H - 4.0) / 2.0 - 0.5;
            ctx.DrawLine(Vector2 { X: 0.0, Y: y }, Vector2 { X: l.width, Y: y }, &line, 1.0, None);
        }
        for (i, (e, &(y, h))) in v.entries.iter().zip(&l.rows).enumerate() {
            if e.kind == Kind::Separator {
                let ly = y + (h / 2.0).floor() + 0.5;
                ctx.DrawLine(Vector2 { X: 0.0, Y: ly }, Vector2 { X: l.width, Y: ly }, &line, 1.0, None);
                continue;
            }
            if e.enabled && (v.hover == Some(Hit::Row(i)) || v.open == Some(i)) {
                let r = rect(INSET, y + 2.0, l.width - 2.0 * INSET, h - 4.0);
                ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: RADIUS, radiusY: RADIUS }, &hover);
            }
            let brush = if e.enabled { &white } else { &grey };
            let icon = rect(ICON_X, y + (h - ICON) / 2.0, ICON, ICON);
            if e.checked {
                let g = if e.radio { '\u{E915}' } else { '\u{E73E}' };
                text(ctx, &g.to_string(), &f.glyph, icon, brush);
            } else if let Some(Some(b)) = v.bitmaps.get(i) {
                let op = if e.enabled { 1.0 } else { 0.4 };
                ctx.DrawBitmap(b, Some(&icon), op, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            } else if let Some(g) = icon_glyph(e) {
                text(ctx, &g.to_string(), &f.glyph, icon, brush);
            }
            let right = l.width - RIGHT - if e.kind == Kind::Submenu { CHEVRON_W } else { 0.0 };
            text(ctx, &e.label, &f.label, rect(TEXT_X, y, (right - TEXT_X).max(0.0), h - 1.0), brush);
            if !e.shortcut.is_empty() {
                text(ctx, &e.shortcut, &f.shortcut, rect(TEXT_X, y, right - TEXT_X, h - 1.0), &shortcut);
            }
            if e.kind == Kind::Submenu {
                text(ctx, "\u{E76C}", &f.chevron, rect(l.width - RIGHT - 16.0, y, 16.0, h), brush);
            }
        }
    }
    s.end()
}

fn icon_glyph(e: &Entry) -> Option<char> {
    if e.kind == Kind::More {
        return Some('\u{E8A7}');
    }
    e.glyph.or_else(|| crate::model::glyph(&e.verb))
}
