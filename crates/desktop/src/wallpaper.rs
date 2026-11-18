//! The wallpaper as Windows sets it: the path from `SPI_GETDESKWALLPAPER`, the fit from
//! `WallpaperStyle` and `TileWallpaper`, the background color from `COLOR_BACKGROUND`. A worker
//! thread decodes and scales it with WIC into one bitmap of the virtual screen; Progman blits it.

use crate::layout::{self, Fit, Rect};
use crate::{log, reg, screen};
use std::path::PathBuf;
use std::time::Instant;
use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetSysColor, COLOR_BACKGROUND, HBITMAP, HDC, HGDIOBJ, SRCCOPY, SelectObject,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows_core::{HSTRING, Interface};

/// Posted to Progman when a frame is ready: wParam = generation, lParam = `Box<Frame>`.
pub const WM_WALLPAPER: u32 = 0x8000 + 1;

pub struct Settings {
    pub path: Option<PathBuf>,
    pub fit: Fit,
    /// COLORREF (0x00BBGGRR).
    pub color: u32,
}

pub struct Frame {
    pub area: Rect,
    /// Top-down BGRA rows of `area`.
    pub pixels: Vec<u8>,
}

pub fn settings() -> Settings {
    let mut buf = [0u16; 520];
    // SAFETY: the buffer and its length in characters.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let set = if ok { String::from_utf16_lossy(&buf[..n]) } else { String::new() };
    // Explorer draws Windows' transcoded copy, not the source file: it matches explorer's capture
    // closer (mean gray diff 4.6 against 5.8 from the source, ffmpeg scaled).
    let transcoded = std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Themes\TranscodedWallpaper"))
        .filter(|p| p.exists());
    let path = if set.is_empty() {
        None
    } else {
        transcoded.or_else(|| Some(PathBuf::from(&set)).filter(|p| p.exists()))
    };
    let num = |v: &str| reg::string(r"Control Panel\Desktop", v).and_then(|s| s.trim().parse::<u32>().ok());
    let fit = Fit::from_registry(num("WallpaperStyle").unwrap_or(10), num("TileWallpaper").unwrap_or(0));
    // SAFETY: plain query.
    let color = unsafe { GetSysColor(COLOR_BACKGROUND) };
    Settings { path, fit, color }
}

/// Renders the wallpaper off the UI thread and posts the frame to `hwnd`.
pub fn spawn(hwnd: HWND, generation: usize) {
    let target = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("wallpaper".into()).spawn(move || {
        let t = Instant::now();
        // SAFETY: COM for this worker thread only, balanced below.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let s = settings();
        let mons = screen::monitors();
        let frame = render(&s, &mons.iter().map(|m| m.rect).collect::<Vec<_>>(), screen::virtual_screen());
        log::line(&format!(
            "wallpaper {:?} {:?} color {:06x}: {}x{} in {} ms",
            s.path, s.fit, s.color, frame.area.w, frame.area.h, t.elapsed().as_millis()
        ));
        if com {
            // SAFETY: balances the successful CoInitializeEx of this thread.
            unsafe { CoUninitialize() };
        }
        let raw = Box::into_raw(Box::new(frame));
        // SAFETY: posting to a window handle; on failure the frame is reclaimed here.
        let posted = unsafe {
            PostMessageW(Some(HWND(target as _)), WM_WALLPAPER, WPARAM(generation), LPARAM(raw as isize))
        };
        if posted.is_err() {
            // SAFETY: the message was not queued, so this thread still owns the box.
            drop(unsafe { Box::from_raw(raw) });
        }
    });
}

fn bgra(colorref: u32) -> [u8; 4] {
    [(colorref >> 16) as u8, (colorref >> 8) as u8, colorref as u8, 255]
}

pub fn render(s: &Settings, monitors: &[Rect], virt: Rect) -> Frame {
    let w = virt.w.max(1);
    let h = virt.h.max(1);
    let px = bgra(s.color);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        pixels.extend_from_slice(&px);
    }
    let mut frame = Frame { area: Rect::new(virt.x, virt.y, w, h), pixels };
    if let Some(path) = &s.path {
        if let Err(e) = draw_image(&mut frame, path, s.fit, monitors) {
            log::line(&format!("wallpaper {path:?} not drawn: {e}"));
        }
    }
    frame
}

fn draw_image(frame: &mut Frame, path: &std::path::Path, fit: Fit, monitors: &[Rect]) -> windows_core::Result<()> {
    // SAFETY: WIC calls on objects this function owns; buffers are sized for each copy.
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let dec = f.CreateDecoderFromFilename(
            &HSTRING::from(path.as_os_str()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(
            &dec.GetFrame(0)?,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let src: IWICBitmapSource = conv.cast()?;
        let (mut iw, mut ih) = (0u32, 0u32);
        src.GetSize(&mut iw, &mut ih)?;
        let img = (iw as i32, ih as i32);
        for area in layout::wallpaper_areas(fit, monitors, frame.area) {
            let first = layout::place(fit, img, area);
            let mut dests = vec![first];
            if fit == Fit::Tile {
                dests.clear();
                let mut y = area.y;
                while y < area.bottom() {
                    let mut x = area.x;
                    while x < area.right() {
                        dests.push(Rect::new(x, y, img.0, img.1));
                        x += img.0;
                    }
                    y += img.1;
                }
            }
            let scaled: IWICBitmapSource = if first.w == img.0 && first.h == img.1 {
                src.clone()
            } else {
                let sc = f.CreateBitmapScaler()?;
                sc.Initialize(&src, first.w as u32, first.h as u32, WICBitmapInterpolationModeHighQualityCubic)?;
                sc.cast()?
            };
            for d in dests {
                let vis = d.intersect(&area).intersect(&frame.area);
                if vis.is_empty() {
                    continue;
                }
                let stride = frame.area.w as usize * 4;
                let start = (vis.y - frame.area.y) as usize * stride + (vis.x - frame.area.x) as usize * 4;
                let len = (vis.h as usize - 1) * stride + vis.w as usize * 4;
                let rc = WICRect { X: vis.x - d.x, Y: vis.y - d.y, Width: vis.w, Height: vis.h };
                scaled.CopyPixels(&rc, stride as u32, &mut frame.pixels[start..start + len])?;
            }
        }
        // Wallpapers are opaque: drop any alpha the file carried.
        for p in frame.pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Ok(())
}

/// The frame as a GDI bitmap in a memory DC, blitted by Progman's paint and by the icon view
/// when it asks for its parent's background.
pub struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
}

impl Surface {
    pub fn new(frame: &Frame) -> Option<Surface> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: frame.area.w,
                biHeight: -frame.area.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: a top-down 32 bpp DIB of the frame's size; the copy fits its pixel buffer.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(dc);
                return None;
            }
            std::ptr::copy_nonoverlapping(frame.pixels.as_ptr(), bits.cast::<u8>(), frame.pixels.len());
            let old = SelectObject(dc, bmp.into());
            Some(Surface { dc, bmp, old })
        }
    }

    /// Copies the client rectangle `r` (client coordinates = virtual screen minus `area` origin).
    pub fn blit(&self, hdc: HDC, r: Rect) {
        // SAFETY: both DCs are valid; GDI clips to the bitmap.
        let _ = unsafe { BitBlt(hdc, r.x, r.y, r.w, r.h, Some(self.dc), r.x, r.y, SRCCOPY) };
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: restores the DC's bitmap, then frees what `new` created.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
