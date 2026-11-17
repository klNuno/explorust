//! Real shell data for the maquettes: pinned and running taskbar apps with their icons,
//! the system clock, and in-process bench counters. Windows only.

pub mod apps;
pub mod bench;
pub mod clock;
pub mod icons;
pub mod layout;

/// A 32-bit BGRA image with straight (non-premultiplied) alpha, rows top-down.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Image {
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.bgra.clone();
        for px in out.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        out
    }

    pub fn to_premultiplied_bgra(&self) -> Vec<u8> {
        let mut out = self.bgra.clone();
        for px in out.chunks_exact_mut(4) {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        out
    }
}

/// Wide string with a trailing NUL for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
