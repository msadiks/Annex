//! CPU reference of the colour packing (spec 4.1, 4.2, 6.3).
//!
//! The Mac does this in a Metal kernel and the PC undoes it in a pixel shader;
//! this module is the readable definition both are checked against, and the
//! test sender uses it to build test streams.
//!
//! BT.709, full range, 8-bit:
//! `Y = 0.2126 R + 0.7152 G + 0.0722 B`, `Cb = (B − Y)/1.8556 + 0.5`,
//! `Cr = (R − Y)/1.5748 + 0.5`.

pub fn rgb_to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (y, (b - y) / 1.8556 + 0.5, (r - y) / 1.5748 + 0.5)
}

pub fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    let cb = cb - 0.5;
    let cr = cr - 0.5;
    let r = y + 1.5748 * cr;
    let b = y + 1.8556 * cb;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    (r, g, b)
}

fn q(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn px(bgra: &[u8], stride: usize, x: usize, y: usize) -> (f32, f32, f32) {
    let i = y * stride + x * 4;
    (bgra[i + 2] as f32 / 255.0, bgra[i + 1] as f32 / 255.0, bgra[i] as f32 / 255.0)
}

/// An NV12 picture: full-resolution luma and interleaved half-resolution CbCr.
#[derive(Debug, Clone)]
pub struct Nv12 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

impl Nv12 {
    /// Raw `yuv420p`-compatible NV12 bytes (Y plane then UV plane).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = self.y.clone();
        v.extend_from_slice(&self.uv);
        v
    }
}

/// Fast: `W×H` NV12, chroma = mean of each 2×2 block.
pub fn pack_fast(bgra: &[u8], w: usize, h: usize, stride: usize) -> Nv12 {
    let mut y = vec![0u8; w * h];
    let mut uv = vec![0u8; w * h / 2];
    for j in 0..h {
        for i in 0..w {
            let (r, g, b) = px(bgra, stride, i, j);
            y[j * w + i] = q(rgb_to_ycbcr(r, g, b).0);
        }
    }
    for j in 0..h / 2 {
        for i in 0..w / 2 {
            let (mut cb, mut cr) = (0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (r, g, b) = px(bgra, stride, 2 * i + dx, 2 * j + dy);
                let (_, u, v) = rgb_to_ycbcr(r, g, b);
                cb += u;
                cr += v;
            }
            uv[j * w + 2 * i] = q(cb / 4.0);
            uv[j * w + 2 * i + 1] = q(cr / 4.0);
        }
    }
    Nv12 { width: w, height: h, y, uv }
}

/// Sharp: `2W×2H` NV12 where each source luma fills a 2×2 block and the
/// chroma plane (`W×H` pairs) carries every source pixel's chroma.
pub fn pack_sharp(bgra: &[u8], w: usize, h: usize, stride: usize) -> Nv12 {
    let ow = 2 * w;
    let mut y = vec![0u8; ow * 2 * h];
    let mut uv = vec![0u8; w * h * 2];
    for j in 0..h {
        for i in 0..w {
            let (r, g, b) = px(bgra, stride, i, j);
            let (yy, cb, cr) = rgb_to_ycbcr(r, g, b);
            let yq = q(yy);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                y[(2 * j + dy) * ow + 2 * i + dx] = yq;
            }
            uv[j * ow + 2 * i] = q(cb);
            uv[j * ow + 2 * i + 1] = q(cr);
        }
    }
    Nv12 { width: ow, height: 2 * h, y, uv }
}

/// Receiver recombine for Sharp, per output pixel: mean of the 2×2 luma
/// block and the point-sampled chroma. Returns BGRA.
pub fn unpack_sharp(nv: &Nv12) -> Vec<u8> {
    let w = nv.width / 2;
    let h = nv.height / 2;
    let mut out = vec![0u8; w * h * 4];
    for j in 0..h {
        for i in 0..w {
            let mut ys = 0.0;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                ys += nv.y[(2 * j + dy) * nv.width + 2 * i + dx] as f32;
            }
            let yy = ys / 4.0 / 255.0;
            let cb = nv.uv[j * nv.width + 2 * i] as f32 / 255.0;
            let cr = nv.uv[j * nv.width + 2 * i + 1] as f32 / 255.0;
            let (r, g, b) = ycbcr_to_rgb(yy, cb, cr);
            let o = (j * w + i) * 4;
            out[o] = q(b);
            out[o + 1] = q(g);
            out[o + 2] = q(r);
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharp_roundtrip_is_near_exact() {
        // Coloured text-like pattern: worst case for 4:2:0.
        let (w, h) = (16, 8);
        let mut img = vec![0u8; w * h * 4];
        for j in 0..h {
            for i in 0..w {
                let o = (j * w + i) * 4;
                let on = (i + j) % 2 == 0;
                img[o] = if on { 255 } else { 0 };
                img[o + 1] = (i * 16) as u8;
                img[o + 2] = if on { 0 } else { 255 };
                img[o + 3] = 255;
            }
        }
        let back = unpack_sharp(&pack_sharp(&img, w, h, w * 4));
        let max_err = img.iter().zip(&back).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(max_err <= 2, "8-bit YCbCr round trip error {max_err}");
    }
}
