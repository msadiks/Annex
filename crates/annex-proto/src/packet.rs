//! UDP video packet header (spec section 8.5).
//!
//! ```text
//! magic         u8   = 0xA7
//! version       u8   = 1
//! stream_id     u8
//! flags         u8   bit0 = contains IDR, bit1 = FEC parity shard
//! frame_seq     u32  per-stream, increments per frame
//! shard_idx     u16
//! data_shards   u16  (k)
//! parity_shards u16  (m)
//! capture_ts    u32  µs since session start (sender clock), low 32 bits
//! payload_len   u16
//! payload       [u8; payload_len]  AEAD ciphertext + 16-byte tag
//! ```
//!
//! All integers are big-endian. The header is the AEAD associated data.

use crate::crypto::TAG_LEN;

/// First byte of every video packet.
pub const VIDEO_MAGIC: u8 = 0xA7;
/// Video packet format version.
pub const VIDEO_VERSION: u8 = 1;
/// Size of the encoded header in bytes.
pub const HEADER_LEN: usize = 20;
/// Largest payload (ciphertext + tag) we put in one packet. Keeps every
/// datagram under a 1500-byte MTU with IPv6 + UDP headers.
pub const MAX_PAYLOAD: usize = 1200;
/// Largest plaintext shard. Even, as Reed-Solomon requires.
pub const MAX_SHARD: usize = MAX_PAYLOAD - TAG_LEN;
/// Largest datagram we ever send.
pub const MAX_DATAGRAM: usize = HEADER_LEN + MAX_PAYLOAD;

/// `flags` bit: this frame is an IDR (keyframe).
pub const FLAG_IDR: u8 = 0x01;
/// `flags` bit: this shard is a Reed-Solomon parity shard.
pub const FLAG_PARITY: u8 = 0x02;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoHeader {
    pub stream_id: u8,
    pub flags: u8,
    pub frame_seq: u32,
    /// Index among all `k + m` shards of the frame. Data shards come first.
    pub shard_idx: u16,
    pub data_shards: u16,
    pub parity_shards: u16,
    pub capture_ts: u32,
    pub payload_len: u16,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("packet too short")]
    TooShort,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("payload length mismatch")]
    LengthMismatch,
    #[error("invalid shard layout")]
    BadShards,
}

impl VideoHeader {
    pub fn is_idr(&self) -> bool {
        self.flags & FLAG_IDR != 0
    }

    pub fn is_parity(&self) -> bool {
        self.flags & FLAG_PARITY != 0
    }

    /// Total shards in the frame (`k + m`).
    pub fn total_shards(&self) -> u32 {
        self.data_shards as u32 + self.parity_shards as u32
    }

    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0] = VIDEO_MAGIC;
        out[1] = VIDEO_VERSION;
        out[2] = self.stream_id;
        out[3] = self.flags;
        out[4..8].copy_from_slice(&self.frame_seq.to_be_bytes());
        out[8..10].copy_from_slice(&self.shard_idx.to_be_bytes());
        out[10..12].copy_from_slice(&self.data_shards.to_be_bytes());
        out[12..14].copy_from_slice(&self.parity_shards.to_be_bytes());
        out[14..18].copy_from_slice(&self.capture_ts.to_be_bytes());
        out[18..20].copy_from_slice(&self.payload_len.to_be_bytes());
    }

    /// Parses and validates a header. `datagram` is the whole UDP payload.
    pub fn decode(datagram: &[u8]) -> Result<Self, HeaderError> {
        if datagram.len() < HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        if datagram[0] != VIDEO_MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if datagram[1] != VIDEO_VERSION {
            return Err(HeaderError::BadVersion(datagram[1]));
        }
        let be16 = |i: usize| u16::from_be_bytes([datagram[i], datagram[i + 1]]);
        let be32 = |i: usize| {
            u32::from_be_bytes([datagram[i], datagram[i + 1], datagram[i + 2], datagram[i + 3]])
        };
        let h = VideoHeader {
            stream_id: datagram[2],
            flags: datagram[3],
            frame_seq: be32(4),
            shard_idx: be16(8),
            data_shards: be16(10),
            parity_shards: be16(12),
            capture_ts: be32(14),
            payload_len: be16(18),
        };
        if datagram.len() != HEADER_LEN + h.payload_len as usize {
            return Err(HeaderError::LengthMismatch);
        }
        let payload = h.payload_len as usize;
        if h.data_shards == 0
            || (h.shard_idx as u32) >= h.total_shards()
            || payload <= TAG_LEN
            || payload > MAX_PAYLOAD
            || (payload - TAG_LEN) % 2 != 0
            || h.is_parity() != (h.shard_idx >= h.data_shards)
        {
            return Err(HeaderError::BadShards);
        }
        Ok(h)
    }
}

