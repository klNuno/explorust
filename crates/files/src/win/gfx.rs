//! Files composition layers and the drawing helpers used by its chrome.

use render::{Color, Device, SwapChain};
use std::rc::Rc;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_core::{HSTRING, Result};
use windows_numerics::Vector2;

pub const UI_FONT: &str = "Segoe UI Variable Text";
pub const ICON_FONT: &str = "Segoe Fluent Icons";

pub struct Format {
    pub f: IDWriteTextFormat,
    /// Distance from the layout top to the baseline.
    pub baseline: f32,
}

pub struct Fonts {
    pub ui12: Format,
    pub ui14: Format,
    pub glyph10: Format,
    pub glyph12: Format,
    pub glyph14: Format,
    pub glyph16: Format,
}

pub struct Shared {
    device: Device,
    pub dcomp: IDCompositionDevice,
    pub dwrite: IDWriteFactory,
    pub fonts: Fonts,
}

fn format(dw: &IDWriteFactory, family: &str, size: f32, center: bool) -> Result<Format> {
    unsafe {
        let f = dw.CreateTextFormat(
            &HSTRING::from(family),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from("en-us"),
        )?;
        f.SetTextAlignment(if center { DWRITE_TEXT_ALIGNMENT_CENTER } else { DWRITE_TEXT_ALIGNMENT_LEADING })?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trim, &sign)?;
        let layout = dw.CreateTextLayout(&[b'A' as u16], &f, 100.0, 100.0)?;
        let mut m = [DWRITE_LINE_METRICS::default(); 1];
        let mut n = 0;
        let _ = layout.GetLineMetrics(Some(&mut m), &mut n);
        Ok(Format { f, baseline: m[0].baseline })
    }
}

impl Shared {
    pub fn new() -> Result<Rc<Shared>> {
        let device = Device::new()?;
        let dcomp = device.create_composition_device()?;
        let dwrite = device.text.factory.clone();
        let fonts = Fonts {
            ui12: format(&dwrite, UI_FONT, 12.0, false)?,
            ui14: format(&dwrite, UI_FONT, 14.0, false)?,
            glyph10: format(&dwrite, ICON_FONT, 10.0, true)?,
            glyph12: format(&dwrite, ICON_FONT, 12.0, true)?,
            glyph14: format(&dwrite, ICON_FONT, 14.0, true)?,
            glyph16: format(&dwrite, ICON_FONT, 16.0, true)?,
        };
        Ok(Rc::new(Shared { device, dcomp, dwrite, fonts }))
    }
}

/// One window's swap chain, drawn in DIPs (the context carries the window's DPI).
pub struct Canvas {
    pub sh: Rc<Shared>,
    pub ctx: ID2D1DeviceContext,
    swap: SwapChain,
    _target: Option<IDCompositionTarget>,
    visual: IDCompositionVisual,
    pub dpi: f32,
}

impl Canvas {
    pub fn new(sh: Rc<Shared>, hwnd: HWND, w: u32, h: u32, dpi: f32) -> Result<Canvas> {
        let mut root = Self::layer(sh, w, h, dpi)?;
        unsafe {
            let dev = &root.sh.device;
            let target = dev.check(root.sh.dcomp.CreateTargetForHwnd(hwnd, true))?;
            dev.check(target.SetRoot(&root.visual))?;
            dev.check(root.sh.dcomp.Commit())?;
            root._target = Some(target);
        }
        Ok(root)
    }

    pub fn add_layer(&self, w: u32, h: u32, dpi: f32) -> Result<Canvas> {
        let layer = Self::layer(self.sh.clone(), w, h, dpi)?;
        self.sh.device.check(unsafe { self.visual.AddVisual(&layer.visual, true, None::<&IDCompositionVisual>) })?;
        Ok(layer)
    }

    pub fn remove_layer(&self, layer: &Canvas) -> Result<()> {
        unsafe {
            self.sh.device.check(self.visual.RemoveVisual(&layer.visual))?;
            self.sh.device.check(self.sh.dcomp.Commit())
        }
    }

    pub fn set_origin(&self, x: i32, y: i32) -> Result<()> {
        unsafe {
            self.sh.device.check(self.visual.SetOffsetX2(x as f32))?;
            self.sh.device.check(self.visual.SetOffsetY2(y as f32))?;
            self.sh.device.check(self.sh.dcomp.Commit())
        }
    }

