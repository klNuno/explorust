//! DirectWrite formats and drawing helpers.

use windows_numerics::Vector2;
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, ID2D1Brush, ID2D1DeviceContext};
use windows::Win32::Graphics::DirectWrite::*;
use windows_core::{HSTRING, Result};

#[derive(Clone)]
pub struct Text {
    pub factory: IDWriteFactory,
}

pub enum Align {
    Leading,
    Center,
    Trailing,
}

impl Text {
    pub fn new() -> Result<Self> {
        Ok(Self { factory: unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? } })
    }

    pub fn format(&self, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT, align: Align) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.factory.CreateTextFormat(
                &HSTRING::from(family),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("fr-fr"),
            )?;
            f.SetTextAlignment(match align {
                Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        }
    }

    /// Distance from the top of a layout box to the baseline, for baseline placement.
    pub fn baseline(&self, format: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(layout) = self.factory.CreateTextLayout(&[b'A' as u16], format, 100.0, 100.0) else { return 0.0 };
            let mut m = [DWRITE_LINE_METRICS::default(); 1];
            let mut n = 0;
            let _ = layout.GetLineMetrics(Some(&mut m), &mut n);
            m[0].baseline
        }
    }
}

/// Draws `s` with its baseline at `baseline_y`, aligned in [x0, x1]; `tabular` turns on
/// fixed-width digits (the taskbar clock uses them).
pub fn draw(
    ctx: &ID2D1DeviceContext,
    text: &Text,
    format: &IDWriteTextFormat,
    s: &str,
    x0: f32,
    x1: f32,
    baseline_y: f32,
    brush: &ID2D1Brush,
    tabular: bool,
) {
    let w: Vec<u16> = s.encode_utf16().collect();
    let top = baseline_y - text.baseline(format);
    unsafe {
        let Ok(layout) = text.factory.CreateTextLayout(&w, format, x1 - x0, 100.0) else { return };
        if tabular {
            if let Ok(t) = text.factory.CreateTypography() {
                let _ = t.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 });
                let _ = layout.SetTypography(&t, DWRITE_TEXT_RANGE { startPosition: 0, length: w.len() as u32 });
            }
        }
        ctx.DrawTextLayout(Vector2 { X: x0, Y: top }, &layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
}
