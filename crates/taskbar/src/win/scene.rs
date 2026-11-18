//! Drawing the bar with crates/render: Start, task buttons with their icons, indicators, hover
//! and press states, the clock. Everything is laid out in DIPs and scaled to the monitor's DPI.

use crate::geom::{self as G, Row};
use crate::theme::{Rgba, Theme};
use render::text::{self, Align, Text};
use render::{Surface, rgba};
use std::collections::HashMap;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1, ID2D1Brush, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, IDWriteTextFormat};
use windows_core::{Interface, Result};
use windows_numerics::Matrix3x2;

/// One slot as drawn: slot 0 is Start (`key` None).
pub struct Slot<'a> {
    pub key: Option<&'a str>,
    pub running: bool,
    pub active: bool,
    pub flashing: bool,
    pub hover: f32,
    pub press: f32,
    pub focused: bool,
}

pub struct Scene {
    surface: Surface,
    text: Text,
    clock_fmt: IDWriteTextFormat,
    start: ID2D1Bitmap1,
    icons: HashMap<String, Option<ID2D1Bitmap1>>,
    pub scale: f32,
    pub width: f32,
}

impl Scene {
    pub fn new(hwnd: HWND, w_px: u32, h_px: u32, scale: f32, theme: &Theme) -> Result<Scene> {
        let surface = Surface::new(hwnd, w_px, h_px)?;
        let text = Text::new()?;
        let clock_fmt = text.format("Segoe UI Variable Text", G::TEXT_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Trailing)?;
        let logo = start_image(theme.light, (G::START_SQ * scale).round() as u32);
        let start = surface.bitmap(logo.width, logo.height, &logo.to_premultiplied_bgra())?;
        Ok(Scene { surface, text, clock_fmt, start, icons: HashMap::new(), scale, width: w_px as f32 / scale })
    }

    pub fn icon_px(&self) -> i32 {
        (G::ICON * self.scale).round() as i32
    }

    pub fn has_icon(&self, key: &str) -> bool {
        self.icons.contains_key(key)
    }

    pub fn set_icon(&mut self, key: &str, img: Option<shelldata::Image>) {
        let bmp = img.and_then(|i| self.surface.bitmap(i.width, i.height, &i.to_premultiplied_bgra()).ok());
        self.icons.insert(key.to_string(), bmp);
    }

    fn brush(&self, c: Rgba) -> Result<ID2D1Brush> {
        let b: ID2D1SolidColorBrush = self.surface.brush(rgba(c.0, c.1, c.2, c.3))?;
        b.cast()
    }

    pub fn draw(&self, theme: &Theme, slots: &[Slot], clock: &(String, String)) -> Result<()> {
        let s = &self.surface;
        let ctx = &s.ctx;
        let row = Row { width: self.width, buttons: slots.len() };
        s.begin();
        // SAFETY: drawing calls between BeginDraw and EndDraw on this thread's context.
        unsafe {
            let k = self.scale;
            ctx.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 });
            ctx.FillRectangle(&rect(0.0, 0.0, self.width, 1.0 / k), &self.brush(theme.top_line)?);
            for (i, b) in slots.iter().enumerate() {
                let cx = row.cx(i);
                let (ox, y, w, h) = G::BUTTON_BG;
                let bg = rect(cx + ox, y, w, h);
                let rr = D2D1_ROUNDED_RECT { rect: bg, radiusX: G::BUTTON_RADIUS, radiusY: G::BUTTON_RADIUS };
                let mut fill = 0.0f32;
                if b.active {
                    fill = theme.active_bg.3;
                }
                fill = fill.max(b.hover * theme.hover_bg.3);
                if b.press > 0.0 {
                    fill = fill * (1.0 - b.press) + theme.press_bg.3 * b.press;
                }
                if b.flashing {
                    ctx.FillRoundedRectangle(&rr, &self.brush(theme.attention_bg)?);
                }
                if fill > 0.0 {
                    let c = theme.active_bg;
                    ctx.FillRoundedRectangle(&rr, &self.brush((c.0, c.1, c.2, fill))?);
                    if theme.active_stroke.3 > 0.0 && (b.active || b.hover > 0.0) {
                        let a = theme.active_stroke.3 * if b.active { 1.0 } else { b.hover };
                        let sr = D2D1_ROUNDED_RECT { rect: rect(bg.left - 0.5, bg.top - 0.5, w + 1.0, h + 1.0), radiusX: G::BUTTON_RADIUS + 0.5, radiusY: G::BUTTON_RADIUS + 0.5 };
                        let c = theme.active_stroke;
                        ctx.DrawRoundedRectangle(&sr, &self.brush((c.0, c.1, c.2, a))?, 1.0, None);
                    }
                }
                if b.focused {
                    let fr = D2D1_ROUNDED_RECT { rect: rect(bg.left + 1.0, bg.top + 1.0, w - 2.0, h - 2.0), radiusX: G::BUTTON_RADIUS, radiusY: G::BUTTON_RADIUS };
                    ctx.DrawRoundedRectangle(&fr, &self.brush(theme.focus)?, 2.0, None);
                }
                let shrink = 1.0 - (1.0 - G::PRESS_SCALE) * b.press;
                let icon = match b.key {
                    None => {
                        let side = (G::START_SQ * 2.0 + 1.0) * shrink;
                        let x = cx - side / 2.0;
                        let top = G::START_TOP + (G::START_SQ * 2.0 + 1.0 - side) / 2.0;
                        Some((&self.start, rect(x.round(), top, side, side)))
                    }
                    Some(key) => self.icons.get(key).and_then(|o| o.as_ref()).map(|bmp| {
                        let side = G::ICON * shrink;
                        let top = G::ICON_TOP + (G::ICON - side) / 2.0;
                        (bmp, rect(cx - side / 2.0, top, side, side))
                    }),
                };
                if let Some((bmp, r)) = icon {
                    ctx.DrawBitmap(bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                if b.running {
                    let (w, c) = if b.active { (G::ACTIVE_W, theme.active_ind) } else { (G::RUNNING_W, theme.running) };
                    let ir = D2D1_ROUNDED_RECT { rect: rect(cx - w / 2.0, G::INDICATOR_Y, w, G::INDICATOR_H), radiusX: 1.5, radiusY: 1.5 };
                    ctx.FillRoundedRectangle(&ir, &self.brush(c)?);
                }
            }
            let right = self.width - G::CLOCK_RIGHT;
            let tb = self.brush(theme.text)?;
            text::draw(ctx, &self.text, &self.clock_fmt, &clock.0, right - 100.0, right, G::LINE1_BASELINE, &tb, true);
            text::draw(ctx, &self.text, &self.clock_fmt, &clock.1, right - 100.0, right, G::LINE2_BASELINE, &tb, true);
        }
        s.end()
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

/// The Start logo: four squares of `sq` px, a 1 px gap, a diagonal gradient.
fn start_image(light: bool, sq: u32) -> shelldata::Image {
    let (a, b) = if light { ((120u8, 215u8, 250u8), (10u8, 128u8, 216u8)) } else { ((128, 224, 255), (14, 158, 255)) };
    let n = sq * 2 + 1;
    let mut bgra = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            if x == sq || y == sq {
                continue;
            }
            let t = (x + y) as f32 / (2 * (n - 1)) as f32;
            let m = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t).round() as u8;
            let i = ((y * n + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[m(a.2, b.2), m(a.1, b.1), m(a.0, b.0), 255]);
        }
    }
    shelldata::Image { width: n, height: n, bgra }
}
