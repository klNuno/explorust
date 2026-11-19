//! Drawing Task View and the snap layouts flyout with Direct2D on `render`'s composition surface,
//! in DIPs. DWM composes the window and desktop thumbnails over the boxes left clear.

use crate::layout::{self as L, Layout, Rect};
use crate::tv_layout::Strip;
use render::text::{self, Align, Text};
use render::{Surface, rgba};
use shelldata::Image;
use std::collections::HashMap;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, IDWriteTextFormat,
};
use windows_core::Result;
use windows_numerics::Matrix3x2;

const UI: &str = "Segoe UI Variable Text";
const ICONS: &str = "Segoe Fluent Icons";
const CLOSE_GLYPH: &str = "\u{E8BB}";
const ADD_GLYPH: &str = "\u{E710}";

fn d2d(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: d2d(r), radiusX: radius, radiusY: radius }
}

/// What the pointer is over in Task View.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hover {
    None,
    Window(usize),
    WindowClose(usize),
    Desk(usize),
    DeskClose(usize),
    NewDesk,
}

pub struct TvView<'a> {
    pub scale: f32,
    pub windows: &'a Layout,
    pub titles: Vec<&'a str>,
    pub keys: Vec<&'a str>,
    pub icons: &'a HashMap<String, Option<Image>>,
    pub strip: &'a Strip,
    pub desks: Vec<&'a str>,
    pub current: usize,
    pub hover: Hover,
    /// The window being dragged (its tile is drawn empty) and the desktop under it.
    pub dragging: Option<usize>,
    pub drop_on: Option<usize>,
    /// Placeholder text when no window is open on this desktop.
    pub empty: &'a str,
}

