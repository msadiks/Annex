//! Adaptive bitrate and FEC (spec 8.6).
//!
//! Driven by the receiver's once-per-second loss report:
//! - loss > 1 % for 2 consecutive seconds → bitrate × 0.8 and more FEC;
//! - 10 clean seconds → bitrate × 1.1 (up to the cap) and FEC back toward default.
//! The sender applies changes at the next frame boundary.

use crate::modes::MIN_KBPS;

pub const DEFAULT_FEC: u8 = 10;
pub const MAX_ADAPTIVE_FEC: u8 = 30;
const LOSS_BAD: f64 = 0.01;
const LOSS_CLEAN: f64 = 0.001;

#[derive(Debug, Clone)]
pub struct AbrController {
    bitrate_kbps: u32,
    cap_kbps: u32,
    fec_percent: u8,
    bad_secs: u32,
    clean_secs: u32,
}

impl AbrController {
    pub fn new(initial_kbps: u32, cap_kbps: u32) -> Self {
        AbrController {
            bitrate_kbps: initial_kbps.min(cap_kbps),
            cap_kbps,
            fec_percent: DEFAULT_FEC,
            bad_secs: 0,
            clean_secs: 0,
        }
    }

    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    pub fn fec_percent(&self) -> u8 {
        self.fec_percent
    }

    /// Feeds one second of loss. Returns the new `(bitrate, fec)` when they change.
    pub fn on_report(&mut self, loss_ratio: f64) -> Option<(u32, u8)> {
        let before = (self.bitrate_kbps, self.fec_percent);
        if loss_ratio > LOSS_BAD {
            self.clean_secs = 0;
            self.bad_secs += 1;
            if self.bad_secs >= 2 {
                self.bad_secs = 0;
                self.bitrate_kbps = ((self.bitrate_kbps as f64 * 0.8) as u32).max(MIN_KBPS);
                self.fec_percent = (self.fec_percent + 5).min(MAX_ADAPTIVE_FEC);
            }
        } else {
            self.bad_secs = 0;
            if loss_ratio < LOSS_CLEAN {
                self.clean_secs += 1;
                if self.clean_secs >= 10 {
                    self.clean_secs = 0;
                    self.bitrate_kbps = ((self.bitrate_kbps as f64 * 1.1) as u32).min(self.cap_kbps);
                    self.fec_percent = self.fec_percent.saturating_sub(2).max(DEFAULT_FEC);
                }
            } else {
                self.clean_secs = 0;
            }
        }
        let after = (self.bitrate_kbps, self.fec_percent);
        (after != before).then_some(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_and_recovers() {
        let mut a = AbrController::new(80_000, 80_000);
        assert_eq!(a.on_report(0.05), None);
        assert_eq!(a.on_report(0.05), Some((64_000, 15)));
        for _ in 0..9 {
            assert_eq!(a.on_report(0.0), None);
        }
        assert_eq!(a.on_report(0.0), Some((70_400, 13)));
        for _ in 0..40 {
            a.on_report(0.0);
        }
        assert_eq!(a.bitrate_kbps(), 80_000);
        assert_eq!(a.fec_percent(), DEFAULT_FEC);
    }
}
