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

impl Surface {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
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
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let device = factory.CreateDevice(&dxgi)?;
            let ctx = device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
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
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
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

    pub fn brush(&self, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.ctx.CreateSolidColorBrush(&c, None) }
    }
}
