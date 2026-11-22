//! Process-wide graphics device. Window targets, swap chains and drawing contexts belong to
//! their respective UI threads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::D2DERR_RECREATE_TARGET;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_core::{Error, Interface, Result};

static SHARED: Mutex<Option<Arc<Graphics>>> = Mutex::new(None);
static DEVICE_LOST: OnceLock<fn(Error)> = OnceLock::new();
static TRACE: OnceLock<fn(&str)> = OnceLock::new();

struct Graphics {
    d3d: ID3D11Device,
    dxgi: IDXGIDevice,
    dxgi_factory: IDXGIFactory2,
    factory: ID2D1Factory1,
    d2d: ID2D1Device,
    lock: ID2D1Multithread,
    lost: AtomicBool,
}

// SAFETY: The D3D11 device and DXGI factories are free-threaded and the D2D factory is explicitly
// multi-threaded. Its lock also guards our DXGI operations on the resources it uses. No drawing
// context or DirectComposition target leaves its UI thread.
unsafe impl Send for Graphics {}
unsafe impl Sync for Graphics {}

impl Graphics {
    fn new() -> Result<Self> {
        unsafe {
            let (mut d3d, mut immediate) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut immediate),
            )?;
            let d3d = d3d.ok_or_else(Error::empty)?;
            let immediate = immediate.ok_or_else(Error::empty)?;
            let _ = immediate
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let dxgi: IDXGIDevice = d3d.cast()?;
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let lock: ID2D1Multithread = factory.cast()?;
            let d2d = factory.CreateDevice(&dxgi)?;
            if let Some(log) = TRACE.get() {
                log(&format!(
                    "render: D3D11/DXGI {:p}, DXGI factory {:p}, D2D device {:p}, D2D factory {:p}",
                    d3d.as_raw(),
                    dxgi_factory.as_raw(),
                    d2d.as_raw(),
                    factory.as_raw()
                ));
            }
            Ok(Self {
                d3d,
                dxgi,
                dxgi_factory,
                factory,
                d2d,
                lock,
                lost: AtomicBool::new(false),
            })
        }
    }

    fn check<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            let reason = unsafe { self.d3d.GetDeviceRemovedReason() }.err();
            let loss =
                reason.or_else(|| (error.code() == D2DERR_RECREATE_TARGET).then(|| error.clone()));
            if let Some(loss) = loss
                && !self.lost.swap(true, Ordering::AcqRel)
                && let Some(handler) = DEVICE_LOST.get()
            {
                handler(loss);
            }
        }
        result
    }

    fn with_dxgi<T>(&self, call: impl FnOnce() -> Result<T>) -> Result<T> {
        let guard = D2dGuard(&self.lock);
        unsafe { guard.0.Enter() };
        let result = call();
        drop(guard);
        self.check(result)
    }
}

struct D2dGuard<'a>(&'a ID2D1Multithread);

impl Drop for D2dGuard<'_> {
    fn drop(&mut self) {
        unsafe { self.0.Leave() }
    }
}

/// A reference to the shared graphics device and DirectWrite's process factory.
#[derive(Clone)]
pub struct Device {
    graphics: Arc<Graphics>,
    pub text: crate::text::Text,
}

// SAFETY: only Graphics and a DirectWrite shared factory reference are carried to the UI thread.
unsafe impl Send for Device {}

