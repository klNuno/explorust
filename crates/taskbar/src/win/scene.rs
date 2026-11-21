//! Drawing the bar with crates/render: Start, task buttons with their icons, indicators, hover
//! and press states, the clock. Everything is laid out in DIPs and scaled to the monitor's DPI.

use crate::geom::{self as G, Row};
use crate::right::{self, Hit, Layout};
use crate::theme::{Rgba, Theme};
use render::text::{self, Align, Text};
use render::{Surface, rgba};
use std::collections::HashMap;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1, ID2D1Brush, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS, IDWriteTextFormat};
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
    /// ITaskbarList3 progress and the animation clock in seconds (indeterminate).
    pub progress: Option<(crate::taskband::Progress, f32)>,
    /// The window whose overlay badge shows on this button.
    pub overlay: Option<isize>,
}

/// The right side as drawn.
pub struct RightView<'a> {
    pub layout: &'a Layout,
    /// Icon handles of the promoted tray icons, left to right.
    pub icons: &'a [isize],
    pub glyphs: &'a [char],
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    /// Keyboard focus (Win+B).
    pub focus: Option<Hit>,
}

pub struct Scene {
    surface: Surface,
    text: Text,
    clock_fmt: IDWriteTextFormat,
    glyph_fmt: IDWriteTextFormat,
    chevron_fmt: IDWriteTextFormat,
    start: ID2D1Bitmap1,
    icons: HashMap<String, Option<ID2D1Bitmap1>>,
    tray: HashMap<isize, Option<ID2D1Bitmap1>>,
    /// Overlay badges by window.
    overlays: HashMap<isize, ID2D1Bitmap1>,
    pub scale: f32,
    pub width: f32,
}