/// Builds the 96-bit AEAD nonce for a video shard: `stream_id ‖ frame_seq ‖ shard_idx`,
/// zero padded. Unique per session as long as `frame_seq` never repeats for a stream id,
/// which the packetizer guarantees.
pub fn video_nonce(stream_id: u8, frame_seq: u32, shard_idx: u16) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = stream_id;
    n[1..5].copy_from_slice(&frame_seq.to_be_bytes());
    n[5..7].copy_from_slice(&shard_idx.to_be_bytes());
    n
}

/// Wrapping sequence comparison: true if `a` comes after `b`.
#[inline]
pub fn seq_after(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// Wrapping distance from `b` to `a` (positive if `a` is after `b`).
#[inline]
pub fn seq_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VideoHeader {
        VideoHeader {
            stream_id: 2,
            flags: FLAG_IDR | FLAG_PARITY,
            frame_seq: 0xDEADBEEF,
            shard_idx: 12,
            data_shards: 10,
            parity_shards: 3,
            capture_ts: 123_456_789,
            payload_len: 116,
        }
    }

    #[test]
    fn roundtrip() {
        let h = sample();
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        let mut dgram = buf.to_vec();
        dgram.extend(std::iter::repeat_n(0u8, h.payload_len as usize));
        assert_eq!(VideoHeader::decode(&dgram).unwrap(), h);
    }

    #[test]
    fn wire_layout_is_big_endian() {
        let h = sample();
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        assert_eq!(buf[0], 0xA7);
        assert_eq!(buf[1], 1);
        assert_eq!(&buf[4..8], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(&buf[18..20], &[0, 116]);
    }

    #[test]
    fn rejects_malformed() {
        let h = sample();
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        let mut dgram = buf.to_vec();
        dgram.extend(std::iter::repeat_n(0u8, h.payload_len as usize));

        assert_eq!(VideoHeader::decode(&dgram[..10]), Err(HeaderError::TooShort));
        let mut bad = dgram.clone();
        bad[0] = 0;
        assert_eq!(VideoHeader::decode(&bad), Err(HeaderError::BadMagic));
        let mut bad = dgram.clone();
        bad[1] = 9;
        assert_eq!(VideoHeader::decode(&bad), Err(HeaderError::BadVersion(9)));
        assert_eq!(
            VideoHeader::decode(&dgram[..dgram.len() - 1]),
            Err(HeaderError::LengthMismatch)
        );
        // Parity flag on a data shard index.
        let mut h2 = h;
        h2.shard_idx = 3;
        h2.encode(&mut buf);
        let mut bad = buf.to_vec();
        bad.extend(std::iter::repeat_n(0u8, h.payload_len as usize));
        assert_eq!(VideoHeader::decode(&bad), Err(HeaderError::BadShards));
    }

    #[test]
    fn seq_math_wraps() {
        assert!(seq_after(1, 0));
        assert!(seq_after(0, u32::MAX));
        assert!(!seq_after(u32::MAX, 0));
        assert_eq!(seq_diff(2, u32::MAX), 3);
    }

    #[test]
    fn nonces_are_distinct() {
        let a = video_nonce(1, 7, 3);
        let b = video_nonce(1, 7, 4);
        let c = video_nonce(2, 7, 3);
        let d = video_nonce(1, 8, 3);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }
}
