//! Drawing the panel with Direct2D on `render`'s composition surface, in DIPs: tile hover fill,
//! app icon, title in Segoe UI Variable, the close glyph of Segoe Fluent Icons and the selection
//! ring. The DWM thumbnails are composed by DWM over the transparent thumbnail boxes.

use crate::layout::{self as L, Layout, Rect};
use render::text::{self, Align, Text};
use render::{Surface, rgba};
use shelldata::Image;
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, IDWriteTextFormat,
};
use windows::Win32::Foundation::HWND;
use windows_core::Result;
use windows_numerics::Matrix3x2;

const UI: &str = "Segoe UI Variable Text";
const CLOSE_GLYPH: &str = "\u{E8BB}";

/// What one frame shows.
pub struct View<'a> {
    pub scale: f32,
    pub layout: &'a Layout,
    pub titles: Vec<&'a str>,
    pub keys: Vec<&'a str>,
    pub icons: &'a HashMap<String, Option<Image>>,
    pub sel: usize,
    pub ring: Rect,
    pub hover: Option<usize>,
    pub hover_close: bool,
}

pub struct Scene {
    surface: Surface,
    text: Text,
    title: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

fn d2d(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: d2d(r), radiusX: radius, radiusY: radius }
}

impl Scene {
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
        let glyph = text.format("Segoe Fluent Icons", 10.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        Ok(Self { surface, text, title, glyph, bitmaps: HashMap::new() })
    }

    pub fn resize(&self, w_px: u32, h_px: u32) -> Result<()> {
        self.surface.resize(w_px, h_px)
    }

    pub fn keep_icons(&mut self, icons: &HashMap<String, Option<Image>>) {
        self.bitmaps.retain(|key, _| icons.contains_key(key));
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

    pub fn draw(&mut self, v: &View) -> Result<()> {
        let white = self.surface.brush(rgba(255, 255, 255, 1.0))?;
        let fill = self.surface.brush(rgba(255, 255, 255, 0.0605))?;
        let red = self.surface.brush(rgba(0xC4, 0x2B, 0x1C, 1.0))?;
        let icons: Vec<Option<ID2D1Bitmap1>> = v.keys.iter().map(|k| self.bitmap(k, v.icons)).collect();
        let ctx = self.surface.ctx.clone();
        self.surface.begin();
        render::style::paint_base(&ctx, render::style::Part::Switcher);
        // SAFETY: drawing calls between BeginDraw and EndDraw on our device context.
        unsafe {
            ctx.SetTransform(&Matrix3x2::scale(v.scale, v.scale));
            for (i, t) in v.layout.tiles.iter().enumerate() {
                let hovered = v.hover == Some(i);
                if hovered || i == v.sel {
                    ctx.FillRoundedRectangle(&rounded(&t.rect, L::RADIUS), &fill);
                }
                if let Some(Some(b)) = icons.get(i) {
                    ctx.DrawBitmap(b, Some(&d2d(&t.icon())), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                let tr = t.title();
                let baseline = tr.y + 20.0;
                let title = v.titles.get(i).copied().unwrap_or("");
                text::draw(&ctx, &self.text, &self.title, title, tr.x, tr.right(), baseline, &white, false);
                if hovered || i == v.sel {
                    let c = t.close();
                    if hovered && v.hover_close {
                        ctx.FillRoundedRectangle(&rounded(&c, 4.0), &red);
                    }
                    text::draw(&ctx, &self.text, &self.glyph, CLOSE_GLYPH, c.x, c.right(), c.y + 19.0, &white, false);
                }
            }
            if !v.layout.tiles.is_empty() {
                let half = L::RING / 2.0;
                let r = Rect::new(v.ring.x + half, v.ring.y + half, v.ring.w - L::RING, v.ring.h - L::RING);
                ctx.DrawRoundedRectangle(&rounded(&r, L::RADIUS - half), &white, L::RING, None);
            }
            ctx.SetTransform(&Matrix3x2::identity());
        }
        self.surface.end()
    }
}
