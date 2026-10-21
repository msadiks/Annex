//! Lossy-channel simulator (spec 8.7).
//!
//! `packetize → channel (loss, bursts, reordering, jitter, duplicates) →
//! reassemble`. Used by the tests to show FEC recovery, nonce uniqueness and
//! in-order delivery; also handy for local experiments.

use std::time::{Duration, Instant};

/// Deterministic SplitMix64, so failures reproduce from the seed.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Debug)]
pub struct ChannelConfig {
    /// Independent loss probability per packet.
    pub loss: f64,
    /// Gilbert-Elliott burst model: probability to enter / leave the bad state.
    /// In the bad state every packet is lost.
    pub burst_enter: f64,
    pub burst_exit: f64,
    /// Probability that a packet is delayed by an extra `reorder_delay`.
    pub reorder: f64,
    pub reorder_delay: Duration,
    /// Uniform jitter added to every packet.
    pub jitter: Duration,
    pub duplicate: f64,
    pub seed: u64,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        ChannelConfig {
            loss: 0.0,
            burst_enter: 0.0,
            burst_exit: 1.0,
            reorder: 0.0,
            reorder_delay: Duration::from_micros(500),
            jitter: Duration::ZERO,
            duplicate: 0.0,
            seed: 1,
        }
    }
}

pub struct LossyChannel {
    cfg: ChannelConfig,
    rng: Rng,
    bad: bool,
    in_flight: Vec<(Instant, u64, Vec<u8>)>,
    order: u64,
}

impl LossyChannel {
    pub fn new(cfg: ChannelConfig) -> Self {
        let rng = Rng::new(cfg.seed);
        LossyChannel { cfg, rng, bad: false, in_flight: Vec::new(), order: 0 }
    }

    /// Sends one datagram at time `at`.
    pub fn send(&mut self, datagram: &[u8], at: Instant) {
        if self.bad {
            if self.rng.unit() < self.cfg.burst_exit {
                self.bad = false;
            }
        } else if self.rng.unit() < self.cfg.burst_enter {
            self.bad = true;
        }
        if self.bad || self.rng.unit() < self.cfg.loss {
            return;
        }
        let copies = if self.rng.unit() < self.cfg.duplicate { 2 } else { 1 };
        for _ in 0..copies {
            let mut t = at + self.cfg.jitter.mul_f64(self.rng.unit());
            if self.rng.unit() < self.cfg.reorder {
                t += self.cfg.reorder_delay;
            }
            self.order += 1;
            self.in_flight.push((t, self.order, datagram.to_vec()));
        }
    }

    /// Returns all datagrams that arrive by `now`, in arrival order.
    pub fn receive(&mut self, now: Instant) -> Vec<(Instant, Vec<u8>)> {
        self.in_flight.sort_by_key(|(t, o, _)| (*t, *o));
        let split = self.in_flight.partition_point(|(t, _, _)| *t <= now);
        self.in_flight.drain(..split).map(|(t, _, d)| (t, d)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::VideoHeader;
    use crate::packetizer::{FrameInput, PacketBatch, Packetizer};
    use crate::reassembler::Reassembler;
    use std::collections::HashSet;

    const KEY: [u8; 32] = [0x11; 32];

    struct Outcome {
        delivered: Vec<u32>,
        idr_requests: u64,
        repaired: u64,
        lost: u64,
    }

    /// Streams `frames` frames at 60 fps (IDR every 120 frames, or when the
    /// receiver asks) through the channel and checks content and order.
    fn run(cfg: ChannelConfig, frames: u32, fec: u8) -> Outcome {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let mut ch = LossyChannel::new(cfg.clone());
        let mut rng = Rng::new(cfg.seed ^ 0xABCD);
        let mut batch = PacketBatch::new();
        let mut out = Vec::new();
        let mut delivered = Vec::new();
        let mut nonces = HashSet::new();
        let mut want_idr = true;
        let t0 = Instant::now();
        for f in 0..frames {
            let now = t0 + Duration::from_micros(16_667 * f as u64);
            let idr = want_idr || f % 120 == 0;
            want_idr = false;
            let len = if idr { 60_000 } else { 2_000 + (rng.next_u64() % 20_000) as usize };
            let au: Vec<u8> = (0..len).map(|i| (i as u32 ^ f) as u8).collect();
            let seq = p
                .packetize(FrameInput { stream_id: 0, au: &au, idr, capture_ts: f, fec_percent: fec }, &mut batch)
                .unwrap();
            let n = batch.len().max(1) as u32;
            for (i, d) in batch.iter().enumerate() {
                let h = VideoHeader::decode(d).unwrap();
                assert!(nonces.insert((h.stream_id, h.frame_seq, h.shard_idx)), "nonce reuse");
                // Pace packets over ~2 ms.
                ch.send(d, now + Duration::from_micros(2000 * i as u64 / n as u64));
            }
            for (at, mut d) in ch.receive(now + Duration::from_millis(16)) {
                r.push(&mut d, at).unwrap();
                while let Some(del) = r.pop(&mut out, at) {
                    // Content check: frame index is the capture timestamp here.
                    let fi = del.capture_ts;
                    assert!(out.iter().enumerate().all(|(i, b)| *b == (i as u32 ^ fi) as u8));
                    if let Some(last) = delivered.last() {
                        assert!(del.frame_seq > *last, "out of order");
                    }
                    delivered.push(del.frame_seq);
                }
                if r.idr_request(at).is_some() {
                    want_idr = true;
                }
            }
            let _ = seq;
        }
        let s = r.stats();
        Outcome { delivered, idr_requests: s.idr_requests, repaired: s.frames_repaired, lost: s.frames_lost }
    }

    #[test]
    fn clean_channel_delivers_everything() {
        let o = run(ChannelConfig { jitter: Duration::from_micros(300), ..Default::default() }, 600, 10);
        assert_eq!(o.delivered.len(), 600);
        assert_eq!(o.idr_requests, 0);
    }

    #[test]
    fn fec_absorbs_one_percent_random_loss() {
        let o = run(ChannelConfig { loss: 0.01, seed: 7, ..Default::default() }, 1200, 10);
        assert!(o.repaired > 50, "FEC should be doing work: {}", o.repaired);
        let ratio = o.delivered.len() as f64 / 1200.0;
        assert!(ratio > 0.97, "delivered {ratio}");
    }

    #[test]
    fn bursts_recover_via_idr() {
        let cfg = ChannelConfig { loss: 0.002, burst_enter: 0.0005, burst_exit: 0.05, reorder: 0.01, seed: 3, ..Default::default() };
        let o = run(cfg, 1800, 10);
        assert!(o.lost > 0, "the burst model should lose some frames");
        assert!(o.idr_requests > 0);
        // After each loss the stream must come back: the last frames get through.
        assert!(o.delivered.last().copied().unwrap_or(0) > 1700);
    }
}
