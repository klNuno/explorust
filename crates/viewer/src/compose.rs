//! Pixel composition for animated GIF frames (premultiplied BGRA), pure so it is tested here.

/// Draws `src` (sw x sh) over `dst` (dw x dh) at (x, y), source-over, clipped.
#[allow(clippy::too_many_arguments)]
pub fn blend(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32, x: u32, y: u32) {
    for row in 0..sh {
        let ty = y + row;
        if ty >= dh {
            break;
        }
        for col in 0..sw {
            let tx = x + col;
            if tx >= dw {
                break;
            }
            let s = ((row * sw + col) * 4) as usize;
            let d = ((ty * dw + tx) * 4) as usize;
            let a = src[s + 3] as u32;
            if a == 255 {
                dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
            } else if a > 0 {
                for c in 0..4 {
                    dst[d + c] = (src[s + c] as u32 + dst[d + c] as u32 * (255 - a) / 255) as u8;
                }
            }
        }
    }
}

/// Clears a rectangle to transparent (GIF disposal 2).
pub fn clear(dst: &mut [u8], dw: u32, dh: u32, x: u32, y: u32, w: u32, h: u32) {
    for ty in y..(y + h).min(dh) {
        for tx in x..(x + w).min(dw) {
            let d = ((ty * dw + tx) * 4) as usize;
            dst[d..d + 4].fill(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_clips_and_keeps_transparent_pixels() {
        let mut dst = vec![9u8; 2 * 2 * 4];
        let src = [255, 0, 0, 255, 0, 0, 0, 0];
        blend(&mut dst, 2, 2, &src, 2, 1, 1, 1);
        assert_eq!(&dst[12..16], &[255, 0, 0, 255]);
        assert_eq!(&dst[0..4], &[9, 9, 9, 9]);
        clear(&mut dst, 2, 2, 1, 1, 5, 5);
        assert_eq!(&dst[12..16], &[0, 0, 0, 0]);
    }
}
