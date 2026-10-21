//! Sender side: access unit → FEC-protected, encrypted UDP datagrams.

use crate::crypto::{PacketCipher, TAG_LEN};
use crate::fec::{FecEncoder, FecError, parity_count};
use crate::packet::{
    FLAG_IDR, FLAG_PARITY, HEADER_LEN, MAX_DATAGRAM, MAX_SHARD, VideoHeader, video_nonce,
};

/// Bytes of length prefix in front of the access unit inside the shard stream.
const LEN_PREFIX: usize = 4;

/// A batch of datagrams for one frame, stored with a fixed stride so the
/// buffer can be reused without reallocating.
#[derive(Default)]
pub struct PacketBatch {
    buf: Vec<u8>,
    lens: Vec<u16>,
    pub stream_id: u8,
    pub frame_seq: u32,
    pub idr: bool,
}

impl PacketBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(datagrams: usize) -> Self {
        PacketBatch {
            buf: Vec::with_capacity(datagrams * MAX_DATAGRAM),
            lens: Vec::with_capacity(datagrams),
            ..Default::default()
        }
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.lens.clear();
    }

    pub fn len(&self) -> usize {
        self.lens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lens.is_empty()
    }

    pub fn get(&self, i: usize) -> &[u8] {
        let start = i * MAX_DATAGRAM;
        &self.buf[start..start + self.lens[i] as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8]> + '_ {
        (0..self.len()).map(move |i| self.get(i))
    }

    /// Total bytes on the wire (UDP payloads).
    pub fn wire_bytes(&self) -> usize {
        self.lens.iter().map(|l| *l as usize).sum()
    }

    fn push_slot(&mut self) -> &mut [u8] {
        let start = self.buf.len();
        self.buf.resize(start + MAX_DATAGRAM, 0);
        self.lens.push(0);
        &mut self.buf[start..start + MAX_DATAGRAM]
    }

    fn set_last_len(&mut self, len: usize) {
        *self.lens.last_mut().expect("slot pushed") = len as u16;
    }
}

