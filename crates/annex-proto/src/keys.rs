//! Long-term identity keys and key derivation helpers.

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::noise::NOISE_PARAMS;

/// A static X25519 keypair identifying one machine.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticKeypair {
    #[serde(with = "hex32")]
    pub private: [u8; 32],
    #[serde(with = "hex32")]
    pub public: [u8; 32],
}

impl std::fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StaticKeypair {{ public: {} }}", hex::encode(self.public))
    }
}

impl StaticKeypair {
    /// Generates a fresh keypair from the OS RNG.
    pub fn generate() -> Self {
        let builder = snow::Builder::new(NOISE_PARAMS.parse().expect("valid noise params"));
        let kp = builder.generate_keypair().expect("keypair generation");
        let mut private = [0u8; 32];
        let mut public = [0u8; 32];
        private.copy_from_slice(&kp.private);
        public.copy_from_slice(&kp.public);
        StaticKeypair { private, public }
    }
}

/// Fills a buffer from the OS RNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random number generator");
    b
}

/// HKDF-SHA256 expand of `ikm` with `salt` into a 32-byte key labelled by `info`.
pub fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("32 bytes is a valid HKDF length");
    okm
}

/// Serde helper: `[u8; 32]` as a hex string (keeps config files readable).
pub mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(s.trim()).map_err(serde::de::Error::custom)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypairs_are_unique() {
        let a = StaticKeypair::generate();
        let b = StaticKeypair::generate();
        assert_ne!(a.public, b.public);
        assert_ne!(a.private, b.private);
    }

    #[test]
    fn derive_is_deterministic_and_labelled() {
        let a = derive_key(b"ikm", b"salt", b"one");
        assert_eq!(a, derive_key(b"ikm", b"salt", b"one"));
        assert_ne!(a, derive_key(b"ikm", b"salt", b"two"));
        assert_ne!(a, derive_key(b"ikm", b"pepper", b"one"));
    }

    #[test]
    fn keypair_hex_serde() {
        let kp = StaticKeypair::generate();
        let s = toml_like(&kp);
        let back: StaticKeypair = postcard::from_bytes(&postcard::to_allocvec(&kp).unwrap()).unwrap();
        assert_eq!(kp, back);
        assert!(s.contains(&hex::encode(kp.public)));
    }

    fn toml_like(kp: &StaticKeypair) -> String {
        format!("{:?}", kp)
    }
}
