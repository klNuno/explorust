//! A Direct2D device context drawing into a premultiplied-alpha composition swap chain,
//! shown through a DirectComposition visual so the DWM backdrop shows through.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_core::{Interface, Result};

pub struct Surface {
    pub ctx: ID2D1DeviceContext,
    pub factory: ID2D1Factory1,
    swap: IDXGISwapChain1,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

/// The device half of a `Surface`: D3D11 device, DXGI and D2D factories, D2D device and the
/// DirectWrite factory with its system font collection loaded. Made on a worker thread while
/// the UI thread creates its window (the viewer's and taskman's first frame), then handed to
/// `Surface::on`. Nothing in it is used by two threads at once: the worker drops it after the
/// send.
pub struct Device {
    d3d: ID3D11Device,
    dxgi: IDXGIDevice,
    dxgi_factory: IDXGIFactory2,
    factory: ID2D1Factory1,
    d2d: ID2D1Device,
    pub text: crate::text::Text,
}

// SAFETY: the D3D11 device is free threaded, the DWrite shared factory is thread safe, and the
// single-threaded D2D factory and device are only used by one thread at a time (made on the
// worker, then used by the UI thread alone).
unsafe impl Send for Device {}

impl Device {
    pub fn new() -> Result<Self> {
        unsafe {
            let mut d3d = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let d3d = d3d.unwrap();
            let dxgi: IDXGIDevice = d3d.cast()?;
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let text = crate::text::Text::new()?;
            Ok(Self { d3d, dxgi, dxgi_factory, factory, d2d, text })
        }
    }

    /// `Device::new` on its own thread; `warm` font families get a format made there so the
    /// UI thread's first `CreateTextFormat` finds the font collection loaded.
    pub fn spawn(warm: &'static [&'static str]) -> std::sync::mpsc::Receiver<Result<Self>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new().name("render-device".into()).spawn(move || {
            let d = Self::new();
            if let Ok(d) = &d {
                for f in warm {
                    let _ = d.text.format(f, 14.0, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL, crate::text::Align::Leading);
                }
            }
            let _ = tx.send(d);
        });
        rx
    }

    pub fn d3d(&self) -> &ID3D11Device {
        &self.d3d
    }
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
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: buffers,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = dev.dxgi_factory.CreateSwapChainForComposition(&dev.dxgi, &desc, None)?;
            let factory = dev.factory.clone();
            let ctx = dev.d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let buffer: IDXGISurface = swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let target = ctx.CreateBitmapFromDxgiSurface(&buffer, Some(&props))?;
            ctx.SetTarget(&target);
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dev.dxgi)?;
            let dtarget = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            dtarget.SetRoot(&visual)?;
            dcomp.Commit()?;
            Ok(Self { ctx, factory, swap, _dcomp: dcomp, _target: dtarget, _visual: visual })
        }
    }

    /// Resizes the swap chain's buffers and retargets the device context to the new back buffer.
    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.ctx.SetTarget(None);
            self.swap.ResizeBuffers(0, width.max(1), height.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            let buffer: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let target = self.ctx.CreateBitmapFromDxgiSurface(&buffer, Some(&props))?;
            self.ctx.SetTarget(&target);
            Ok(())
        }
    }

    pub fn begin(&self) {
        unsafe {
            self.ctx.BeginDraw();
            self.ctx.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
    }

    pub fn end(&self) -> Result<()> {
        unsafe {
            self.ctx.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
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
            self.ctx.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(premul_bgra.as_ptr().cast()),
                width * 4,
                &props,
            )
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
        unsafe { self.ctx.CreateSolidColorBrush(&c, None) }
    }
}