impl Scene {
    pub fn new(hwnd: HWND, w_px: u32, h_px: u32, scale: f32, theme: &Theme) -> Result<Scene> {
        let surface = Surface::new(hwnd, w_px, h_px)?;
        let text = Text::new()?;
        let clock_fmt = text.format("Segoe UI Variable Text", G::TEXT_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Trailing)?;
        let glyph_fmt = text.format("Segoe Fluent Icons", right::GLYPH, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let chevron_fmt = text.format("Segoe Fluent Icons", G::CHEVRON_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let logo = start_image(theme.light, (G::START_SQ * scale).round() as u32);
        let start = surface.bitmap(logo.width, logo.height, &logo.to_premultiplied_bgra())?;
        Ok(Scene { surface, text, clock_fmt, glyph_fmt, chevron_fmt, start, icons: HashMap::new(), tray: HashMap::new(), overlays: HashMap::new(), scale, width: w_px as f32 / scale })
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

    pub fn set_overlay(&mut self, hwnd: isize, img: Option<shelldata::Image>) {
        match img.and_then(|i| self.surface.bitmap(i.width, i.height, &i.to_premultiplied_bgra()).ok()) {
            Some(b) => {
                self.overlays.insert(hwnd, b);
            }
            None => {
                self.overlays.remove(&hwnd);
            }
        }
    }

    pub fn tray_px(&self) -> i32 {
        (right::TRAY_ICON * self.scale).round() as i32
    }

    pub fn has_tray_icon(&self, hicon: isize) -> bool {
        self.tray.contains_key(&hicon)
    }

    pub fn set_tray_icon(&mut self, hicon: isize, img: Option<shelldata::Image>) {
        let bmp = img.and_then(|i| self.surface.bitmap(i.width, i.height, &i.to_premultiplied_bgra()).ok());
        self.tray.insert(hicon, bmp);
    }

    /// Forgets the bitmaps of icon handles no longer shown.
    pub fn keep_tray_icons(&mut self, live: &[isize]) {
        self.tray.retain(|h, _| live.contains(h));
    }

    /// Width of the clock text in DIPs: the wider of its two lines.
    pub fn clock_width(&self, clock: &(String, String)) -> f32 {
        let measure = |s: &str| {
            let w: Vec<u16> = s.encode_utf16().collect();
            // SAFETY: a DirectWrite layout measured and dropped.
            unsafe {
                let Ok(l) = self.text.factory.CreateTextLayout(&w, &self.clock_fmt, 400.0, 100.0) else { return 0.0 };
                let t = self.text.factory.CreateTypography().ok();
                if let Some(t) = &t {
                    let f = windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE {
                        nameTag: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                        parameter: 1,
                    };
                    let _ = t.AddFontFeature(f);
                    let _ = l.SetTypography(t, windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE { startPosition: 0, length: w.len() as u32 });
                }
                let mut m = DWRITE_TEXT_METRICS::default();
                let _ = l.GetMetrics(&mut m);
                m.width
            }
        };
        measure(&clock.0).max(measure(&clock.1))
    }

    /// Chevron, promoted tray icons, quick settings glyphs and the hover states of the right side.
    fn draw_right(&self, theme: &Theme, v: &RightView) -> Result<()> {
        let ctx = &self.surface.ctx;
        let l = v.layout;
        let (top, h) = (right::BUTTON_TOP, right::BUTTON_H);
        // SAFETY: drawing calls between BeginDraw and EndDraw on this thread's context.
        unsafe {
            for hit in [v.hover, v.pressed].into_iter().flatten() {
                if hit == Hit::ShowDesktop {
                    continue;
                }
                let Some((a, b)) = l.span(hit) else { continue };
                let c = theme.active_bg;
                let alpha = if v.pressed == Some(hit) { theme.press_bg.3 } else { theme.hover_bg.3 };
                let rr = D2D1_ROUNDED_RECT { rect: rect(a, top, b - a, h), radiusX: G::BUTTON_RADIUS, radiusY: G::BUTTON_RADIUS };
                ctx.FillRoundedRectangle(&rr, &self.brush((c.0, c.1, c.2, alpha))?);
            }
            if let Some((a, b)) = v.focus.and_then(|f| l.span(f)) {
                let fr = D2D1_ROUNDED_RECT { rect: rect(a + 1.0, top + 1.0, b - a - 2.0, h - 2.0), radiusX: G::BUTTON_RADIUS, radiusY: G::BUTTON_RADIUS };
                ctx.DrawRoundedRectangle(&fr, &self.brush(theme.focus)?, 2.0, None);
            }
            let ink = self.brush(theme.text)?;
            if let Some((a, b)) = l.chevron {
                text::draw(ctx, &self.text, &self.chevron_fmt, "\u{E70E}", a, b, G::CHEVRON_BASELINE, &ink, false);
            }
            for (i, (a, b)) in l.icons.iter().enumerate() {
                let Some(Some(bmp)) = v.icons.get(i).and_then(|h| self.tray.get(h)) else { continue };
                let s = right::TRAY_ICON;
                let x = ((a + b) / 2.0 - s / 2.0).round();
                ctx.DrawBitmap(bmp, Some(&rect(x, right::GLYPH_TOP, s, s)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            }
            for (g, x) in v.glyphs.iter().zip(&l.glyphs) {
                text::draw(ctx, &self.text, &self.glyph_fmt, &g.to_string(), *x, x + right::GLYPH, G::GLYPH_BASELINE, &ink, false);
            }
        }
        Ok(())
    }

    /// The button's progress bar where the indicator is: a track the button's width, the filled
    /// part in the state's color, or a moving segment when indeterminate.
    fn draw_progress(&self, theme: &Theme, cx: f32, p: crate::taskband::Progress, t: f32) -> Result<()> {
        use crate::taskband::ProgressState as P;
        let ctx = &self.surface.ctx;
        let w = G::PROGRESS_W;
        let x0 = cx - w / 2.0;
        let y = G::INDICATOR_Y;
        let h = G::INDICATOR_H;
        let r = h / 2.0;
        let c = match p.state {
            P::Error => theme.progress_error,
            P::Paused => theme.progress_paused,
            _ => theme.progress,
        };
        // SAFETY: drawing calls between BeginDraw and EndDraw on this thread's context.
        unsafe {
            let track = D2D1_ROUNDED_RECT { rect: rect(x0, y, w, h), radiusX: r, radiusY: r };
            ctx.FillRoundedRectangle(&track, &self.brush(theme.progress_track)?);
            let (a, len) = if p.state == P::Indeterminate {
                let seg = w * 0.4;
                let k = (t % 1.5) / 1.5;
                let a = -seg + (w + seg) * k;
                (a.max(0.0), (a + seg).min(w) - a.max(0.0))
            } else {
                (0.0, (w * p.value).max(h))
            };
            if len > 0.0 {
                let fill = D2D1_ROUNDED_RECT { rect: rect(x0 + a, y, len, h), radiusX: r, radiusY: r };
                ctx.FillRoundedRectangle(&fill, &self.brush(c)?);
            }
        }
        Ok(())
    }

    fn brush(&self, c: Rgba) -> Result<ID2D1Brush> {
        let b: ID2D1SolidColorBrush = self.surface.brush(rgba(c.0, c.1, c.2, c.3))?;
        b.cast()
    }

    pub fn draw(&self, theme: &Theme, slots: &[Slot], clock: &(String, String), right: &RightView, left: bool) -> Result<()> {
        let s = &self.surface;
        let ctx = &s.ctx;
        let row = Row { width: self.width, buttons: slots.len(), left };
        s.begin();
        render::style::paint_base(ctx, render::style::Part::Taskbar);
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
                if fill > 0.0 && !b.flashing {
                    // Windows draws a 38 px fill inside a 1 px stroke that closes the 40 px box.
                    let c = theme.active_bg;
                    let inner = D2D1_ROUNDED_RECT { rect: rect(bg.left + 1.0, bg.top + 1.0, w - 2.0, h - 2.0), radiusX: G::BUTTON_RADIUS - 1.0, radiusY: G::BUTTON_RADIUS - 1.0 };
                    ctx.FillRoundedRectangle(&inner, &self.brush((c.0, c.1, c.2, fill))?);
                    let k = if b.active { 1.0 } else { b.hover };
                    if theme.active_stroke.3 > 0.0 && k > 0.0 {
                        let sr = D2D1_ROUNDED_RECT { rect: rect(bg.left + 0.5, bg.top + 0.5, w - 1.0, h - 1.0), radiusX: G::BUTTON_RADIUS - 0.5, radiusY: G::BUTTON_RADIUS - 0.5 };
                        let c = theme.active_stroke;
                        ctx.DrawRoundedRectangle(&sr, &self.brush((c.0, c.1, c.2, c.3 * k))?, 1.0, None);
                        let c = theme.active_stroke_bottom;
                        let line = rect(bg.left + G::BUTTON_RADIUS, bg.bottom - 1.0, w - 2.0 * G::BUTTON_RADIUS, 1.0);
                        ctx.FillRectangle(&line, &self.brush((c.0, c.1, c.2, c.3 * k))?);
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
                if let Some(Some(bmp)) = b.overlay.map(|h| self.overlays.get(&h)) {
                    let o = super::taskband::OVERLAY;
                    let r = rect(cx + G::ICON / 2.0 - o + G::OVERLAY_OUT, G::ICON_TOP + G::ICON - o + G::OVERLAY_OUT, o, o);
                    ctx.DrawBitmap(bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                if let Some((p, t)) = b.progress {
                    self.draw_progress(theme, cx, p, t)?;
                } else if b.running {
                    let (w, c) = if b.flashing {
                        (G::ATTENTION_W, theme.attention_ind)
                    } else if b.active {
                        (G::ACTIVE_W, theme.active_ind)
                    } else {
                        (G::RUNNING_W, theme.running)
                    };
                    let ir = D2D1_ROUNDED_RECT { rect: rect(cx - w / 2.0, G::INDICATOR_Y, w, G::INDICATOR_H), radiusX: 1.5, radiusY: 1.5 };
                    ctx.FillRoundedRectangle(&ir, &self.brush(c)?);
                }
            }
            self.draw_right(theme, right)?;
            let r = self.width - G::CLOCK_RIGHT;
            let tb = self.brush(theme.text)?;
            text::draw(ctx, &self.text, &self.clock_fmt, &clock.0, r - 200.0, r, G::LINE1_BASELINE, &tb, true);
            text::draw(ctx, &self.text, &self.clock_fmt, &clock.1, r - 200.0, r, G::LINE2_BASELINE, &tb, true);
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
