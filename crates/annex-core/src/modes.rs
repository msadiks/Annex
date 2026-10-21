//! Sharp/Fast assignment under an encoder budget (spec 4.3).

use annex_proto::control::VideoMode;

use crate::resolution::sharp_eligible;

/// Default bitrates at 1080p (spec 4.4).
pub const SHARP_KBPS_1080P: u32 = 80_000;
pub const FAST_KBPS_1080P: u32 = 40_000;
pub const MIN_KBPS: u32 = 15_000;
pub const MAX_KBPS: u32 = 150_000;

/// Encoder throughput available for all streams, in encoded megapixels per
/// second. Sharp costs 4× its picture size (it encodes 2W×2H).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncoderBudget {
    pub mpx_per_sec: f64,
}

impl EncoderBudget {
    /// Conservative default for one Apple media engine. Phase 1 measurements
    /// on a base M1 refine this; the sender also downgrades at runtime when
    /// measured encode times say the budget was optimistic.
    pub const fn single_engine() -> Self {
        EncoderBudget { mpx_per_sec: 750.0 }
    }

    /// Scales the default by the number of video encode engines in the chip
    /// (base/Pro = 1, Max = 2, Ultra = 4).
    pub fn for_chip(chip_name: &str) -> Self {
        let engines = if chip_name.contains("Ultra") {
            4.0
        } else if chip_name.contains("Max") {
            2.0
        } else {
            1.0
        };
        EncoderBudget { mpx_per_sec: Self::single_engine().mpx_per_sec * engines }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorDemand {
    pub monitor_id: u64,
    pub stream_w: u32,
    pub stream_h: u32,
    /// Lower is more important.
    pub priority: u8,
    /// User pin from either menu.
    pub pinned: Option<VideoMode>,
    /// Runtime measurement says Sharp can't be sustained on this stream.
    pub force_fast: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModeAssignment {
    pub monitor_id: u64,
    pub mode: VideoMode,
    pub bitrate_kbps: u32,
    /// Set when a pinned Sharp can't be honoured or sustained.
    pub warning: Option<String>,
    /// Lowered from Sharp by the budget.
    pub downgraded: bool,
}

pub fn cost_mpx(w: u32, h: u32, mode: VideoMode) -> f64 {
    let px = w as f64 * h as f64 * 60.0 / 1e6;
    match mode {
        VideoMode::Sharp => px * 4.0,
        VideoMode::Fast => px,
    }
}

/// Default bitrate for a stream, scaled by its pixel count relative to 1080p.
pub fn default_bitrate_kbps(w: u32, h: u32, mode: VideoMode) -> u32 {
    let base = match mode {
        VideoMode::Sharp => SHARP_KBPS_1080P,
        VideoMode::Fast => FAST_KBPS_1080P,
    } as f64;
    let scale = (w as f64 * h as f64 / (1920.0 * 1080.0)).clamp(0.5, 2.0);
    ((base * scale) as u32).clamp(MIN_KBPS, MAX_KBPS)
}

/// Default: the most important display is Sharp (if eligible), others Fast.
/// Pins are honoured. If the total exceeds the budget, the lowest-priority
/// unpinned Sharp streams drop to Fast; pinned Sharp streams that still don't
/// fit keep Sharp and carry a warning instead of silently dropping frames.
pub fn assign_modes(monitors: &[MonitorDemand], budget: &EncoderBudget) -> Vec<ModeAssignment> {
    let mut order: Vec<&MonitorDemand> = monitors.iter().collect();
    order.sort_by_key(|m| (m.priority, m.monitor_id));

    let mut out: Vec<ModeAssignment> = Vec::with_capacity(order.len());
    let mut first_auto_done = false;
    for m in &order {
        let eligible = sharp_eligible(m.stream_w);
        let (mode, warning) = match m.pinned {
            Some(VideoMode::Fast) => (VideoMode::Fast, None),
            Some(VideoMode::Sharp) if !eligible => (
                VideoMode::Fast,
                Some(format!("Sharp needs a stream width of 1920 or less (this display is {})", m.stream_w)),
            ),
            Some(VideoMode::Sharp) if m.force_fast => (
                VideoMode::Sharp,
                Some("The encoder can't keep Sharp at 60 fps on this display".to_string()),
            ),
            Some(VideoMode::Sharp) => (VideoMode::Sharp, None),
            None => {
                let sharp = !first_auto_done && eligible && !m.force_fast;
                first_auto_done = true;
                (if sharp { VideoMode::Sharp } else { VideoMode::Fast }, None)
            }
        };
        out.push(ModeAssignment {
            monitor_id: m.monitor_id,
            mode,
            bitrate_kbps: 0,
            warning,
            downgraded: m.pinned.is_none() && m.force_fast && eligible,
        });
    }

    let total = |out: &[ModeAssignment]| -> f64 {
        out.iter()
            .zip(&order)
            .map(|(a, m)| cost_mpx(m.stream_w, m.stream_h, a.mode))
            .sum()
    };
    // Downgrade unpinned Sharp streams from the least important up.
    for i in (0..out.len()).rev() {
        if total(&out) <= budget.mpx_per_sec {
            break;
        }
        if out[i].mode == VideoMode::Sharp && order[i].pinned.is_none() {
            out[i].mode = VideoMode::Fast;
            out[i].downgraded = true;
        }
    }
    if total(&out) > budget.mpx_per_sec {
        for (a, m) in out.iter_mut().zip(&order) {
            if a.mode == VideoMode::Sharp && m.pinned == Some(VideoMode::Sharp) && a.warning.is_none() {
                a.warning = Some("Encoder budget exceeded: pinned Sharp may not hold 60 fps".to_string());
            }
        }
    }
    for (a, m) in out.iter_mut().zip(&order) {
        a.bitrate_kbps = default_bitrate_kbps(m.stream_w, m.stream_h, a.mode);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: u64, w: u32, prio: u8, pinned: Option<VideoMode>) -> MonitorDemand {
        MonitorDemand { monitor_id: id, stream_w: w, stream_h: 1080, priority: prio, pinned, force_fast: false }
    }

    #[test]
    fn first_display_sharp_others_fast() {
        let a = assign_modes(&[mon(2, 1920, 1, None), mon(1, 1920, 0, None), mon(3, 1920, 2, None)], &EncoderBudget::single_engine());
        assert_eq!(a[0].monitor_id, 1);
        assert_eq!(a[0].mode, VideoMode::Sharp);
        assert_eq!(a[1].mode, VideoMode::Fast);
        assert_eq!(a[2].mode, VideoMode::Fast);
        assert_eq!(a[0].bitrate_kbps, 80_000);
        assert_eq!(a[1].bitrate_kbps, 40_000);
    }

    #[test]
    fn budget_downgrades_and_pins_warn() {
        let tiny = EncoderBudget { mpx_per_sec: 200.0 };
        let a = assign_modes(&[mon(1, 1920, 0, None)], &tiny);
        assert_eq!(a[0].mode, VideoMode::Fast);
        assert!(a[0].downgraded);
        let a = assign_modes(&[mon(1, 1920, 0, Some(VideoMode::Sharp))], &tiny);
        assert_eq!(a[0].mode, VideoMode::Sharp);
        assert!(a[0].warning.is_some());
        // Wide displays can't be Sharp at all.
        let a = assign_modes(&[mon(1, 2580, 0, Some(VideoMode::Sharp))], &EncoderBudget::single_engine());
        assert_eq!(a[0].mode, VideoMode::Fast);
        assert!(a[0].warning.is_some());
    }
}
