//! Small popups of the bar, drawn with crates/render: the tooltip of a tray icon and the overflow
//! flyout behind the chevron. Both use the bar's window procedure; the bar tells them apart by
//! handle.

use crate::right;
use crate::theme::Theme;
use render::text::{Align, Text};
use render::{Surface, rgba};
use std::collections::HashMap;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS, IDWriteTextFormat};
use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWMWCP_ROUNDSMALL, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Result, w};
use windows_numerics::{Matrix3x2, Vector2};

pub const CLASS: windows_core::PCWSTR = w!("explorust.taskbar.popup");
const TIP_PAD_X: f32 = 8.0;
const TIP_H: f32 = 30.0;
const TIP_GAP: f32 = 8.0;

/// Registers the popup class with the bar's window procedure.
pub fn register(proc_: WNDPROC) {
    // SAFETY: class registration for this module.
    unsafe {
        let Ok(inst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW { lpfnWndProc: proc_, hInstance: inst.into(), lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
    }
}

fn create(x: i32, y: i32, w: i32, h: i32, activate: bool, corner: i32) -> Result<HWND> {
    // SAFETY: creates a popup window of our class.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let mut ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP;
        if !activate {
            ex |= WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
        }
        let hwnd = CreateWindowExW(ex, CLASS, w!(""), WS_POPUP, x, y, w, h, None, None, Some(inst.into()), None)?;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        Ok(hwnd)
    }
}

pub struct Popup {
    pub hwnd: HWND,
    surface: Surface,
    text: Text,
    format: IDWriteTextFormat,
    scale: f32,
    w: f32,
    h: f32,
}

impl Popup {
    fn new(hwnd: HWND, w_px: i32, h_px: i32, scale: f32) -> Result<Popup> {
        let surface = Surface::new(hwnd, w_px as u32, h_px as u32)?;
        let text = Text::new()?;
        let format = text.format("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?;
        Ok(Popup { hwnd, surface, text, format, scale, w: w_px as f32 / scale, h: h_px as f32 / scale })
    }

    fn begin(&self) {
        self.surface.begin();
        let k = self.scale;
        // SAFETY: a transform on our own context, between BeginDraw and EndDraw.
        unsafe { self.surface.ctx.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 }) };
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: destroys our own window.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Width of `s` in DIPs in the 12 px UI font.
fn text_width(text: &Text, format: &IDWriteTextFormat, s: &str) -> f32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: a DirectWrite layout measured and dropped.
    unsafe {
        let Ok(l) = text.factory.CreateTextLayout(&w, format, 4000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        let _ = l.GetMetrics(&mut m);
        m.widthIncludingTrailingWhitespace
    }
}

/// A tray icon's tooltip, centered over `anchor_x` (screen px) above the bar top `bar_top`.
pub fn tooltip(tip: &str, anchor_x: i32, bar_top: i32, scale: f32, theme: &Theme) -> Option<Popup> {
    let first = tip.lines().next().unwrap_or("");
    let lines: Vec<&str> = tip.lines().collect();
    let text = Text::new().ok()?;
    let fmt = text.format("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading).ok()?;
    let tw = lines.iter().map(|l| text_width(&text, &fmt, l)).fold(text_width(&text, &fmt, first), f32::max);
    let n = lines.len().max(1) as f32;
    let (w, h) = (((tw + 2.0 * TIP_PAD_X) * scale).ceil() as i32, ((TIP_H + 16.0 * (n - 1.0)) * scale).ceil() as i32);
    let x = anchor_x - w / 2;
    let y = bar_top - h - (TIP_GAP * scale) as i32;
    let hwnd = create(x, y, w, h, false, DWMWCP_ROUNDSMALL.0).ok()?;
    let p = Popup::new(hwnd, w, h, scale).ok()?;
    let (bg, border, fg) = if theme.light { ((249, 249, 249), (0, 0, 0, 0.0578), (0, 0, 0, 0.896)) } else { ((44, 44, 44), (0, 0, 0, 0.2), (255, 255, 255, 1.0)) };
    p.begin();
    // SAFETY: drawing on the popup's context.
    unsafe {
        let ctx = &p.surface.ctx;
        let r = D2D_RECT_F { left: 0.5, top: 0.5, right: p.w - 0.5, bottom: p.h - 0.5 };
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0, radiusY: 4.0 };
        if let Ok(b) = p.surface.brush(rgba(bg.0, bg.1, bg.2, 1.0)) {
            ctx.FillRoundedRectangle(&rr, &b);
        }
        if let Ok(b) = p.surface.brush(rgba(border.0, border.1, border.2, border.3)) {
            ctx.DrawRoundedRectangle(&rr, &b, 1.0, None);
        }
        if let Ok(b) = p.surface.brush(rgba(fg.0, fg.1, fg.2, fg.3)) {
            for (i, l) in lines.iter().enumerate() {
                let wl: Vec<u16> = l.encode_utf16().collect();
                if let Ok(layout) = p.text.factory.CreateTextLayout(&wl, &p.format, p.w, p.h) {
                    ctx.DrawTextLayout(Vector2 { X: TIP_PAD_X, Y: 6.0 + 16.0 * i as f32 }, &layout, &b, Default::default());
                }
            }
        }
    }
    let _ = p.surface.end();
    // SAFETY: shows our popup without activating it.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Some(p)
}

/// The overflow flyout: the hidden icons in a grid above the chevron.
pub struct Flyout {
    pub popup: Popup,
    /// Tray indexes shown, in cell order.
    pub icons: Vec<usize>,
    pub hover: Option<usize>,
    bitmaps: HashMap<isize, Option<ID2D1Bitmap1>>,
}

impl Flyout {
    /// Opens over `anchor_x` (screen px, the chevron's center) above `bar_top`, activated so a
    /// click elsewhere closes it.
    pub fn open(icons: Vec<usize>, anchor_x: i32, bar_top: i32, scale: f32, theme: &Theme) -> Option<Flyout> {
        let (w, h) = right::flyout_size(icons.len());
        let (w, h) = ((w * scale).round() as i32, (h * scale).round() as i32);
        let mut work = RECT::default();
        // SAFETY: reads the work area.
        let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
        let x = (anchor_x - w / 2).clamp(work.left + 12, (work.right - 12 - w).max(work.left));
        let y = bar_top - h - (12.0 * scale) as i32;
        let hwnd = create(x, y, w, h, true, DWMWCP_ROUND.0).ok()?;
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: theme.tint_abgr });
        let round = DWMWCP_ROUND.0;
        // SAFETY: the backdrop squares the corners; the flyout keeps Windows' rounded ones.
        let _ = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4) };
        let popup = Popup::new(hwnd, w, h, scale).ok()?;
        Some(Flyout { popup, icons, hover: None, bitmaps: HashMap::new() })
    }

    pub fn set_bitmap(&mut self, hicon: isize, img: Option<shelldata::Image>) {
        let bmp = img.and_then(|i| self.popup.surface.bitmap(i.width, i.height, &i.to_premultiplied_bgra()).ok());
        self.bitmaps.insert(hicon, bmp);
    }

    pub fn has_bitmap(&self, hicon: isize) -> bool {
        self.bitmaps.contains_key(&hicon)
    }

    pub fn icon_px(&self) -> i32 {
        (right::TRAY_ICON * self.popup.scale).round() as i32
    }

    /// The cell under a client point in px.
    pub fn hit(&self, x_px: i32, y_px: i32) -> Option<usize> {
        let k = self.popup.scale;
        right::flyout_hit(self.icons.len(), x_px as f32 / k, y_px as f32 / k)
    }

    /// Screen center of cell `i`, the point given to version 4 icons.
    pub fn cell_center(&self, i: usize) -> (i32, i32) {
        let mut r = RECT::default();
        // SAFETY: reads our own window's rect.
        let _ = unsafe { GetWindowRect(self.popup.hwnd, &mut r) };
        let (cx, cy) = right::flyout_cell(i);
        let k = self.popup.scale;
        let half = right::FLYOUT_CELL / 2.0;
        (r.left + ((cx + half) * k) as i32, r.top + ((cy + half) * k) as i32)
    }

    pub fn draw(&self, theme: &Theme, hicons: &[isize]) {
        let p = &self.popup;
        p.begin();
        // SAFETY: drawing on the flyout's context.
        unsafe {
            let ctx = &p.surface.ctx;
            let (bg, a) = if theme.light { ((243, 243, 243), 0.6) } else { ((44, 44, 44), 0.6) };
            if let Ok(b) = p.surface.brush(rgba(bg.0, bg.1, bg.2, a)) {
                ctx.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: p.w, bottom: p.h }, &b);
            }
            for (cell, hicon) in hicons.iter().enumerate() {
                let (x, y) = right::flyout_cell(cell);
                let c = right::FLYOUT_CELL;
                if self.hover == Some(cell) {
                    let hb = theme.hover_bg;
                    let alpha = if theme.light { 0.06 } else { hb.3 };
                    let fill = if theme.light { (0, 0, 0) } else { (hb.0, hb.1, hb.2) };
                    if let Ok(b) = p.surface.brush(rgba(fill.0, fill.1, fill.2, alpha)) {
                        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x + 2.0, top: y + 2.0, right: x + c - 2.0, bottom: y + c - 2.0 }, radiusX: 4.0, radiusY: 4.0 };
                        ctx.FillRoundedRectangle(&rr, &b);
                    }
                }
                if let Some(Some(bmp)) = self.bitmaps.get(hicon) {
                    let s = right::TRAY_ICON;
                    let r = D2D_RECT_F { left: x + (c - s) / 2.0, top: y + (c - s) / 2.0, right: x + (c + s) / 2.0, bottom: y + (c + s) / 2.0 };
                    ctx.DrawBitmap(bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }
        }
        let _ = p.surface.end();
    }
}
