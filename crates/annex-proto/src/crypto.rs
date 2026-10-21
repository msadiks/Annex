//! Per-packet AEAD (ChaCha20-Poly1305) for the UDP channels.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};

/// Poly1305 tag length.
pub const TAG_LEN: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("packet authentication failed")]
pub struct AuthError;

/// A symmetric packet key for one direction of one channel.
#[derive(Clone)]
pub struct PacketCipher {
    aead: ChaCha20Poly1305,
}

impl std::fmt::Debug for PacketCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PacketCipher(..)")
    }
}

impl PacketCipher {
    pub fn new(key: &[u8; 32]) -> Self {
        PacketCipher {
            aead: ChaCha20Poly1305::new(Key::from_slice(key)),
        }
    }

    /// Encrypts `buf` in place and returns the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], buf: &mut [u8]) -> [u8; TAG_LEN] {
        let tag = self
            .aead
            .encrypt_in_place_detached(Nonce::from_slice(nonce), aad, buf)
            .expect("chacha20poly1305 encryption cannot fail for packet-sized buffers");
        tag.into()
    }

    /// Verifies and decrypts `buf` in place.
    pub fn open(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8],
    ) -> Result<(), AuthError> {
        if tag.len() != TAG_LEN {
            return Err(AuthError);
        }
        self.aead
            .decrypt_in_place_detached(Nonce::from_slice(nonce), aad, buf, Tag::from_slice(tag))
            .map_err(|_| AuthError)
    }
}

/// Seals a small standalone message (pairing payloads) and returns ciphertext ‖ tag.
pub fn seal_message(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let c = PacketCipher::new(key);
    let mut out = plaintext.to_vec();
    let tag = c.seal(nonce, aad, &mut out);
    out.extend_from_slice(&tag);
    out
}

/// Opens a message produced by [`seal_message`].
pub fn open_message(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, AuthError> {
    if sealed.len() < TAG_LEN {
        return Err(AuthError);
    }
    let (ct, tag) = sealed.split_at(sealed.len() - TAG_LEN);
    let mut out = ct.to_vec();
    PacketCipher::new(key).open(nonce, aad, &mut out, tag)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = [7u8; 32];
        let c = PacketCipher::new(&key);
        let nonce = [1u8; 12];
        let mut buf = b"hello annex".to_vec();
        let tag = c.seal(&nonce, b"hdr", &mut buf);
        assert_ne!(&buf[..], b"hello annex");
        c.open(&nonce, b"hdr", &mut buf, &tag).unwrap();
        assert_eq!(&buf[..], b"hello annex");
    }

    #[test]
    fn tampering_is_detected() {
        let key = [7u8; 32];
        let c = PacketCipher::new(&key);
        let nonce = [1u8; 12];
        let mut buf = b"hello annex".to_vec();
        let tag = c.seal(&nonce, b"hdr", &mut buf);

        let mut b2 = buf.clone();
        b2[0] ^= 1;
        assert!(c.open(&nonce, b"hdr", &mut b2, &tag).is_err());
        let mut b3 = buf.clone();
        assert!(c.open(&nonce, b"hdX", &mut b3, &tag).is_err());
        let mut b4 = buf.clone();
        assert!(c.open(&[2u8; 12], b"hdr", &mut b4, &tag).is_err());
        let other = PacketCipher::new(&[8u8; 32]);
        let mut b5 = buf.clone();
        assert!(other.open(&nonce, b"hdr", &mut b5, &tag).is_err());
    }

    #[test]
    fn message_helpers() {
        let key = [3u8; 32];
        let sealed = seal_message(&key, &[0; 12], b"a", b"payload");
        assert_eq!(open_message(&key, &[0; 12], b"a", &sealed).unwrap(), b"payload");
        assert!(open_message(&[4; 32], &[0; 12], b"a", &sealed).is_err());
        assert!(open_message(&key, &[0; 12], b"a", &sealed[..3]).is_err());
    }
}
