//! A Direct2D device context drawing into a premultiplied-alpha composition swap chain,
//! shown through a DirectComposition visual so the DWM backdrop shows through.

pub use crate::device::Device;
use crate::device::SwapChain;
use std::cell::Cell;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_core::Result;

pub struct Surface {
    pub ctx: ID2D1DeviceContext,
    pub factory: ID2D1Factory1,
    swap: SwapChain,
    size: Cell<(u32, u32)>,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

impl Surface {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        Self::with_buffers(hwnd, width, height, 2)
    }

    /// As `new` with `buffers` back buffers. Three let a window present twice in one refresh
    /// without its next draw waiting for the vblank that releases the buffer on screen (Start's
    /// search: a key right after another).
    pub fn with_buffers(hwnd: HWND, width: u32, height: u32, buffers: u32) -> Result<Self> {
        Self::on(&Device::new()?, hwnd, width, height, buffers)
    }

    /// The swap chain, device context and composition visual of `hwnd` on a device made
    /// before (`Device::spawn`).
    pub fn on(dev: &Device, hwnd: HWND, width: u32, height: u32, buffers: u32) -> Result<Self> {
        unsafe {
            let swap = dev.create_swap_chain(width, height, buffers)?;
            let factory = dev.factory();
            let ctx = dev.create_context()?;
            swap.bind(&ctx, 96.0)?;
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dcomp = dev.create_composition_device()?;
            let dtarget = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            swap.attach(&visual)?;
            dtarget.SetRoot(&visual)?;
            dcomp.Commit()?;
            Ok(Self { ctx, factory, swap, size: Cell::new((width.max(1), height.max(1))), _dcomp: dcomp, _target: dtarget, _visual: visual })
        }
    }

    /// Resizes the swap chain's buffers and retargets the device context to the new back buffer.
    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        let size = (width.max(1), height.max(1));
        if self.size.get() == size {
            return Ok(());
        }
        unsafe { self.ctx.SetTarget(None) };
        self.size.set((0, 0));
        let resized = self.swap.resize(size.0, size.1);
        // After a failed resize, rebind the old buffer so the previous size can still draw.
        let bound = self.swap.bind(&self.ctx, 96.0);
        resized.and(bound)?;
        self.size.set(size);
        Ok(())
    }

    pub fn begin(&self) {
        unsafe {
            self.ctx.BeginDraw();
            self.ctx.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
    }

    pub fn end(&self) -> Result<()> {
        self.swap.check(unsafe { self.ctx.EndDraw(None, None) })?;
        self.swap.present()
    }

    /// A D2D bitmap from premultiplied BGRA pixels.
    pub fn bitmap(&self, width: u32, height: u32, premul_bgra: &[u8]) -> Result<ID2D1Bitmap1> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            self.swap.check(self.ctx.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(premul_bgra.as_ptr().cast()),
                width * 4,
                &props,
            ))
        }
    }

    /// Open animation of the content: slides in from `from_y` pixels with a cubic ease-out over
    /// `slide_s` seconds and fades in over `fade_s`, run by DirectComposition (no timers).
    pub fn slide_in(&self, from_y: f32, slide_s: f32, fade_s: f32) -> Result<()> {
        unsafe {
            let t = slide_s.max(0.001);
            // y(t) = from_y * (1 - t/T)^3 expanded into cubic coefficients.
            let slide = self._dcomp.CreateAnimation()?;
            slide.AddCubic(0.0, from_y, -3.0 * from_y / t, 3.0 * from_y / (t * t), -from_y / (t * t * t))?;
            slide.End(t as f64, 0.0)?;
            self._visual.SetOffsetY(&slide)?;
            let f = fade_s.max(0.001);
            let fade = self._dcomp.CreateAnimation()?;
            fade.AddCubic(0.0, 0.0, 1.0 / f, 0.0, 0.0)?;
            fade.End(f as f64, 1.0)?;
            let group = self._dcomp.CreateEffectGroup()?;
            group.SetOpacity(&fade)?;
            self._visual.SetEffect(&group)?;
            self._dcomp.Commit()
        }
    }

    pub fn brush(&self, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        self.swap.check(unsafe { self.ctx.CreateSolidColorBrush(&c, None) })
    }
}
