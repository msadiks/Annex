//! Stats overlay text, rasterized on the CPU with an 8×8 bitmap font into a
//! premultiplied BGRA image (rebuilt once per second, uploaded as a texture).

use font8x8::legacy::BASIC_LEGACY;

/// Returns `(width, height, bgra)`.
pub fn render(lines: &[String], scale: u32) -> (u32, u32, Vec<u8>) {
    let pad = 6 * scale;
    let cw = 8 * scale;
    let lh = 10 * scale;
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as u32;
    let w = cols * cw + 2 * pad;
    let h = lines.len().max(1) as u32 * lh + 2 * pad;
    let mut px = vec![0u8; (w * h * 4) as usize];
    // Translucent black panel (premultiplied).
    for p in px.chunks_exact_mut(4) {
        p[3] = 170;
    }
    for (li, line) in lines.iter().enumerate() {
        for (ci, ch) in line.chars().enumerate() {
            let glyph = BASIC_LEGACY[(ch as usize) & 0x7f];
            for (gy, row) in glyph.iter().enumerate() {
                for gx in 0..8 {
                    if row & (1 << gx) == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let x = pad + ci as u32 * cw + gx * scale + sx;
                            let y = pad + li as u32 * lh + gy as u32 * scale + sy;
                            let o = ((y * w + x) * 4) as usize;
                            px[o..o + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
    (w, h, px)
}
