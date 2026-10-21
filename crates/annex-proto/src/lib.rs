//! Annex wire protocol.
//!
//! Everything both sides must agree on lives here: the UDP video packet format,
//! Reed-Solomon FEC, per-packet AEAD, the packetizer and reassembler, control
//! messages and their framing, the Noise session handshake, PIN pairing, mDNS
//! discovery and a lossy-channel simulator used by the tests.
//!
//! This crate is platform independent and must stay that way: it is unit-tested
//! in CI on Linux, macOS and Windows.

pub mod addr;
pub mod control;
pub mod crypto;
pub mod cursor;
pub mod discovery;
pub mod fec;
pub mod framing;
pub mod keys;
pub mod noise;
pub mod packet;
pub mod packetizer;
pub mod pairing;
pub mod reassembler;
pub mod sim;

/// Version of the wire protocol. Bumped on any incompatible change.
pub const PROTO_VERSION: u16 = 1;

/// Version of the Annex application, sent in `Hello`.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default TCP control port on the receiver.
pub const DEFAULT_CONTROL_PORT: u16 = 48800;

/// Default UDP video port on the receiver.
pub const DEFAULT_VIDEO_PORT: u16 = 48801;

/// mDNS service type advertised by the receiver.
pub const SERVICE_TYPE: &str = "_annex._tcp.local.";

/// Maximum number of concurrent streams (= Windows monitors driven by Annex).
pub const MAX_STREAMS: u8 = 3;

pub use control::ControlMsg;
pub use packet::{VideoHeader, HEADER_LEN, MAX_PAYLOAD, MAX_SHARD};
