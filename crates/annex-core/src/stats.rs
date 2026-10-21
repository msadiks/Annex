//! Timing helpers: per-second averages, rate meters, NTP-style clock sync and
//! 32-bit timestamp unwrapping (spec 10).

use std::time::Instant;

/// Microseconds since `epoch`.
pub fn us_since(epoch: Instant, t: Instant) -> u64 {
    t.saturating_duration_since(epoch).as_micros() as u64
}

/// Mean of samples since the last `take`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Mean {
    sum: f64,
    n: u32,
}

impl Mean {
    pub fn add(&mut self, v: f64) {
        self.sum += v;
        self.n += 1;
    }

    pub fn count(&self) -> u32 {
        self.n
    }

    /// Returns the mean (or `None` without samples) and resets.
    pub fn take(&mut self) -> Option<f64> {
        let r = (self.n > 0).then(|| self.sum / self.n as f64);
        *self = Mean::default();
        r
    }
}

/// Clock offset estimator. Uses the lowest-RTT sample among the last few,
/// which is the one least distorted by queueing.
#[derive(Debug, Default, Clone)]
pub struct ClockSync {
    samples: Vec<(u64, i64)>, // (rtt, offset)
}

impl ClockSync {
    const KEEP: usize = 8;

    /// `t1`, `t4`: our send/receive times; `t2`, `t3`: peer receive/send times
    /// (peer clock). All µs.
    pub fn add(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        let rtt = (t4 as i64 - t1 as i64) - (t3 as i64 - t2 as i64);
        if rtt < 0 {
            return;
        }
        let offset = ((t2 as i64 - t1 as i64) + (t3 as i64 - t4 as i64)) / 2;
        if self.samples.len() == Self::KEEP {
            self.samples.remove(0);
        }
        self.samples.push((rtt as u64, offset));
    }

    /// Peer clock minus our clock, in µs.
    pub fn offset_us(&self) -> Option<i64> {
        self.samples.iter().min_by_key(|s| s.0).map(|s| s.1)
    }

    pub fn rtt_us(&self) -> Option<u64> {
        self.samples.iter().map(|s| s.0).min()
    }
}

/// Expands a 32-bit µs timestamp to 64 bits using a nearby full value.
pub fn unwrap_ts(ts32: u32, near: u64) -> u64 {
    let base = near & !0xFFFF_FFFF;
    let candidates = [base.wrapping_sub(1 << 32), base, base + (1 << 32)];
    candidates
        .iter()
        .map(|b| b + ts32 as u64)
        .min_by_key(|c| (*c as i64 - near as i64).unsigned_abs())
        .unwrap_or(ts32 as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_sync_picks_min_rtt() {
        let mut c = ClockSync::default();
        // Peer is 1000 µs ahead; 100 µs one-way delay.
        c.add(0, 1100, 1110, 210);
        // Same offset but asymmetric queueing on the way back: worse sample.
        c.add(500, 1600, 1610, 2000);
        assert_eq!(c.offset_us(), Some(1000));
        assert_eq!(c.rtt_us(), Some(200));
    }

    #[test]
    fn unwrap_handles_wrap() {
        assert_eq!(unwrap_ts(5, (1u64 << 32) + 10), (1u64 << 32) + 5);
        assert_eq!(unwrap_ts(u32::MAX - 5, (1u64 << 32) + 10), (1u64 << 32) - 6);
        assert_eq!(unwrap_ts(100, 90), 100);
    }
}
