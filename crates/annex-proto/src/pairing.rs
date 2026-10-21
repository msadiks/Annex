//! One-time PIN pairing (spec 8.2).
//!
//! The receiver shows a 6-digit PIN; the user types it on the Mac. Both run
//! SPAKE2 over the PIN, which yields a shared key only if the PINs match and
//! gives an eavesdropper nothing to brute-force offline. Under that key each
//! side sends its long-term X25519 public key and id. A wrong PIN shows up as
//! an authentication failure of the first sealed message.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::crypto::{open_message, seal_message};
use crate::framing::{self, invalid, read_frame, write_frame};
use crate::keys::derive_key;

const ID_SENDER: &[u8] = b"annex-sender";
const ID_RECEIVER: &[u8] = b"annex-receiver";

/// What each side stores about its peer after pairing.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub id: String,
    pub name: String,
    #[serde(with = "crate::keys::hex32")]
    pub public_key: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug)]
enum PairMsg {
    Request { sender: String, spake: Vec<u8> },
    Response { receiver: String, spake: Vec<u8> },
    Reject { reason: String },
    SenderProof { sealed: Vec<u8> },
    ReceiverProof { sealed: Vec<u8> },
    WrongPin,
}

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("wrong PIN")]
    WrongPin,
    #[error("pairing refused: {0}")]
    Rejected(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn send<W: Write>(w: &mut W, m: &PairMsg) -> io::Result<()> {
    write_frame(w, &postcard::to_allocvec(m).expect("serializable"))
}

fn recv<R: Read>(r: &mut R) -> io::Result<PairMsg> {
    let mut buf = Vec::new();
    read_frame(r, &mut buf)?;
    postcard::from_bytes(&buf).map_err(|_| invalid("bad pairing message"))
}

/// Normalizes user input: keeps digits only.
pub fn normalize_pin(pin: &str) -> String {
    pin.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Generates a random 6-digit PIN.
pub fn random_pin() -> String {
    let n = u32::from_le_bytes(crate::keys::random_bytes::<4>()) % 1_000_000;
    format!("{n:06}")
}

fn session_keys(k: &[u8], transcript: &[u8]) -> ([u8; 32], [u8; 32]) {
    (
        derive_key(k, transcript, b"annex/1 pair s2r"),
        derive_key(k, transcript, b"annex/1 pair r2s"),
    )
}

fn transcript(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut t = Vec::with_capacity(a.len() + b.len() + 8);
    t.extend_from_slice(&(a.len() as u32).to_be_bytes());
    t.extend_from_slice(a);
    t.extend_from_slice(&(b.len() as u32).to_be_bytes());
    t.extend_from_slice(b);
    t
}

/// Mac side: connect, prove knowledge of the PIN, exchange identities.
pub fn pair_as_sender<S: Read + Write>(
    stream: &mut S,
    pin: &str,
    me: &PeerIdentity,
) -> Result<PeerIdentity, PairError> {
    framing::write_preamble(stream, framing::ConnKind::Pairing)?;
    let (state, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(normalize_pin(pin).as_bytes()),
        &Identity::new(ID_SENDER),
        &Identity::new(ID_RECEIVER),
    );
    send(stream, &PairMsg::Request { sender: me.name.clone(), spake: msg_a.clone() })?;
    let msg_b = match recv(stream)? {
        PairMsg::Response { spake, .. } => spake,
        PairMsg::Reject { reason } => return Err(PairError::Rejected(reason)),
        _ => return Err(invalid("unexpected pairing message").into()),
    };
    let k = state.finish(&msg_b).map_err(|_| invalid("bad SPAKE2 message"))?;
    let t = transcript(&msg_a, &msg_b);
    let (s2r, r2s) = session_keys(&k, &t);
    let mine = postcard::to_allocvec(me).expect("serializable");
    send(stream, &PairMsg::SenderProof { sealed: seal_message(&s2r, &[0; 12], &t, &mine) })?;
    match recv(stream)? {
        PairMsg::ReceiverProof { sealed } => {
            let plain = open_message(&r2s, &[0; 12], &t, &sealed).map_err(|_| PairError::WrongPin)?;
            postcard::from_bytes(&plain).map_err(|_| invalid("bad receiver identity").into())
        }
        PairMsg::WrongPin => Err(PairError::WrongPin),
        PairMsg::Reject { reason } => Err(PairError::Rejected(reason)),
        _ => Err(invalid("unexpected pairing message").into()),
    }
}

/// Windows side, after the preamble was read. `pin` is `None` when the
/// receiver is not in pairing mode.
pub fn pair_as_receiver<S: Read + Write>(
    stream: &mut S,
    pin: Option<&str>,
    me: &PeerIdentity,
) -> Result<PeerIdentity, PairError> {
    let msg_a = match recv(stream)? {
        PairMsg::Request { spake, .. } => spake,
        _ => return Err(invalid("unexpected pairing message").into()),
    };
    let Some(pin) = pin else {
        let reason = "the PC is not in pairing mode; choose Pair… in its tray menu".to_string();
        send(stream, &PairMsg::Reject { reason: reason.clone() })?;
        return Err(PairError::Rejected(reason));
    };
    let (state, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(normalize_pin(pin).as_bytes()),
        &Identity::new(ID_SENDER),
        &Identity::new(ID_RECEIVER),
    );
    send(stream, &PairMsg::Response { receiver: me.name.clone(), spake: msg_b.clone() })?;
    let k = state.finish(&msg_a).map_err(|_| invalid("bad SPAKE2 message"))?;
    let t = transcript(&msg_a, &msg_b);
    let (s2r, r2s) = session_keys(&k, &t);
    let sealed = match recv(stream)? {
        PairMsg::SenderProof { sealed } => sealed,
        _ => return Err(invalid("unexpected pairing message").into()),
    };
    let Ok(plain) = open_message(&s2r, &[0; 12], &t, &sealed) else {
        let _ = send(stream, &PairMsg::WrongPin);
        return Err(PairError::WrongPin);
    };
    let peer: PeerIdentity = postcard::from_bytes(&plain).map_err(|_| invalid("bad sender identity"))?;
    let mine = postcard::to_allocvec(me).expect("serializable");
    send(stream, &PairMsg::ReceiverProof { sealed: seal_message(&r2s, &[0; 12], &t, &mine) })?;
    Ok(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::read_preamble;
    use std::net::{TcpListener, TcpStream};

    fn ident(name: &str) -> PeerIdentity {
        PeerIdentity { id: name.into(), name: name.into(), public_key: crate::keys::random_bytes() }
    }

    fn run(pin_sender: &str, pin_receiver: Option<&'static str>) -> (Result<PeerIdentity, PairError>, Result<PeerIdentity, PairError>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_id = ident("pc");
        let ri = recv_id.clone();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            read_preamble(&mut s).unwrap();
            pair_as_receiver(&mut s, pin_receiver, &ri)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let r = pair_as_sender(&mut c, pin_sender, &ident("mac"));
        (r, server.join().unwrap())
    }

    #[test]
    fn matching_pins_exchange_identities() {
        let (s, r) = run("123 456", Some("123456"));
        assert_eq!(s.unwrap().name, "pc");
        assert_eq!(r.unwrap().name, "mac");
    }

    #[test]
    fn wrong_pin_fails_on_both_sides() {
        let (s, r) = run("123457", Some("123456"));
        assert!(matches!(s, Err(PairError::WrongPin)));
        assert!(matches!(r, Err(PairError::WrongPin)));
    }

    #[test]
    fn not_in_pairing_mode_is_rejected() {
        let (s, _) = run("123456", None);
        assert!(matches!(s, Err(PairError::Rejected(_))));
    }
}
