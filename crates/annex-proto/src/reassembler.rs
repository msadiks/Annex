//! Receiver side: datagrams → verified, FEC-repaired access units, in order.
//!
//! Rules (spec 5.3 and 8.5):
//! - A small ring of [`RING`] frames keyed by `frame_seq`.
//! - A frame completes when any `k` of its `k + m` shards arrived.
//! - Frames are delivered strictly in order. If a frame is still incomplete
//!   when a later one completes (or after [`FRAME_TIMEOUT`]), it is dropped,
//!   and everything up to the next IDR is discarded: HEVC P-frames reference
//!   the previous frame, so decoding them would only show corruption.
//! - While waiting for an IDR, [`Reassembler::idr_request`] yields a request
//!   to send over the control channel (rate limited).
//! - There is no jitter buffer: a complete frame is handed out immediately.

use std::time::{Duration, Instant};

use crate::crypto::{AuthError, PacketCipher, TAG_LEN};
use crate::fec::FecDecoder;
use crate::packet::{HEADER_LEN, HeaderError, VideoHeader, seq_diff, video_nonce};

/// Frames tracked at once.
pub const RING: usize = 8;
/// An incomplete frame older than this is given up.
pub const FRAME_TIMEOUT: Duration = Duration::from_millis(80);
/// Minimum spacing of repeated IDR requests for the same loss.
pub const IDR_RETRY: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("packet for stream {got}, reassembler is for stream {want}")]
    WrongStream { got: u8, want: u8 },
    #[error("shard layout disagrees with earlier shards of the frame")]
    Inconsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Accepted,
    Duplicate,
    /// The frame was already delivered or given up.
    Stale,
}

/// Metadata of a delivered frame. The access unit itself is in the caller's buffer.
#[derive(Debug, Clone, Copy)]
pub struct Delivery {
    pub frame_seq: u32,
    pub idr: bool,
    pub capture_ts: u32,
    /// FEC had to rebuild missing data shards.
    pub repaired: bool,
    pub first_packet_at: Instant,
    pub completed_at: Instant,
    pub au_len: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyStats {
    pub packets: u64,
    pub bytes: u64,
    pub duplicate_packets: u64,
    pub stale_packets: u64,
    pub auth_failures: u64,
    pub frames_delivered: u64,
    pub frames_repaired: u64,
    pub frames_lost: u64,
    /// Complete frames thrown away while waiting for an IDR.
    pub frames_skipped: u64,
    /// Shards of finished frames that never arrived (estimated packet loss).
    pub shards_missing: u64,
    pub idr_requests: u64,
}

impl ReassemblyStats {
    /// Packet loss over the counted interval, 0..=1.
    pub fn loss_ratio(&self) -> f64 {
        let total = self.packets + self.shards_missing;
        if total == 0 {
            0.0
        } else {
            self.shards_missing as f64 / total as f64
        }
    }

    /// Counter difference `self - earlier`, for per-second reports.
    pub fn delta(&self, earlier: &ReassemblyStats) -> ReassemblyStats {
        ReassemblyStats {
            packets: self.packets - earlier.packets,
            bytes: self.bytes - earlier.bytes,
            duplicate_packets: self.duplicate_packets - earlier.duplicate_packets,
            stale_packets: self.stale_packets - earlier.stale_packets,
            auth_failures: self.auth_failures - earlier.auth_failures,
            frames_delivered: self.frames_delivered - earlier.frames_delivered,
            frames_repaired: self.frames_repaired - earlier.frames_repaired,
            frames_lost: self.frames_lost - earlier.frames_lost,
            frames_skipped: self.frames_skipped - earlier.frames_skipped,
            shards_missing: self.shards_missing - earlier.shards_missing,
            idr_requests: self.idr_requests - earlier.idr_requests,
        }
    }
}

struct Slot {
    in_use: bool,
    seq: u32,
    k: usize,
    m: usize,
    shard: usize,
    idr: bool,
    capture_ts: u32,
    have_data: Vec<bool>,
    have_parity: Vec<bool>,
    n_data: usize,
    n_parity: usize,
    data: Vec<u8>,
    parity: Vec<u8>,
    first_at: Instant,
    completed_at: Option<Instant>,
}

impl Slot {
    fn new(now: Instant) -> Self {
        Slot {
            in_use: false,
            seq: 0,
            k: 0,
            m: 0,
            shard: 0,
            idr: false,
            capture_ts: 0,
            have_data: Vec::with_capacity(512),
            have_parity: Vec::with_capacity(128),
            n_data: 0,
            n_parity: 0,
            data: Vec::with_capacity(256 * 1024),
            parity: Vec::with_capacity(64 * 1024),
            first_at: now,
            completed_at: None,
        }
    }

