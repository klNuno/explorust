//! One toast card: a topmost no-activate popup with DWM acrylic and Windows 11's rounded
//! corners, drawn with Direct2D: app icon and name, the close button, the title and body.

use crate::model::{self, Toast, geo};
use render::text::{Align, Text};
use render::{Surface, rgba};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PCWSTR, Result, w};

pub struct Card {
    pub hwnd: HWND,
    pub toast: Toast,
    pub scale: f32,
    pub height_dip: f32,
    pub hover: bool,
    pub hover_close: bool,
    /// On screen (a card waits hidden while Windows' own popup may show instead).
    pub shown: bool,
    dark: bool,
    surface: Surface,
    text: Text,
    icon: Option<ID2D1Bitmap1>,
}

pub fn create_window(class: PCWSTR) -> Result<HWND> {
    // SAFETY: a popup owned by this thread, shown later without activation.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("New notification"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )
    }
}

impl Card {
    pub fn new(hwnd: HWND, toast: Toast) -> Result<Card> {
        // SAFETY: plain queries on our own window.
        let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let height_dip = model::card_height(toast.body().len());
        let (pw, ph) = ((geo::WIDTH * scale).round() as u32, (height_dip * scale).round() as u32);
        let dark = super::system::dark();
        let tint = if dark { 0xE0_2C_2C_2C } else { 0xE0_EE_EE_EE };
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: tint });
        let round = DWMWCP_ROUND;
        // SAFETY: sets Windows 11's rounded corners on our window (after the backdrop chose square).
        unsafe {
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
            let _ = SetWindowPos(hwnd, None, 0, 0, pw as i32, ph as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let surface = Surface::new(hwnd, pw, ph)?;
        // SAFETY: draw in DIPs, D2D maps them to pixels.
        unsafe { surface.ctx.SetDpi(96.0 * scale, 96.0 * scale) };
        let icon_px = (geo::ICON * scale).round() as i32;
        let icon = shelldata::items::apps_folder_item(&toast.aumid)
            .and_then(|item| shelldata::icons::icon_for_item(&item, icon_px))
            .and_then(|img| surface.bitmap(img.width, img.height, &img.to_premultiplied_bgra()).ok());
        let card = Card { hwnd, toast, scale, height_dip, hover: false, hover_close: false, shown: false, dark, surface, text: Text::new()?, icon };
        card.paint();
        Ok(card)
    }

    pub fn paint(&self) {
        if let Err(e) = self.draw() {
            crate::log::line(&format!("toast {} not drawn: {e}", self.toast.id));
        }
    }

    fn draw(&self) -> Result<()> {
        let ctx = &self.surface.ctx;
        let (primary, secondary) = if self.dark {
            (rgba(255, 255, 255, 1.0), rgba(255, 255, 255, 0.79))
        } else {
            (rgba(0, 0, 0, 0.9), rgba(0, 0, 0, 0.62))
        };
        let primary = self.surface.brush(primary)?;
        let secondary = self.surface.brush(secondary)?;
        let w = geo::WIDTH;
        let small = self.format("Segoe UI Variable Small", 12.0)?;
        let body = self.format("Segoe UI Variable Text", 14.0)?;
        let glyphs = self.text.format("Segoe Fluent Icons", 10.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        self.surface.begin();
        // SAFETY: drawing calls between begin and end on our device context.
        unsafe {
            if let Some(icon) = &self.icon {
                let y = geo::CLOSE_CY - geo::ICON / 2.0;
                let r = D2D_RECT_F { left: geo::PAD, top: y, right: geo::PAD + geo::ICON, bottom: y + geo::ICON };
                ctx.DrawBitmap(icon, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
            if self.hover_close {
                let hb = self.surface.brush(if self.dark { rgba(255, 255, 255, 0.08) } else { rgba(0, 0, 0, 0.06) })?;
                let cx = w - geo::CLOSE_DX;
                let half = geo::CLOSE_HIT / 2.0;
                let r = D2D_RECT_F { left: cx - half, top: geo::CLOSE_CY - half, right: cx + half, bottom: geo::CLOSE_CY + half };
                ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0, radiusY: 4.0 }, &hb);
            }
        }
        let header_x = geo::PAD + geo::ICON + 8.0;
        render::text::draw(ctx, &self.text, &small, &self.toast.app, header_x, w - 90.0, geo::CLOSE_CY + 4.0, &secondary, false);
        let more_cx = w - geo::CLOSE_DX - 39.0;
        render::text::draw(ctx, &self.text, &glyphs, "\u{E712}", more_cx - 16.0, more_cx + 16.0, geo::CLOSE_CY + 4.0, &secondary, false);
        let close_cx = w - geo::CLOSE_DX;
        render::text::draw(ctx, &self.text, &glyphs, "\u{E8BB}", close_cx - 16.0, close_cx + 16.0, geo::CLOSE_CY + 4.0, &primary, false);
        let mut baseline = geo::HEADER + 4.0 + 15.0;
        render::text::draw(ctx, &self.text, &body, self.toast.title(), geo::PAD, w - geo::PAD, baseline, &primary, false);
        for line in self.toast.body() {
            baseline += geo::LINE;
            render::text::draw(ctx, &self.text, &body, line, geo::PAD, w - geo::PAD, baseline, &secondary, false);
        }
        self.surface.end()
    }

    /// A one-line format that ends with an ellipsis when the text is too long, as Windows trims.
    fn format(&self, family: &str, size: f32) -> Result<IDWriteTextFormat> {
        let f = self.text.format(family, size, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?;
        // SAFETY: plain DirectWrite calls on a format we own.
        unsafe {
            let sign = self.text.factory.CreateEllipsisTrimmingSign(&f)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            f.SetTrimming(&trim, &sign)?;
        }
        Ok(f)
    }
}
