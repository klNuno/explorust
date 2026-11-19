//! The hover thumbnail flyout above a task button: one card per window of the group with its
//! app icon, title and close button over a live DWM thumbnail (`DwmRegisterThumbnail`). It never
//! takes the activation; the bar drives its timers and clicks.

use crate::theme::Theme;
use crate::thumbs::{self as T, Layout};
use render::text::{Align, Text};
use render::{Surface, rgba};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Result, w};
use windows_numerics::{Matrix3x2, Vector2};

pub struct ThumbFlyout {
    pub hwnd: HWND,
    pub slot: usize,
    pub wins: Vec<HWND>,
    pub hover: Option<(usize, bool)>,
    surface: Surface,
    text: Text,
    title_fmt: IDWriteTextFormat,
    glyph_fmt: IDWriteTextFormat,
    titles: Vec<String>,
    icon: Option<ID2D1Bitmap1>,
    thumbs: Vec<isize>,
    layout: Layout,
    scale: f32,
}

fn title_of(h: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: fills a local buffer.
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

impl ThumbFlyout {
    /// Opens over `anchor_x` (screen px, the button's center) above `bar_top`.
    pub fn open(slot: usize, wins: Vec<HWND>, icon: Option<shelldata::Image>, anchor_x: i32, bar_top: i32, scale: f32, theme: &Theme) -> Result<ThumbFlyout> {
        // SAFETY: window creation, DWM thumbnails and drawing on this thread's own windows.
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(ex, super::popup::CLASS, w!(""), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst.into()), None)?;
            let mut thumbs = Vec::new();
            let mut sizes = Vec::new();
            for h in &wins {
                let (id, sz) = match DwmRegisterThumbnail(hwnd, *h) {
                    Ok(id) => (id, DwmQueryThumbnailSourceSize(id).unwrap_or_default()),
                    Err(_) => (0, Default::default()),
                };
                thumbs.push(id);
                sizes.push((sz.cx as f32, sz.cy as f32));
            }
            let layout = T::layout(&sizes);
            let (w, h) = ((layout.w * scale).round() as i32, (layout.h * scale).round() as i32);
            let mut work = RECT::default();
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            let x = T::place_x(anchor_x as f32, w as f32, (work.left + 12) as f32, (work.right - 12) as f32) as i32;
            let y = bar_top - h - (T::ABOVE_BAR * scale) as i32;
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: theme.tint_abgr });
            let round = DWMWCP_ROUND.0;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
            let surface = Surface::new(hwnd, w as u32, h as u32)?;
            let text = Text::new()?;
            let title_fmt = text.format("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?;
            let _ = title_fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let _ = title_fmt.SetTrimming(&trim, None);
            let glyph_fmt = text.format("Segoe Fluent Icons", 10.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
            let icon = icon.and_then(|i| surface.bitmap(i.width, i.height, &i.to_premultiplied_bgra()).ok());
            let titles = wins.iter().map(|h| title_of(*h)).collect();
            let f = ThumbFlyout { hwnd, slot, wins, hover: None, surface, text, title_fmt, glyph_fmt, titles, icon, thumbs, layout, scale };
            f.place_thumbs();
            f.draw(theme);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(f)
        }
    }

    fn place_thumbs(&self) {
        let k = self.scale;
        let px = |v: f32| (v * k).round() as i32;
        for (id, c) in self.thumbs.iter().zip(&self.layout.cards) {
            if *id == 0 {
                continue;
            }
            let r = c.thumb;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT { left: px(r.x), top: px(r.y), right: px(r.x + r.w), bottom: px(r.y + r.h) },
                opacity: 255,
                fVisible: true.into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            // SAFETY: a thumbnail registered on our window.
            let _ = unsafe { DwmUpdateThumbnailProperties(*id, &props) };
        }
    }

    /// The card and close button under a client point in px.
    pub fn hit(&self, x_px: i32, y_px: i32) -> Option<(usize, bool)> {
        T::hit(&self.layout, x_px as f32 / self.scale, y_px as f32 / self.scale)
    }

    pub fn draw(&self, theme: &Theme) {
        let s = &self.surface;
        s.begin();
        let k = self.scale;
        let (fg, card_hover, close_hover) = if theme.light {
            ((0u8, 0u8, 0u8, 0.896f32), (0u8, 0u8, 0u8, 0.0373f32), (196u8, 43u8, 28u8, 1.0f32))
        } else {
            ((255, 255, 255, 1.0), (255, 255, 255, 0.0605), (196, 43, 28, 1.0))
        };
        // SAFETY: drawing calls between BeginDraw and EndDraw on this window's context.
        unsafe {
            let ctx = &s.ctx;
            ctx.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 });
            let ink = s.brush(rgba(fg.0, fg.1, fg.2, fg.3));
            for (i, c) in self.layout.cards.iter().enumerate() {
                let hovered = self.hover.map(|h| h.0) == Some(i);
                if hovered {
                    if let Ok(b) = s.brush(rgba(card_hover.0, card_hover.1, card_hover.2, card_hover.3)) {
                        let r = c.card;
                        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }, radiusX: T::RADIUS, radiusY: T::RADIUS };
                        ctx.FillRoundedRectangle(&rr, &b);
                    }
                }
                let t = c.title;
                let mut text_x = t.x + T::PAD;
                if let Some(bmp) = &self.icon {
                    let top = t.y + (t.h - T::ICON) / 2.0;
                    let r = D2D_RECT_F { left: t.x + T::PAD, top, right: t.x + T::PAD + T::ICON, bottom: top + T::ICON };
                    ctx.DrawBitmap(bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                    text_x += T::ICON + 8.0;
                }
                let text_right = if hovered { c.close.x - 2.0 } else { t.x + t.w - T::PAD };
                if let (Ok(b), Some(title)) = (&ink, self.titles.get(i)) {
                    let wt: Vec<u16> = title.encode_utf16().collect();
                    if let Ok(l) = self.text.factory.CreateTextLayout(&wt, &self.title_fmt, (text_right - text_x).max(1.0), t.h) {
                        let top = t.y + (t.h - 16.0) / 2.0;
                        ctx.DrawTextLayout(Vector2 { X: text_x, Y: top }, &l, b, Default::default());
                    }
                }
                if hovered {
                    let cr = c.close;
                    let on_close = self.hover == Some((i, true));
                    if on_close {
                        if let Ok(b) = s.brush(rgba(close_hover.0, close_hover.1, close_hover.2, close_hover.3)) {
                            let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: cr.x, top: cr.y, right: cr.x + cr.w, bottom: cr.y + cr.h }, radiusX: T::RADIUS, radiusY: T::RADIUS };
                            ctx.FillRoundedRectangle(&rr, &b);
                        }
                    }
                    let gb = if on_close { s.brush(rgba(255, 255, 255, 1.0)) } else { s.brush(rgba(fg.0, fg.1, fg.2, fg.3)) };
                    if let Ok(b) = gb {
                        let wt: Vec<u16> = "\u{E8BB}".encode_utf16().collect();
                        if let Ok(l) = self.text.factory.CreateTextLayout(&wt, &self.glyph_fmt, cr.w, cr.h) {
                            ctx.DrawTextLayout(Vector2 { X: cr.x, Y: cr.y + (cr.h - 13.0) / 2.0 }, &l, &b, Default::default());
                        }
                    }
                }
            }
        }
        let _ = s.end();
    }
}

impl Drop for ThumbFlyout {
    fn drop(&mut self) {
        // SAFETY: thumbnails and the window this flyout owns.
        unsafe {
            for id in self.thumbs.drain(..) {
                if id != 0 {
                    let _ = DwmUnregisterThumbnail(id);
                }
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