    fn reset(&mut self, h: &VideoHeader, now: Instant) {
        self.in_use = true;
        self.seq = h.frame_seq;
        self.k = h.data_shards as usize;
        self.m = h.parity_shards as usize;
        self.shard = h.payload_len as usize - TAG_LEN;
        self.idr = h.is_idr();
        self.capture_ts = h.capture_ts;
        self.have_data.clear();
        self.have_data.resize(self.k, false);
        self.have_parity.clear();
        self.have_parity.resize(self.m, false);
        self.n_data = 0;
        self.n_parity = 0;
        self.data.clear();
        self.data.resize(self.k * self.shard, 0);
        self.parity.clear();
        self.parity.resize(self.m * self.shard, 0);
        self.first_at = now;
        self.completed_at = None;
    }

    fn received(&self) -> usize {
        self.n_data + self.n_parity
    }

    fn complete(&self) -> bool {
        self.completed_at.is_some()
    }

    fn missing(&self) -> u64 {
        (self.k + self.m - self.received()) as u64
    }
}

pub struct Reassembler {
    stream_id: u8,
    cipher: PacketCipher,
    slots: Vec<Slot>,
    next_seq: Option<u32>,
    last_delivered: Option<u32>,
    waiting_idr: bool,
    idr_needed_since: Option<Instant>,
    last_idr_request: Option<Instant>,
    fec: FecDecoder,
    stats: ReassemblyStats,
    frame_timeout: Duration,
}

/// Reads the stream id of a datagram without validating it (for demuxing).
pub fn peek_stream_id(datagram: &[u8]) -> Option<u8> {
    (datagram.len() >= HEADER_LEN && datagram[0] == crate::packet::VIDEO_MAGIC).then(|| datagram[2])
}

impl Reassembler {
    pub fn new(stream_id: u8, key: &[u8; 32]) -> Self {
        let now = Instant::now();
        Reassembler {
            stream_id,
            cipher: PacketCipher::new(key),
            slots: (0..RING).map(|_| Slot::new(now)).collect(),
            next_seq: None,
            last_delivered: None,
            waiting_idr: true,
            idr_needed_since: None,
            last_idr_request: None,
            fec: FecDecoder::new(),
            stats: ReassemblyStats::default(),
            frame_timeout: FRAME_TIMEOUT,
        }
    }

    pub fn set_frame_timeout(&mut self, t: Duration) {
        self.frame_timeout = t;
    }

    pub fn stats(&self) -> ReassemblyStats {
        self.stats
    }

    pub fn waiting_for_idr(&self) -> bool {
        self.waiting_idr
    }

    /// Verifies, decrypts and stores one datagram. The buffer is decrypted in place.
    pub fn push(&mut self, datagram: &mut [u8], now: Instant) -> Result<PushOutcome, PushError> {
        let h = VideoHeader::decode(datagram)?;
        if h.stream_id != self.stream_id {
            return Err(PushError::WrongStream { got: h.stream_id, want: self.stream_id });
        }
        let (hdr, payload) = datagram.split_at_mut(HEADER_LEN);
        let (body, tag) = payload.split_at_mut(payload.len() - TAG_LEN);
        if let Err(e) = self
            .cipher
            .open(&video_nonce(h.stream_id, h.frame_seq, h.shard_idx), hdr, body, tag)
        {
            self.stats.auth_failures += 1;
            return Err(e.into());
        }
        self.stats.packets += 1;
        self.stats.bytes += (HEADER_LEN + h.payload_len as usize) as u64;

        let next = *self.next_seq.get_or_insert(h.frame_seq);
        let ahead = seq_diff(h.frame_seq, next);
        if ahead < 0 {
            self.stats.stale_packets += 1;
            return Ok(PushOutcome::Stale);
        }
        if ahead as usize >= RING {
            // The sender is more than a ring ahead of us: everything before the
            // new window is lost.
            let new_next = h.frame_seq.wrapping_sub(RING as u32 - 1);
            self.give_up_until(new_next, now);
        }

        let idx = (h.frame_seq as usize) % RING;
        let slot = &mut self.slots[idx];
        if slot.in_use && slot.seq != h.frame_seq {
            // Leftover from an older window (only possible right after a jump).
            self.stats.shards_missing += slot.missing();
            self.stats.frames_lost += 1;
            slot.in_use = false;
        }
        if !slot.in_use {
            slot.reset(&h, now);
        } else if slot.k != h.data_shards as usize
            || slot.m != h.parity_shards as usize
            || slot.shard != h.payload_len as usize - TAG_LEN
        {
            return Err(PushError::Inconsistent);
        }
        if slot.complete() {
            // Late shard of a frame that is already complete but not yet delivered.
            self.stats.duplicate_packets += 1;
            return Ok(PushOutcome::Duplicate);
        }
        let i = h.shard_idx as usize;
        let ss = slot.shard;
        if i < slot.k {
            if slot.have_data[i] {
                self.stats.duplicate_packets += 1;
                return Ok(PushOutcome::Duplicate);
            }
            slot.data[i * ss..(i + 1) * ss].copy_from_slice(body);
            slot.have_data[i] = true;
            slot.n_data += 1;
        } else {
            let j = i - slot.k;
            if slot.have_parity[j] {
                self.stats.duplicate_packets += 1;
                return Ok(PushOutcome::Duplicate);
            }
            slot.parity[j * ss..(j + 1) * ss].copy_from_slice(body);
            slot.have_parity[j] = true;
            slot.n_parity += 1;
        }
        if slot.received() >= slot.k {
            slot.completed_at = Some(now);
        }
        Ok(PushOutcome::Accepted)
    }

