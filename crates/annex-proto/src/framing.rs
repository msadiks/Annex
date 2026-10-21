//! Stream framing for the TCP control connection.
//!
//! Raw frames are `u16` big-endian length + bytes. They carry the preamble,
//! the Noise handshake and the pairing exchange. After the handshake, logical
//! control messages are split into Noise transport messages (each at most
//! 65535 bytes on the wire); the first chunk of a message starts with its
//! total plaintext length as `u32`.

use std::io::{self, Read, Write};
use std::sync::Arc;

use crate::control::ControlMsg;

/// Largest raw frame.
pub const MAX_FRAME: usize = u16::MAX as usize;
/// Largest logical control message we accept.
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const NOISE_TAG: usize = 16;
const MAX_CHUNK_PLAINTEXT: usize = MAX_FRAME - NOISE_TAG;

/// First bytes on every control connection.
pub const PREAMBLE_MAGIC: &[u8; 4] = b"ANNX";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnKind {
    Session = 1,
    Pairing = 2,
}

pub fn write_frame<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    let mut buf = Vec::with_capacity(2 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(bytes);
    w.write_all(&buf)
}

pub fn read_frame<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    let len = u16::from_be_bytes(len) as usize;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)
}

pub fn write_preamble<W: Write>(w: &mut W, kind: ConnKind) -> io::Result<()> {
    let mut p = Vec::with_capacity(7);
    p.extend_from_slice(PREAMBLE_MAGIC);
    p.extend_from_slice(&crate::PROTO_VERSION.to_be_bytes());
    p.push(kind as u8);
    write_frame(w, &p)
}

pub fn read_preamble<R: Read>(r: &mut R) -> io::Result<ConnKind> {
    let mut buf = Vec::new();
    read_frame(r, &mut buf)?;
    if buf.len() != 7 || &buf[0..4] != PREAMBLE_MAGIC {
        return Err(invalid("not an Annex connection"));
    }
    let ver = u16::from_be_bytes([buf[4], buf[5]]);
    if ver != crate::PROTO_VERSION {
        return Err(invalid("incompatible Annex protocol version"));
    }
    match buf[6] {
        1 => Ok(ConnKind::Session),
        2 => Ok(ConnKind::Pairing),
        _ => Err(invalid("unknown connection kind")),
    }
}

pub(crate) fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Sending half of an established session. Nonces count up from zero.
pub struct SecureWriter<W: Write> {
    transport: Arc<snow::StatelessTransportState>,
    nonce: u64,
    stream: W,
    scratch: Vec<u8>,
}

/// Receiving half of an established session.
pub struct SecureReader<R: Read> {
    transport: Arc<snow::StatelessTransportState>,
    nonce: u64,
    stream: R,
    frame: Vec<u8>,
    plain: Vec<u8>,
}

pub fn secure_pair<R: Read, W: Write>(
    transport: snow::StatelessTransportState,
    reader: R,
    writer: W,
) -> (SecureReader<R>, SecureWriter<W>) {
    let t = Arc::new(transport);
    (
        SecureReader {
            transport: t.clone(),
            nonce: 0,
            stream: reader,
            frame: Vec::with_capacity(MAX_FRAME),
            plain: Vec::with_capacity(MAX_FRAME),
        },
        SecureWriter {
            transport: t,
            nonce: 0,
            stream: writer,
            scratch: vec![0u8; 2 + MAX_FRAME],
        },
    )
}

impl<W: Write> SecureWriter<W> {
    pub fn send(&mut self, msg: &ControlMsg) -> io::Result<()> {
        let body = msg.encode();
        if body.len() > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "control message too large"));
        }
        let mut plain = Vec::with_capacity(4 + body.len());
        plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
        plain.extend_from_slice(&body);
        let mut out = Vec::with_capacity(plain.len() + 64);
        for chunk in plain.chunks(MAX_CHUNK_PLAINTEXT) {
            let n = self
                .transport
                .write_message(self.nonce, chunk, &mut self.scratch[2..])
                .map_err(|e| io::Error::other(format!("noise encrypt: {e}")))?;
            self.nonce += 1;
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&self.scratch[2..2 + n]);
        }
        self.stream.write_all(&out)?;
        self.stream.flush()
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.stream
    }
}

impl<R: Read> SecureReader<R> {
    pub fn recv(&mut self) -> io::Result<ControlMsg> {
        let mut total: Option<usize> = None;
        let mut msg = Vec::new();
        loop {
            read_frame(&mut self.stream, &mut self.frame)?;
            self.plain.resize(self.frame.len(), 0);
            let n = self
                .transport
                .read_message(self.nonce, &self.frame, &mut self.plain)
                .map_err(|_| invalid("control message authentication failed"))?;
            self.nonce += 1;
            let mut chunk = &self.plain[..n];
            if total.is_none() {
                if chunk.len() < 4 {
                    return Err(invalid("short control message"));
                }
                let len = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
                if len > MAX_MESSAGE {
                    return Err(invalid("control message too large"));
                }
                total = Some(len);
                msg.reserve(len);
                chunk = &chunk[4..];
            }
            msg.extend_from_slice(chunk);
            let want = total.unwrap_or(0);
            if msg.len() > want {
                return Err(invalid("control message overrun"));
            }
            if msg.len() == want {
                return ControlMsg::decode(&msg).map_err(|e| invalid(&format!("bad control message: {e}")));
            }
        }
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.stream
    }
}