impl Device {
    pub fn new() -> Result<Self> {
        let graphics = {
            let mut cell = SHARED.lock().unwrap_or_else(|e| e.into_inner());
            if cell
                .as_ref()
                .is_some_and(|g| g.lost.load(Ordering::Acquire))
            {
                if DEVICE_LOST.get().is_some() {
                    // The owner will rebuild all its windows, target bitmaps and icon caches.
                    return Err(Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED));
                }
                *cell = None;
            }
            match &*cell {
                Some(graphics) => graphics.clone(),
                None => {
                    let graphics = Arc::new(Graphics::new()?);
                    *cell = Some(graphics.clone());
                    graphics
                }
            }
        };
        Ok(Self {
            graphics,
            text: crate::text::Text::new()?,
        })
    }

    /// Report the first loss of the shared device to the shell for a coordinated reload.
    pub fn on_device_lost(handler: fn(Error)) {
        let _ = DEVICE_LOST.set(handler);
    }

    pub fn trace(log: fn(&str)) {
        let _ = TRACE.set(log);
    }

    /// Warm DirectWrite on a worker while the caller builds its window.
    pub fn spawn(warm: &'static [&'static str]) -> std::sync::mpsc::Receiver<Result<Self>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("render-device".into())
            .spawn(move || {
                let d = Self::new();
                if let Ok(d) = &d {
                    for f in warm {
                        let _ = d.text.format(
                            f,
                            14.0,
                            windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
                            crate::text::Align::Leading,
                        );
                    }
                }
                let _ = tx.send(d);
            });
        rx
    }

    pub(crate) fn factory(&self) -> ID2D1Factory1 {
        self.graphics.factory.clone()
    }

    pub fn create_context(&self) -> Result<ID2D1DeviceContext> {
        self.check(unsafe {
            self.graphics
                .d2d
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
        })
    }

    pub fn create_composition_device(&self) -> Result<IDCompositionDevice> {
        self.check(unsafe { DCompositionCreateDevice(&self.graphics.dxgi) })
    }

    pub fn create_swap_chain(&self, w: u32, h: u32, buffers: u32) -> Result<SwapChain> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w.max(1),
            Height: h.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: buffers,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let chain = self.graphics.with_dxgi(|| unsafe {
            self.graphics.dxgi_factory.CreateSwapChainForComposition(
                &self.graphics.dxgi,
                &desc,
                None,
            )
        })?;
        let swap = SwapChain {
            chain,
            graphics: self.graphics.clone(),
        };
        if let Some(log) = TRACE.get() {
            log(&format!(
                "render: swap chain {:p} created by {} {}x{} x{}",
                swap.chain.as_raw(),
                std::thread::current().name().unwrap_or("unnamed"),
                desc.Width,
                desc.Height,
                buffers
            ));
        }
        Ok(swap)
    }

    pub fn check<T>(&self, result: Result<T>) -> Result<T> {
        self.graphics.check(result)
    }
}

/// One composition swap chain with its shared graphics device.
pub struct SwapChain {
    chain: IDXGISwapChain1,
    graphics: Arc<Graphics>,
}

impl SwapChain {
    pub fn attach(&self, visual: &IDCompositionVisual) -> Result<()> {
        self.graphics
            .check(unsafe { visual.SetContent(&self.chain) })
    }

    pub fn resize(&self, w: u32, h: u32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        self.graphics.with_dxgi(|| unsafe {
            self.chain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
        })?;
        if let Some(log) = TRACE.get() {
            log(&format!(
                "render: swap chain {:p} resized to {w}x{h}",
                self.chain.as_raw()
            ));
        }
        Ok(())
    }

    pub fn bind(&self, ctx: &ID2D1DeviceContext, dpi: f32) -> Result<()> {
        unsafe {
            let buffer: IDXGISurface = self.graphics.with_dxgi(|| self.chain.GetBuffer(0))?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi,
                dpiY: dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let target = self
                .graphics
                .check(ctx.CreateBitmapFromDxgiSurface(&buffer, Some(&props)))?;
            ctx.SetTarget(&target);
            ctx.SetDpi(dpi, dpi);
        }
        Ok(())
    }

    pub fn present(&self) -> Result<()> {
        // These are composition chains without an HWND. Present cannot send messages to the UI
        // thread that owns a swap-chain HWND while the shared D2D lock is held.
        self.graphics
            .with_dxgi(|| unsafe { self.chain.Present(1, DXGI_PRESENT(0)).ok() })
    }

    pub(crate) fn check<T>(&self, result: Result<T>) -> Result<T> {
        self.graphics.check(result)
    }
}

impl Drop for SwapChain {
    fn drop(&mut self) {
        if let Some(log) = TRACE.get() {
            log(&format!(
                "render: swap chain {:p} released",
                self.chain.as_raw()
            ));
        }
    }
}