    /// Hands out the next in-order complete frame, copying its access unit into
    /// `out`. Call repeatedly after each push until it returns `None`.
    pub fn pop(&mut self, out: &mut Vec<u8>, now: Instant) -> Option<Delivery> {
        loop {
            let next = self.next_seq?;
            let idx = (next as usize) % RING;
            let ready = {
                let s = &self.slots[idx];
                s.in_use && s.seq == next && s.complete()
            };
            if ready {
                match self.take_frame(idx, out) {
                    Some(d) => {
                        self.next_seq = Some(next.wrapping_add(1));
                        if self.waiting_idr && !d.idr {
                            self.stats.frames_skipped += 1;
                            self.idr_needed_since.get_or_insert(now);
                            continue;
                        }
                        if d.idr {
                            self.waiting_idr = false;
                            self.idr_needed_since = None;
                        }
                        self.stats.frames_delivered += 1;
                        if d.repaired {
                            self.stats.frames_repaired += 1;
                        }
                        self.last_delivered = Some(d.frame_seq);
                        return Some(d);
                    }
                    None => {
                        // Recovery failed (should not happen with >= k shards).
                        self.drop_frame(next, now);
                        continue;
                    }
                }
            }
            // `next` is not ready. Give up on it if a later frame is complete or it timed out.
            let later_complete = self
                .slots
                .iter()
                .any(|s| s.in_use && s.complete() && seq_diff(s.seq, next) > 0);
            let timed_out = {
                let s = &self.slots[idx];
                s.in_use && s.seq == next && now.duration_since(s.first_at) > self.frame_timeout
            };
            // A frame we never saw a packet of, with later frames already arriving.
            let unseen_and_passed = {
                let s = &self.slots[idx];
                !(s.in_use && s.seq == next)
                    && self.slots.iter().any(|o| {
                        o.in_use
                            && seq_diff(o.seq, next) > 0
                            && now.duration_since(o.first_at) > self.frame_timeout
                    })
            };
            if later_complete || timed_out || unseen_and_passed {
                self.drop_frame(next, now);
                continue;
            }
            return None;
        }
    }

    /// Returns `Some(last_good_seq)` when an IDR should be requested now.
    pub fn idr_request(&mut self, now: Instant) -> Option<u32> {
        self.idr_needed_since?;
        if !self.waiting_idr {
            self.idr_needed_since = None;
            return None;
        }
        if let Some(last) = self.last_idr_request {
            if now.duration_since(last) < IDR_RETRY {
                return None;
            }
        }
        self.last_idr_request = Some(now);
        self.stats.idr_requests += 1;
        Some(self.last_delivered.unwrap_or(0))
    }

    /// Forces the reassembler to wait for an IDR (e.g. after a decoder error).
    pub fn require_idr(&mut self, now: Instant) {
        self.waiting_idr = true;
        self.idr_needed_since.get_or_insert(now);
    }

