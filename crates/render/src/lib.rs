//! Rendering layer for explorust maquettes: Direct2D 1.1 on a composition swap chain hosted
//! by DirectComposition, DirectWrite text, and the DWM backdrop under it.

pub mod backdrop;
pub mod surface;
pub mod text;

pub use surface::Surface;
pub use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F as Color;

/// sRGB color from 0..255 channels and an alpha in 0..1.
pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}