pub struct Painter {
    surface: Surface,
    text: Text,
    title: IDWriteTextFormat,
    center: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    big_glyph: IDWriteTextFormat,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

impl Painter {
    pub fn new(hwnd: HWND, w_px: u32, h_px: u32) -> Result<Self> {
        let surface = Surface::new(hwnd, w_px, h_px)?;
        let text = Text::new()?;
        let title = text.format(UI, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?;
        // SAFETY: character trimming with an ellipsis on a format we own.
        unsafe {
            let sign = text.factory.CreateEllipsisTrimmingSign(&title)?;
            let t = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            title.SetTrimming(&t, &sign)?;
        }
        let center = text.format(UI, 14.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let glyph = text.format(ICONS, 10.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let big_glyph = text.format(ICONS, 16.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        Ok(Self { surface, text, title, center, glyph, big_glyph, bitmaps: HashMap::new() })
    }

    fn bitmap(&mut self, key: &str, icons: &HashMap<String, Option<Image>>) -> Option<ID2D1Bitmap1> {
        if let Some(b) = self.bitmaps.get(key) {
            return Some(b.clone());
        }
        let img = icons.get(key)?.as_ref()?;
        let b = self.surface.bitmap(img.width, img.height, &img.to_premultiplied_bgra()).ok()?;
        self.bitmaps.insert(key.to_string(), b.clone());
        Some(b)
    }

    pub fn task_view(&mut self, v: &TvView) -> Result<()> {
        let white = self.surface.brush(rgba(255, 255, 255, 1.0))?;
        let dim = self.surface.brush(rgba(255, 255, 255, 0.786))?;
        let hover_fill = self.surface.brush(rgba(255, 255, 255, 0.0605))?;
        let card = self.surface.brush(rgba(255, 255, 255, 0.0326))?;
        let current_fill = self.surface.brush(rgba(255, 255, 255, 0.0837))?;
        let stroke = self.surface.brush(rgba(255, 255, 255, 0.0698))?;
        let red = self.surface.brush(rgba(0xC4, 0x2B, 0x1C, 1.0))?;
        let accent = self.surface.brush(rgba(0x4C, 0xC2, 0xFF, 1.0))?;
        let strip_bg = self.surface.brush(rgba(0x1C, 0x1C, 0x1C, 0.6))?;
        let icons: Vec<Option<ID2D1Bitmap1>> = v.keys.iter().map(|k| self.bitmap(k, v.icons)).collect();
        let ctx = self.surface.ctx.clone();
        self.surface.begin();
        // SAFETY: drawing calls between BeginDraw and EndDraw on our device context.
        unsafe {
            ctx.SetTransform(&Matrix3x2::scale(v.scale, v.scale));
            for (i, t) in v.windows.tiles.iter().enumerate() {
                if v.dragging == Some(i) {
                    continue;
                }
                let hovered = matches!(v.hover, Hover::Window(h) | Hover::WindowClose(h) if h == i);
                if hovered {
                    ctx.FillRoundedRectangle(&rounded(&t.rect, L::RADIUS), &hover_fill);
                    ctx.DrawRoundedRectangle(&rounded(&t.rect, L::RADIUS), &stroke, 1.0, None);
                }
                if let Some(Some(b)) = icons.get(i) {
                    ctx.DrawBitmap(b, Some(&d2d(&t.icon())), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                let tr = t.title();
                let title = v.titles.get(i).copied().unwrap_or("");
                text::draw(&ctx, &self.text, &self.title, title, tr.x, tr.right(), tr.y + 20.0, &white, false);
                if hovered {
                    let c = t.close();
                    if v.hover == Hover::WindowClose(i) {
                        ctx.FillRoundedRectangle(&rounded(&c, 4.0), &red);
                    }
                    text::draw(&ctx, &self.text, &self.glyph, CLOSE_GLYPH, c.x, c.right(), c.y + 19.0, &white, false);
                }
            }
            if v.windows.tiles.is_empty() || v.windows.tiles.len() == 1 && v.dragging == Some(0) {
                let s = self.surface_size();
                let y = (s.1 / v.scale - crate::tv_layout::STRIP_H) / 2.0;
                text::draw(&ctx, &self.text, &self.center, v.empty, 0.0, s.0 / v.scale, y, &dim, false);
            }
            // The desktops strip.
            if let (Some(first), true) = (v.strip.desks.first(), true) {
                let band = Rect::new(first.rect.x - 12.0, first.rect.y - 12.0, v.strip.new_desk.right() - first.rect.x + 24.0, first.rect.h + 24.0);
                ctx.FillRoundedRectangle(&rounded(&band, L::RADIUS), &strip_bg);
            }
            for (i, d) in v.strip.desks.iter().enumerate() {
                let hovered = matches!(v.hover, Hover::Desk(h) | Hover::DeskClose(h) if h == i) || v.drop_on == Some(i);
                let fill = if i == v.current { &current_fill } else { &card };
                ctx.FillRoundedRectangle(&rounded(&d.rect, L::RADIUS), fill);
                if hovered {
                    ctx.FillRoundedRectangle(&rounded(&d.rect, L::RADIUS), &hover_fill);
                }
                if v.drop_on == Some(i) {
                    ctx.DrawRoundedRectangle(&rounded(&d.rect, L::RADIUS), &accent, 2.0, None);
                }
                let name = v.desks.get(i).copied().unwrap_or("");
                text::draw(&ctx, &self.text, &self.title, name, d.label.x, d.label.right(), d.label.y + 21.0, &white, false);
                if i == v.current {
                    // Windows marks the current desktop with an accent bar under its picture.
                    let bar = Rect::new(d.thumb.x + d.thumb.w / 2.0 - 8.0, d.rect.bottom() - 5.0, 16.0, 3.0);
                    ctx.FillRoundedRectangle(&rounded(&bar, 1.5), &accent);
                }
                if hovered && v.desks.len() > 1 && v.drop_on.is_none() {
                    let c = d.close;
                    if v.hover == Hover::DeskClose(i) {
                        ctx.FillRoundedRectangle(&rounded(&c, 4.0), &red);
                    }
                    text::draw(&ctx, &self.text, &self.glyph, CLOSE_GLYPH, c.x, c.right(), c.y + 19.0, &white, false);
                }
            }
            let n = &v.strip.new_desk;
            ctx.FillRoundedRectangle(&rounded(n, L::RADIUS), if v.hover == Hover::NewDesk { &hover_fill } else { &card });
            ctx.DrawRoundedRectangle(&rounded(n, L::RADIUS), &stroke, 1.0, None);
            let cy = n.y + n.h / 2.0;
            text::draw(&ctx, &self.text, &self.big_glyph, ADD_GLYPH, n.x, n.right(), cy + 2.0, &white, false);
            text::draw(&ctx, &self.text, &self.center, "New desktop", n.x, n.right(), cy + 30.0, &white, false);
            ctx.SetTransform(&Matrix3x2::identity());
        }
        self.surface.end()
    }

    fn surface_size(&self) -> (f32, f32) {
        // SAFETY: plain query on our device context.
        let s = unsafe { self.surface.ctx.GetSize() };
        (s.width, s.height)
    }

    /// The snap layouts flyout: one rounded picture per layout, its zones as tiles, the hovered
    /// zone in the accent color.
    pub fn snap(&mut self, scale: f32, zones: &[Vec<Rect>], hover: Option<(usize, usize)>) -> Result<()> {
        let zone = self.surface.brush(rgba(255, 255, 255, 0.0837))?;
        let zone_stroke = self.surface.brush(rgba(255, 255, 255, 0.16))?;
        let accent = self.surface.brush(D2D1_COLOR_F { r: 0x4C as f32 / 255.0, g: 0xC2 as f32 / 255.0, b: 1.0, a: 1.0 })?;
        let ctx = self.surface.ctx.clone();
        self.surface.begin();
        // SAFETY: drawing calls between BeginDraw and EndDraw on our device context.
        unsafe {
            ctx.SetTransform(&Matrix3x2::scale(scale, scale));
            for (l, zs) in zones.iter().enumerate() {
                for (z, r) in zs.iter().enumerate() {
                    let on = hover == Some((l, z));
                    ctx.FillRoundedRectangle(&rounded(r, 4.0), if on { &accent } else { &zone });
                    if !on {
                        ctx.DrawRoundedRectangle(&rounded(r, 4.0), &zone_stroke, 1.0, None);
                    }
                }
            }
            ctx.SetTransform(&Matrix3x2::identity());
        }
        self.surface.end()
    }
}