    fn drop_frame(&mut self, seq: u32, now: Instant) {
        let idx = (seq as usize) % RING;
        let s = &mut self.slots[idx];
        if s.in_use && s.seq == seq {
            self.stats.shards_missing += s.missing();
            s.in_use = false;
        }
        self.stats.frames_lost += 1;
        self.next_seq = Some(seq.wrapping_add(1));
        self.waiting_idr = true;
        self.idr_needed_since.get_or_insert(now);
    }

    fn give_up_until(&mut self, new_next: u32, now: Instant) {
        while let Some(n) = self.next_seq {
            if seq_diff(new_next, n) <= 0 {
                break;
            }
            self.drop_frame(n, now);
        }
    }

    fn take_frame(&mut self, idx: usize, out: &mut Vec<u8>) -> Option<Delivery> {
        let s = &mut self.slots[idx];
        s.in_use = false;
        let repaired = s.n_data < s.k;
        if repaired
            && self
                .fec
                .recover(&mut s.data, &s.have_data, &s.parity, &s.have_parity, s.k, s.m, s.shard)
                .is_err()
        {
            return None;
        }
        // Parity that arrives after completion is not stored, so only missing
        // data shards are known losses here.
        self.stats.shards_missing += (s.k - s.n_data) as u64;
        if s.data.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes([s.data[0], s.data[1], s.data[2], s.data[3]]) as usize;
        if 4 + len > s.data.len() {
            return None;
        }
        out.clear();
        out.extend_from_slice(&s.data[4..4 + len]);
        Some(Delivery {
            frame_seq: s.seq,
            idr: s.idr,
            capture_ts: s.capture_ts,
            repaired,
            first_packet_at: s.first_at,
            completed_at: s.completed_at.unwrap_or(s.first_at),
            au_len: len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packetizer::{FrameInput, PacketBatch, Packetizer};

    const KEY: [u8; 32] = [5; 32];

    fn frame(p: &mut Packetizer, seq_hint: u8, len: usize, idr: bool, fec: u8) -> PacketBatch {
        let au: Vec<u8> = (0..len).map(|i| (i as u8) ^ seq_hint).collect();
        let mut b = PacketBatch::new();
        p.packetize(FrameInput { stream_id: 0, au: &au, idr, capture_ts: seq_hint as u32, fec_percent: fec }, &mut b)
            .unwrap();
        b
    }

    fn feed(r: &mut Reassembler, b: &PacketBatch, skip: &[usize], now: Instant) {
        for (i, d) in b.iter().enumerate() {
            if skip.contains(&i) {
                continue;
            }
            let mut d = d.to_vec();
            r.push(&mut d, now).unwrap();
        }
    }

    fn drain(r: &mut Reassembler, now: Instant) -> Vec<(u32, bool, Vec<u8>)> {
        let mut v = vec![];
        let mut out = Vec::new();
        while let Some(d) = r.pop(&mut out, now) {
            v.push((d.frame_seq, d.idr, out.clone()));
        }
        v
    }

    #[test]
    fn delivers_in_order_without_loss() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 9000, true, 10);
        let f1 = frame(&mut p, 1, 300, false, 10);
        feed(&mut r, &f0, &[], now);
        feed(&mut r, &f1, &[], now);
        let got = drain(&mut r, now);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 0);
        assert!(got[0].1);
        assert_eq!(got[0].2.len(), 9000);
        assert_eq!(got[0].2[17], 17u8);
        assert_eq!(got[1].2.len(), 300);
        assert_eq!(got[1].2[3], 3u8 ^ 1);
        assert_eq!(r.idr_request(now), None);
        assert_eq!(r.stats().frames_delivered, 2);
        assert_eq!(r.stats().shards_missing, 0);
    }

