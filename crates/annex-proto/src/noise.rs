//! Session handshake: `Noise_IK_25519_ChaChaPoly_BLAKE2s` (spec 8.3).
//!
//! The sender is the initiator and already knows the receiver's static key
//! from pairing. The receiver learns and checks the sender's static key.
//!
//! UDP keys are derived with HKDF from the handshake. The spec says "from the
//! handshake hash"; the hash alone is computable by anyone who saw the
//! handshake, so it is used as the HKDF salt and the secret Noise split keys
//! are the input key material.

use std::io::{self, Read, Write};

use crate::framing::{self, invalid, read_frame, write_frame};
use crate::keys::{StaticKeypair, derive_key};

pub const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"annex/1 session";

/// Keys and transport of an established session.
pub struct Handshake {
    pub transport: snow::StatelessTransportState,
    pub remote_static: [u8; 32],
    /// Sender → receiver video packets.
    pub video_key: [u8; 32],
    /// Sender → receiver cursor packets.
    pub cursor_key: [u8; 32],
}

fn builder(local: &StaticKeypair) -> io::Result<snow::Builder<'_>> {
    snow::Builder::new(NOISE_PARAMS.parse().expect("valid noise params"))
        .local_private_key(&local.private)
        .and_then(|b| b.prologue(PROLOGUE))
        .map_err(|e| io::Error::other(format!("noise setup: {e}")))
}

fn finish(mut hs: snow::HandshakeState) -> io::Result<Handshake> {
    let remote: [u8; 32] = hs
        .get_remote_static()
        .ok_or_else(|| invalid("peer sent no static key"))?
        .try_into()
        .map_err(|_| invalid("bad static key length"))?;
    let hash = hs.get_handshake_hash().to_vec();
    let (c1, c2) = hs.dangerously_get_raw_split();
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(&c1);
    ikm[32..].copy_from_slice(&c2);
    let video_key = derive_key(&ikm, &hash, b"annex/1 video s2r");
    let cursor_key = derive_key(&ikm, &hash, b"annex/1 cursor s2r");
    let transport = hs
        .into_stateless_transport_mode()
        .map_err(|e| io::Error::other(format!("noise split: {e}")))?;
    Ok(Handshake { transport, remote_static: remote, video_key, cursor_key })
}

/// Sender side. Writes the session preamble and runs the handshake.
pub fn initiate<S: Read + Write>(
    stream: &mut S,
    local: &StaticKeypair,
    remote_public: &[u8; 32],
) -> io::Result<Handshake> {
    framing::write_preamble(stream, framing::ConnKind::Session)?;
    let mut hs = builder(local)?
        .remote_public_key(remote_public)
        .and_then(|b| b.build_initiator())
        .map_err(|e| io::Error::other(format!("noise init: {e}")))?;
    let mut buf = vec![0u8; framing::MAX_FRAME];
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| io::Error::other(format!("noise write: {e}")))?;
    write_frame(stream, &buf[..n])?;
    let mut msg = Vec::new();
    read_frame(stream, &mut msg)?;
    hs.read_message(&msg, &mut buf)
        .map_err(|_| invalid("receiver rejected the handshake or has a different key (re-pair?)"))?;
    finish(hs)
}

/// Receiver side, after the preamble was read. `accept` decides whether the
/// sender's static key is the paired Mac.
pub fn respond<S: Read + Write>(
    stream: &mut S,
    local: &StaticKeypair,
    accept: impl Fn(&[u8; 32]) -> bool,
) -> io::Result<Handshake> {
    let mut hs = builder(local)?
        .build_responder()
        .map_err(|e| io::Error::other(format!("noise init: {e}")))?;
    let mut msg = Vec::new();
    read_frame(stream, &mut msg)?;
    let mut buf = vec![0u8; framing::MAX_FRAME];
    hs.read_message(&msg, &mut buf)
        .map_err(|_| invalid("bad handshake from sender"))?;
    let remote: [u8; 32] = hs
        .get_remote_static()
        .ok_or_else(|| invalid("sender sent no static key"))?
        .try_into()
        .map_err(|_| invalid("bad static key length"))?;
    if !accept(&remote) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "sender is not the paired Mac"));
    }
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| io::Error::other(format!("noise write: {e}")))?;
    write_frame(stream, &buf[..n])?;
    finish(hs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlMsg;
    use crate::framing::{read_preamble, secure_pair};
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn handshake_and_messages_over_tcp() {
        let recv_keys = StaticKeypair::generate();
        let send_keys = StaticKeypair::generate();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let rk = recv_keys.clone();
        let sender_pub = send_keys.public;
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            assert_eq!(read_preamble(&mut s).unwrap(), framing::ConnKind::Session);
            let hs = respond(&mut s, &rk, |k| *k == sender_pub).unwrap();
            let (mut r, mut w) = secure_pair(hs.transport, s.try_clone().unwrap(), s);
            let big = r.recv().unwrap();
            w.send(&ControlMsg::Heartbeat).unwrap();
            (hs.video_key, hs.cursor_key, big)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let hs = initiate(&mut c, &send_keys, &recv_keys.public).unwrap();
        assert_eq!(hs.remote_static, recv_keys.public);
        let (mut r, mut w) = secure_pair(hs.transport, c.try_clone().unwrap(), c);
        // Larger than one Noise message, exercises chunking.
        let big = ControlMsg::Warning { stream_id: None, text: "x".repeat(200_000) };
        w.send(&big).unwrap();
        assert_eq!(r.recv().unwrap(), ControlMsg::Heartbeat);
        let (vk, ck, got) = server.join().unwrap();
        assert_eq!(vk, hs.video_key);
        assert_eq!(ck, hs.cursor_key);
        assert_ne!(vk, ck);
        assert_eq!(got, big);
    }

    #[test]
    fn unpaired_sender_is_rejected() {
        let recv_keys = StaticKeypair::generate();
        let stranger = StaticKeypair::generate();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let rk = recv_keys.clone();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            read_preamble(&mut s).unwrap();
            respond(&mut s, &rk, |_| false).is_err()
        });
        let mut c = TcpStream::connect(addr).unwrap();
        assert!(initiate(&mut c, &stranger, &recv_keys.public).is_err());
        assert!(server.join().unwrap());
    }
}