    fn layer(sh: Rc<Shared>, w: u32, h: u32, dpi: f32) -> Result<Canvas> {
        unsafe {
            let swap = sh.device.create_swap_chain(w, h, 2)?;
            let ctx = sh.device.create_context()?;
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let visual = sh.device.check(sh.dcomp.CreateVisual())?;
            swap.attach(&visual)?;
            let c = Canvas { sh, ctx, swap, _target: None, visual, dpi };
            c.bind()?;
            Ok(c)
        }
    }

    fn bind(&self) -> Result<()> {
        self.swap.bind(&self.ctx, self.dpi)
    }

    pub fn resize(&mut self, w: u32, h: u32, dpi: f32) -> Result<()> {
        unsafe { self.ctx.SetTarget(None) };
        self.swap.resize(w, h)?;
        self.dpi = dpi;
        self.bind()
    }

    pub fn begin(&self) {
        unsafe {
            self.ctx.BeginDraw();
            self.ctx.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
    }

    pub fn end(&self) -> Result<()> {
        self.sh.device.check(unsafe { self.ctx.EndDraw(None, None) })?;
        self.swap.present()
    }

    pub fn bitmap(&self, width: u32, height: u32, premul_bgra: &[u8]) -> Result<ID2D1Bitmap1> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            self.sh.device.check(self.ctx.CreateBitmap(D2D_SIZE_U { width, height }, Some(premul_bgra.as_ptr().cast()), width * 4, &props))
        }
    }

    fn brush(&self, c: Color) -> Option<ID2D1SolidColorBrush> {
        unsafe { self.ctx.CreateSolidColorBrush(&c, None).ok() }
    }

    pub fn fill(&self, r: D2D_RECT_F, c: Color) {
        if let Some(b) = self.brush(c) {
            unsafe { self.ctx.FillRectangle(&r, &b) };
        }
    }

    pub fn round(&self, r: D2D_RECT_F, radius: f32, c: Color) {
        if let Some(b) = self.brush(c) {
            let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
            unsafe { self.ctx.FillRoundedRectangle(&rr, &b) };
        }
    }

    fn layout(&self, f: &Format, s: &str, width: f32) -> Option<IDWriteTextLayout> {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe { self.sh.dwrite.CreateTextLayout(&w, &f.f, width.max(1.0), 100.0).ok() }
    }

    /// Text with its baseline at `baseline`, laid out in [x0, x1], trimmed with an ellipsis.
    pub fn text(&self, f: &Format, s: &str, x0: f32, x1: f32, baseline: f32, c: Color) {
        let (Some(l), Some(b)) = (self.layout(f, s, x1 - x0), self.brush(c)) else { return };
        unsafe {
            self.ctx.DrawTextLayout(Vector2 { X: x0, Y: baseline - f.baseline }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_CLIP)
        };
    }

    /// Width of `s` in `f`.
    pub fn measure(&self, f: &Format, s: &str) -> f32 {
        let Some(l) = self.layout(f, s, 4000.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        let _ = unsafe { l.GetMetrics(&mut m) };
        m.widthIncludingTrailingWhitespace
    }

    /// X offset of the UTF-16 position `pos` in `s`.
    pub fn caret_x(&self, f: &Format, s: &str, pos: usize) -> f32 {
        let Some(l) = self.layout(f, s, 4000.0) else { return 0.0 };
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        let _ = unsafe { l.HitTestTextPosition(pos as u32, false, &mut x, &mut y, &mut m) };
        x
    }

    /// A Segoe Fluent Icons glyph centered on (cx, cy).
    pub fn glyph(&self, f: &Format, g: char, cx: f32, cy: f32, size: f32, c: Color) {
        let top = cy - size / 2.0;
        self.text(f, &g.to_string(), cx - size, cx + size, top + f.baseline, c);
    }

    pub fn image(&self, b: &ID2D1Bitmap1, x: f32, y: f32, size: f32) {
        unsafe {
            self.ctx.DrawBitmap(b, Some(&rect(x, y, size, size)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None)
        };
    }

}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}