    #[test]
    fn fec_repairs_lost_shards() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 20_000, true, 20); // k=17, m>=4
        feed(&mut r, &f0, &[0, 5, 9], now);
        let got = drain(&mut r, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2, (0..20_000).map(|i| i as u8).collect::<Vec<_>>());
        assert_eq!(r.stats().frames_repaired, 1);
        assert_eq!(r.stats().shards_missing, 3);
    }

    #[test]
    fn loss_drops_until_next_idr_and_requests_one() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 3000, true, 0);
        let f1 = frame(&mut p, 1, 3000, false, 0);
        let f2 = frame(&mut p, 2, 3000, false, 0);
        let f3 = frame(&mut p, 3, 3000, true, 0);
        feed(&mut r, &f0, &[], now);
        assert_eq!(drain(&mut r, now).len(), 1);
        feed(&mut r, &f1, &[1], now); // unrecoverable (no FEC)
        feed(&mut r, &f2, &[], now);
        assert!(drain(&mut r, now).is_empty(), "f1 lost, f2 must be skipped");
        assert_eq!(r.idr_request(now), Some(0));
        assert_eq!(r.idr_request(now), None, "rate limited");
        assert_eq!(r.idr_request(now + IDR_RETRY), Some(0), "retried");
        feed(&mut r, &f3, &[], now);
        let got = drain(&mut r, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 3);
        assert!(got[0].1);
        assert_eq!(r.idr_request(now + IDR_RETRY * 4), None);
        let s = r.stats();
        assert_eq!(s.frames_lost, 1);
        assert_eq!(s.frames_skipped, 1);
    }

    #[test]
    fn incomplete_frame_times_out() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let t0 = Instant::now();
        let f0 = frame(&mut p, 0, 3000, true, 0); // k=3, m=1 (IDR minimum)
        feed(&mut r, &f0, &[1, 3], t0);
        assert!(drain(&mut r, t0).is_empty());
        assert!(drain(&mut r, t0 + FRAME_TIMEOUT + Duration::from_millis(1)).is_empty());
        assert_eq!(r.stats().frames_lost, 1);
        assert!(r.idr_request(t0 + FRAME_TIMEOUT * 2).is_some());
    }

    #[test]
    fn reordered_shards_within_frame_are_fine() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 10_000, true, 10);
        let mut pkts: Vec<Vec<u8>> = f0.iter().map(|d| d.to_vec()).collect();
        pkts.reverse();
        for mut d in pkts {
            r.push(&mut d, now).unwrap();
        }
        assert_eq!(drain(&mut r, now).len(), 1);
    }

    #[test]
    fn duplicates_and_stale_are_counted() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 2000, true, 0);
        let mut d0 = f0.get(0).to_vec();
        let mut d0b = d0.clone();
        assert_eq!(r.push(&mut d0, now).unwrap(), PushOutcome::Accepted);
        assert_eq!(r.push(&mut d0b, now).unwrap(), PushOutcome::Duplicate);
        let mut d1 = f0.get(1).to_vec();
        r.push(&mut d1, now).unwrap();
        assert_eq!(drain(&mut r, now).len(), 1);
        let mut again = f0.get(0).to_vec();
        assert_eq!(r.push(&mut again, now).unwrap(), PushOutcome::Stale);
    }

    #[test]
    fn rejects_forgeries_and_wrong_key() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &[6; 32]);
        let f0 = frame(&mut p, 0, 2000, true, 0);
        let mut d = f0.get(0).to_vec();
        assert!(matches!(r.push(&mut d, Instant::now()), Err(PushError::Auth(_))));
        let mut r = Reassembler::new(0, &KEY);
        let mut d = f0.get(0).to_vec();
        d[30] ^= 0x80;
        assert!(matches!(r.push(&mut d, Instant::now()), Err(PushError::Auth(_))));
        let mut d = f0.get(0).to_vec();
        d[4] ^= 1; // header tampering breaks the AAD
        assert!(matches!(r.push(&mut d, Instant::now()), Err(PushError::Auth(_))));
        assert_eq!(r.stats().auth_failures, 2);
    }

    #[test]
    fn far_jump_ahead_resyncs() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let f0 = frame(&mut p, 0, 1000, true, 0);
        feed(&mut r, &f0, &[], now);
        assert_eq!(drain(&mut r, now).len(), 1);
        // Skip 20 frames entirely (e.g. Wi-Fi outage), then an IDR arrives.
        for _ in 0..20 {
            frame(&mut p, 0, 1000, false, 0);
        }
        let idr = frame(&mut p, 9, 1000, true, 0);
        feed(&mut r, &idr, &[], now);
        let got = drain(&mut r, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 21);
    }

    #[test]
    fn joining_mid_stream_waits_for_idr() {
        let mut p = Packetizer::new(&KEY);
        let mut r = Reassembler::new(0, &KEY);
        let now = Instant::now();
        let _missed = frame(&mut p, 0, 1000, true, 0);
        let p1 = frame(&mut p, 1, 1000, false, 0);
        feed(&mut r, &p1, &[], now);
        assert!(drain(&mut r, now).is_empty());
        assert_eq!(r.idr_request(now), Some(0));
        let i2 = frame(&mut p, 2, 1000, true, 0);
        feed(&mut r, &i2, &[], now);
        assert_eq!(drain(&mut r, now).len(), 1);
    }
}
