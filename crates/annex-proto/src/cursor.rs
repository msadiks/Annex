//! Client-side cursor position packets (UDP, separate key).
//!
//! ```text
//! magic     u8 = 0xA8
//! version   u8 = 1
//! stream_id u8
//! flags     u8   bit0 = visible
//! seq       u32  per stream, increments per packet
//! --- encrypted (12 bytes + 16 tag) ---
//! x         i32  hotspot position in stream pixels
//! y         i32
//! shape_id  u32  matches a CursorShape sent on the control channel
//! ```
//! The 8-byte header is the AEAD associated data. Images travel on the
//! control channel as [`crate::control::CursorShape`].

use crate::crypto::{PacketCipher, TAG_LEN};

pub const CURSOR_MAGIC: u8 = 0xA8;
pub const CURSOR_VERSION: u8 = 1;
pub const CURSOR_HEADER_LEN: usize = 8;
pub const CURSOR_PACKET_LEN: usize = CURSOR_HEADER_LEN + 12 + TAG_LEN;
pub const CURSOR_FLAG_VISIBLE: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub stream_id: u8,
    pub visible: bool,
    pub seq: u32,
    pub x: i32,
    pub y: i32,
    pub shape_id: u32,
}

fn nonce(stream_id: u8, seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = 0xC0;
    n[1] = stream_id;
    n[2..6].copy_from_slice(&seq.to_be_bytes());
    n
}

pub fn encode(c: &CursorPos, cipher: &PacketCipher) -> [u8; CURSOR_PACKET_LEN] {
    let mut out = [0u8; CURSOR_PACKET_LEN];
    out[0] = CURSOR_MAGIC;
    out[1] = CURSOR_VERSION;
    out[2] = c.stream_id;
    out[3] = if c.visible { CURSOR_FLAG_VISIBLE } else { 0 };
    out[4..8].copy_from_slice(&c.seq.to_be_bytes());
    let (hdr, rest) = out.split_at_mut(CURSOR_HEADER_LEN);
    let (body, tag) = rest.split_at_mut(12);
    body[0..4].copy_from_slice(&c.x.to_be_bytes());
    body[4..8].copy_from_slice(&c.y.to_be_bytes());
    body[8..12].copy_from_slice(&c.shape_id.to_be_bytes());
    let t = cipher.seal(&nonce(c.stream_id, c.seq), hdr, body);
    tag.copy_from_slice(&t);
    out
}

/// Returns `None` for anything that is not an authentic cursor packet.
pub fn decode(datagram: &[u8], cipher: &PacketCipher) -> Option<CursorPos> {
    if datagram.len() != CURSOR_PACKET_LEN || datagram[0] != CURSOR_MAGIC || datagram[1] != CURSOR_VERSION {
        return None;
    }
    let mut buf = [0u8; CURSOR_PACKET_LEN];
    buf.copy_from_slice(datagram);
    let stream_id = buf[2];
    let flags = buf[3];
    let seq = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let (hdr, rest) = buf.split_at_mut(CURSOR_HEADER_LEN);
    let (body, tag) = rest.split_at_mut(12);
    cipher.open(&nonce(stream_id, seq), hdr, body, tag).ok()?;
    let be = |i: usize| [body[i], body[i + 1], body[i + 2], body[i + 3]];
    Some(CursorPos {
        stream_id,
        visible: flags & CURSOR_FLAG_VISIBLE != 0,
        seq,
        x: i32::from_be_bytes(be(0)),
        y: i32::from_be_bytes(be(4)),
        shape_id: u32::from_be_bytes(be(8)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_auth() {
        let c = PacketCipher::new(&[9; 32]);
        let p = CursorPos { stream_id: 2, visible: true, seq: 77, x: -5, y: 1079, shape_id: 3 };
        let mut d = encode(&p, &c);
        assert_eq!(decode(&d, &c), Some(p));
        d[10] ^= 1;
        assert_eq!(decode(&d, &c), None);
        assert_eq!(decode(&encode(&p, &c), &PacketCipher::new(&[8; 32])), None);
    }
}