/// Input to [`Packetizer::packetize`].
#[derive(Clone, Copy, Debug)]
pub struct FrameInput<'a> {
    pub stream_id: u8,
    /// One Annex B access unit (one frame).
    pub au: &'a [u8],
    pub idr: bool,
    /// µs since session start, low 32 bits.
    pub capture_ts: u32,
    pub fec_percent: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum PacketizeError {
    #[error("access unit too large ({0} bytes)")]
    TooLarge(usize),
    #[error(transparent)]
    Fec(#[from] FecError),
}

/// Splits frames into shards, adds parity, encrypts.
///
/// Frame sequence numbers are tracked per stream id for the lifetime of the
/// packetizer (= one session), so a stream id that is reconfigured or reused
/// keeps counting. That keeps every AEAD nonce unique under the session key.
pub struct Packetizer {
    cipher: PacketCipher,
    next_seq: [u32; 256],
    fec: FecEncoder,
    data: Vec<u8>,
    parity: Vec<u8>,
}

/// Shard geometry for a frame of `au_len` bytes: `(k, shard_size)`.
pub fn shard_layout(au_len: usize) -> Option<(usize, usize)> {
    let total = au_len + LEN_PREFIX;
    let k = total.div_ceil(MAX_SHARD);
    if k > u16::MAX as usize / 2 {
        return None;
    }
    let mut shard = total.div_ceil(k);
    shard += shard & 1;
    debug_assert!(shard <= MAX_SHARD);
    Some((k, shard))
}

impl Packetizer {
    pub fn new(key: &[u8; 32]) -> Self {
        Packetizer {
            cipher: PacketCipher::new(key),
            next_seq: [0; 256],
            fec: FecEncoder::new(),
            data: Vec::with_capacity(512 * 1024),
            parity: Vec::with_capacity(128 * 1024),
        }
    }

    /// Sequence number the next frame of `stream_id` will get.
    pub fn peek_seq(&self, stream_id: u8) -> u32 {
        self.next_seq[stream_id as usize]
    }

    /// Packetizes one frame into `out` (cleared first). Returns its `frame_seq`.
    pub fn packetize(
        &mut self,
        input: FrameInput<'_>,
        out: &mut PacketBatch,
    ) -> Result<u32, PacketizeError> {
        let (k, shard) = shard_layout(input.au.len()).ok_or(PacketizeError::TooLarge(input.au.len()))?;
        let m = parity_count(k, input.fec_percent, input.idr);

        self.data.clear();
        self.data.extend_from_slice(&(input.au.len() as u32).to_be_bytes());
        self.data.extend_from_slice(input.au);
        self.data.resize(k * shard, 0);
        self.parity.clear();
        self.parity.resize(m * shard, 0);
        self.fec.encode(&self.data, k, m, shard, &mut self.parity)?;

        let seq = self.next_seq[input.stream_id as usize];
        self.next_seq[input.stream_id as usize] = seq.wrapping_add(1);

        out.clear();
        out.stream_id = input.stream_id;
        out.frame_seq = seq;
        out.idr = input.idr;
        let base_flags = if input.idr { FLAG_IDR } else { 0 };
        for idx in 0..k + m {
            let (src, flags) = if idx < k {
                (&self.data[idx * shard..(idx + 1) * shard], base_flags)
            } else {
                let j = idx - k;
                (&self.parity[j * shard..(j + 1) * shard], base_flags | FLAG_PARITY)
            };
            let header = VideoHeader {
                stream_id: input.stream_id,
                flags,
                frame_seq: seq,
                shard_idx: idx as u16,
                data_shards: k as u16,
                parity_shards: m as u16,
                capture_ts: input.capture_ts,
                payload_len: (shard + TAG_LEN) as u16,
            };
            let slot = out.push_slot();
            let (hdr, rest) = slot.split_at_mut(HEADER_LEN);
            let hdr: &mut [u8; HEADER_LEN] = hdr.try_into().expect("header slice");
            header.encode(hdr);
            let (body, tag_out) = rest.split_at_mut(shard);
            body.copy_from_slice(src);
            let tag = self
                .cipher
                .seal(&video_nonce(input.stream_id, seq, idx as u16), hdr, body);
            tag_out[..TAG_LEN].copy_from_slice(&tag);
            out.set_last_len(HEADER_LEN + shard + TAG_LEN);
        }
        Ok(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_even_and_bounded() {
        for len in [0usize, 1, 2, 100, 1179, 1180, 1181, 5000, 166_000, 2_000_000] {
            let (k, s) = shard_layout(len).unwrap();
            assert_eq!(s % 2, 0);
            assert!(s <= MAX_SHARD);
            assert!(k * s >= len + LEN_PREFIX, "len {len}");
            assert!((k - 1) * s < len + LEN_PREFIX, "len {len} wastes a shard");
        }
    }

    #[test]
    fn packets_have_valid_headers() {
        let mut p = Packetizer::new(&[1; 32]);
        let au = vec![0x42u8; 5000];
        let mut batch = PacketBatch::new();
        let seq = p
            .packetize(
                FrameInput { stream_id: 1, au: &au, idr: true, capture_ts: 99, fec_percent: 10 },
                &mut batch,
            )
            .unwrap();
        assert_eq!(seq, 0);
        let (k, _) = shard_layout(5000).unwrap();
        let m = parity_count(k, 10, true);
        assert_eq!(batch.len(), k + m);
        for (i, d) in batch.iter().enumerate() {
            assert!(d.len() <= MAX_DATAGRAM);
            let h = VideoHeader::decode(d).unwrap();
            assert_eq!(h.shard_idx as usize, i);
            assert_eq!(h.stream_id, 1);
            assert!(h.is_idr());
            assert_eq!(h.is_parity(), i >= k);
            assert_eq!(h.capture_ts, 99);
        }
        let seq2 = p
            .packetize(
                FrameInput { stream_id: 1, au: &au, idr: false, capture_ts: 100, fec_percent: 10 },
                &mut batch,
            )
            .unwrap();
        assert_eq!(seq2, 1);
        // Independent counter per stream id.
        let s3 = p
            .packetize(
                FrameInput { stream_id: 2, au: &au, idr: false, capture_ts: 100, fec_percent: 0 },
                &mut batch,
            )
            .unwrap();
        assert_eq!(s3, 0);
        assert_eq!(batch.len(), k);
    }
}
