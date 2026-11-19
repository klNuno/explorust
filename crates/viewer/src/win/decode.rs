//! WIC decoding: a first frame scaled to the window (JPEG decodes at 1/2, 1/4 or 1/8 through the
//! codec's own scaling), the full image after, and GIF frames composed with their delays.

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::{VT_UI1, VT_UI2, VT_UI4};
use windows_core::{GUID, HSTRING, Result};

pub struct Image {
    pub w: u32,
    pub h: u32,
    /// Premultiplied BGRA, `w * 4` bytes a row.
    pub px: Vec<u8>,
}

pub struct Decoded {
    /// Size of the full image, so a scaled first frame is drawn at the right size.
    pub full_w: u32,
    pub full_h: u32,
    /// Quarter turns clockwise from the EXIF orientation.
    pub turns: u8,
    /// One image, or the composed frames of an animation with their delays in ms.
    pub frames: Vec<(Image, u32)>,
}

pub fn factory() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

fn pv_u32(pv: &PROPVARIANT) -> Option<u32> {
    unsafe {
        let inner = &pv.Anonymous.Anonymous;
        match inner.vt {
            VT_UI1 => Some(inner.Anonymous.bVal as u32),
            VT_UI2 => Some(inner.Anonymous.uiVal as u32),
            VT_UI4 => Some(inner.Anonymous.ulVal),
            _ => None,
        }
    }
}

fn meta(reader: &IWICMetadataQueryReader, name: &str) -> Option<u32> {
    unsafe {
        let mut pv = PROPVARIANT::default();
        reader.GetMetadataByName(&HSTRING::from(name), &mut pv).ok()?;
        let v = pv_u32(&pv);
        let _ = PropVariantClear(&mut pv);
        v
    }
}

fn pixels(f: &IWICImagingFactory, src: &IWICBitmapSource) -> Result<Image> {
    unsafe {
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let (mut w, mut h) = (0, 0);
        conv.GetSize(&mut w, &mut h)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px)?;
        Ok(Image { w, h, px })
    }
}

fn orientation(frame: &IWICBitmapFrameDecode, container: GUID) -> u8 {
    let Ok(r) = (unsafe { frame.GetMetadataQueryReader() }) else { return 0 };
    let key = if container == GUID_ContainerFormatJpeg { "/app1/ifd/{ushort=274}" } else { "/ifd/{ushort=274}" };
    match meta(&r, key) {
        Some(3) => 2,
        Some(6) => 1,
        Some(8) => 3,
        _ => 0,
    }
}

/// Decodes `path`; with `fit` = Some((w, h)) the first frame is scaled down to fit that box
/// (never up), which for a 12 MP JPEG skips most of the work.
pub fn decode(f: &IWICImagingFactory, path: &str, fit: Option<(u32, u32)>) -> Result<Decoded> {
    unsafe {
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let container = dec.GetContainerFormat().unwrap_or_default();
        let count = dec.GetFrameCount()?;
        let frame = dec.GetFrame(0)?;
        let (mut w, mut h) = (0, 0);
        frame.GetSize(&mut w, &mut h)?;
        let turns = orientation(&frame, container);
        if container == GUID_ContainerFormatGif && count > 1 {
            return gif(f, &dec, count, w, h).map(|(fw, fh, frames)| Decoded { full_w: fw, full_h: fh, turns: 0, frames });
        }
        let src: IWICBitmapSource = match fit {
            Some((bw, bh)) if w > bw || h > bh => {
                let (bw, bh) = if turns % 2 == 1 { (bh, bw) } else { (bw, bh) };
                let k = (bw as f64 / w as f64).min(bh as f64 / h as f64);
                let (sw, sh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
                let scaler = f.CreateBitmapScaler()?;
                scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant)?;
                windows_core::Interface::cast(&scaler)?
            }
            _ => windows_core::Interface::cast(&frame)?,
        };
        let img = pixels(f, &src)?;
        Ok(Decoded { full_w: w, full_h: h, turns, frames: vec![(img, 0)] })
    }
}

/// Composes GIF frames on the logical screen with their offsets and disposal.
fn gif(f: &IWICImagingFactory, dec: &IWICBitmapDecoder, count: u32, w0: u32, h0: u32) -> Result<(u32, u32, Vec<(Image, u32)>)> {
    unsafe {
        let (mut sw, mut sh) = (w0, h0);
        if let Ok(r) = dec.GetMetadataQueryReader() {
            sw = meta(&r, "/logscrdesc/Width").unwrap_or(w0).max(1);
            sh = meta(&r, "/logscrdesc/Height").unwrap_or(h0).max(1);
        }
        let mut canvas = vec![0u8; (sw * sh * 4) as usize];
        let mut out = Vec::with_capacity(count.min(512) as usize);
        for i in 0..count.min(512) {
            let frame = dec.GetFrame(i)?;
            let r = frame.GetMetadataQueryReader()?;
            let left = meta(&r, "/imgdesc/Left").unwrap_or(0);
            let top = meta(&r, "/imgdesc/Top").unwrap_or(0);
            let delay = meta(&r, "/grctlext/Delay").unwrap_or(10).max(2) * 10;
            let disposal = meta(&r, "/grctlext/Disposal").unwrap_or(0);
            let img = pixels(f, &windows_core::Interface::cast(&frame)?)?;
            let saved = (disposal == 3).then(|| canvas.clone());
            crate::compose::blend(&mut canvas, sw, sh, &img.px, img.w, img.h, left, top);
            out.push((Image { w: sw, h: sh, px: canvas.clone() }, delay));
            match disposal {
                2 => crate::compose::clear(&mut canvas, sw, sh, left, top, img.w, img.h),
                3 => canvas = saved.unwrap_or(canvas),
                _ => {}
            }
        }
        Ok((sw, sh, out))
    }
}

/// True when Windows has a decoder for the file (HEIC and AVIF need the Store extensions).
pub fn can_open(f: &IWICImagingFactory, path: &str) -> bool {
    unsafe { f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).is_ok() }
}
