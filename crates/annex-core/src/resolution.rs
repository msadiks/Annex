//! Resolution rule (spec 3.1).

/// Streams are never taller than this.
pub const MAX_STREAM_H: u32 = 1080;
/// Sharp packs a W×H picture into 2W×2H; HEVC Level 5.1 tops out at 3840 wide.
pub const SHARP_MAX_W: u32 = 1920;

pub fn round_to_even(v: f64) -> u32 {
    let r = (v / 2.0).round() * 2.0;
    r.max(2.0) as u32
}

/// Stream size for a panel running at `native_w × native_h`.
///
/// ```
/// use annex_core::stream_size;
/// assert_eq!(stream_size(3440, 1440), (2580, 1080));
/// ```
pub fn stream_size(native_w: u32, native_h: u32) -> (u32, u32) {
    let native_w = native_w.max(2);
    let native_h = native_h.max(2);
    let h = native_h.min(MAX_STREAM_H);
    let w = round_to_even(native_w as f64 * h as f64 / native_h as f64);
    (w, round_to_even(h as f64))
}

pub fn sharp_eligible(stream_w: u32) -> bool {
    stream_w <= SHARP_MAX_W
}

/// Mirror mode: the largest even size with the source's aspect that fits in
/// the stream box of the target panel.
pub fn fit_within(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let (sw, sh) = (src_w.max(1) as f64, src_h.max(1) as f64);
    let scale = (box_w as f64 / sw).min(box_h as f64 / sh);
    let w = round_to_even(sw * scale).min(box_w - box_w % 2);
    let h = round_to_even(sh * scale).min(box_h - box_h % 2);
    (w.max(2), h.max(2))
}

/// Destination rectangle `(x, y, w, h)` that shows a `src` picture on a `dst`
/// panel with aspect preserved and black bars. Aspect differences under 1 %
/// (rounding to even sizes) fill the panel.
pub fn letterbox(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> (u32, u32, u32, u32) {
    let sa = src_w as f64 / src_h.max(1) as f64;
    let da = dst_w as f64 / dst_h.max(1) as f64;
    if (sa / da - 1.0).abs() < 0.01 {
        return (0, 0, dst_w, dst_h);
    }
    if sa > da {
        let h = (dst_w as f64 / sa).round() as u32;
        (0, (dst_h - h) / 2, dst_w, h)
    } else {
        let w = (dst_h as f64 * sa).round() as u32;
        ((dst_w - w) / 2, 0, w, dst_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        assert_eq!(stream_size(1920, 1080), (1920, 1080));
        assert_eq!(stream_size(1366, 768), (1366, 768));
        assert_eq!(stream_size(2560, 1440), (1920, 1080));
        assert_eq!(stream_size(3440, 1440), (2580, 1080));
        assert_eq!(stream_size(5120, 1440), (3840, 1080));
        assert_eq!(stream_size(1920, 1200), (1728, 1080));
        assert!(sharp_eligible(1920));
        assert!(!sharp_eligible(2580));
    }

    #[test]
    fn mirror_fit_and_letterbox() {
        // 16:10 MacBook onto a 16:9 panel → pillarboxed.
        assert_eq!(fit_within(3024, 1890, 1920, 1080), (1728, 1080));
        assert_eq!(letterbox(1728, 1080, 2560, 1440), (128, 0, 2304, 1440));
        assert_eq!(letterbox(1920, 1080, 3840, 2160), (0, 0, 3840, 2160));
        assert_eq!(letterbox(2580, 1080, 3440, 1440), (0, 0, 3440, 1440));
    }
}
