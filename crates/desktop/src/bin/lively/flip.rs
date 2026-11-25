use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
};
use windows_core::{Error, HRESULT, Interface, Result};

pub struct Renderer {
    chain: IDXGISwapChain1,
    context: ID3D11DeviceContext,
    target: ID3D11RenderTargetView,
    color: [f32; 4],
    _composition: Option<(IDCompositionDevice, IDCompositionTarget)>,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32, rgb: u32, composition: bool) -> Result<Self> {
        // SAFETY: hwnd belongs to the caller's thread and remains valid for the renderer.
        unsafe {
            let (mut device, mut context) = (None, None);
            let mut create = |driver| {
                D3D11CreateDevice(
                    None,
                    driver,
                    Default::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            create(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| create(D3D_DRIVER_TYPE_WARP))?;
            let device: ID3D11Device = device.ok_or_else(Error::empty)?;
            let context = context.ok_or_else(Error::empty)?;
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let (chain, composition) = if composition {
                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    ..desc
                };
                let dxgi: IDXGIDevice = device.cast()?;
                let chain = factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;
                let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
                let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
                let visual = dcomp.CreateVisual()?;
                visual.SetContent(&chain)?;
                target.SetRoot(&visual)?;
                dcomp.Commit()?;
                (chain, Some((dcomp, target)))
            } else {
                let chain = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
                (chain, None)
            };
            let buffer: ID3D11Texture2D = chain.GetBuffer(0)?;
            let mut target = None;
            device.CreateRenderTargetView(&buffer, None, Some(&mut target))?;
            let target = target.ok_or_else(Error::empty)?;
            let channel = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
            let color = [channel(16), channel(8), channel(0), 1.0];
            Ok(Self {
                chain,
                context,
                target,
                color,
                _composition: composition,
            })
        }
    }

    pub fn present(&self) -> Result<HRESULT> {
        // SAFETY: the render target belongs to this context and flip chain.
        unsafe {
            self.context
                .ClearRenderTargetView(&self.target, &self.color);
            let status = self.chain.Present(1, DXGI_PRESENT(0));
            status.ok()?;
            Ok(status)
        }
    }
}
