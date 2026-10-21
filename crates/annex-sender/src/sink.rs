//! Where backends hand over encoded frames and cursor updates.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use annex_core::stats::{Mean, us_since};
use annex_proto::control::CursorShape;
use crossbeam_channel::{Receiver, Sender, TrySendError};

/// One encoded access unit on its way to the packetizer.
pub struct EncodedFrame {
    pub stream_id: u8,
    pub data: Vec<u8>,
    pub idr: bool,
    /// µs on the engine clock.
    pub capture_us: u64,
}

#[derive(Default)]
pub(crate) struct StreamMeters {
    pub captured: u32,
    pub sent: u32,
    pub dropped: u32,
    pub pack_ms: Mean,
    pub encode_ms: Mean,
}

/// Shared between the engine and all sinks.
pub(crate) struct SinkShared {
    pub epoch: Instant,
    pub tx: Sender<EncodedFrame>,
    pub pool: Receiver<Vec<u8>>,
    pub meters: Mutex<HashMap<u8, StreamMeters>>,
}

/// Cloneable handle a backend pushes one stream's frames into. Safe to call
/// from any thread (e.g. a VideoToolbox output callback).
#[derive(Clone)]
pub struct FrameSink {
    pub(crate) stream_id: u8,
    pub(crate) shared: Arc<SinkShared>,
    /// False until the receiver said `StreamReady`, and while disconnected.
    pub(crate) live: Arc<AtomicBool>,
    pub(crate) generation: Arc<AtomicU32>,
    pub(crate) my_generation: u32,
}

impl FrameSink {
    pub fn stream_id(&self) -> u8 {
        self.stream_id
    }

    /// The engine clock, for capture timestamps.
    pub fn epoch(&self) -> Instant {
        self.shared.epoch
    }

    /// False once the engine replaced or stopped this stream: the backend
    /// should stop producing.
    pub fn is_current(&self) -> bool {
        self.generation.load(Ordering::Relaxed) == self.my_generation
    }

    /// A frame was captured (counts input fps even if it is not encoded).
    pub fn note_captured(&self) {
        if let Some(m) = self.shared.meters.lock().unwrap().get_mut(&self.stream_id) {
            m.captured += 1;
        }
    }

    /// Hands over an encoded access unit (Annex B). Copies into a pooled buffer.
    pub fn push(&self, au: &[u8], idr: bool, captured_at: Instant, pack_ms: f64, encode_ms: f64) {
        {
            let mut meters = self.shared.meters.lock().unwrap();
            let m = meters.entry(self.stream_id).or_default();
            m.pack_ms.add(pack_ms);
            m.encode_ms.add(encode_ms);
        }
        if !self.live.load(Ordering::Acquire) || !self.is_current() {
            return;
        }
        let mut buf = self.shared.pool.try_recv().unwrap_or_else(|_| Vec::with_capacity(512 * 1024));
        buf.clear();
        buf.extend_from_slice(au);
        let frame = EncodedFrame {
            stream_id: self.stream_id,
            data: buf,
            idr,
            capture_us: us_since(self.shared.epoch, captured_at),
        };
        let ok = match self.shared.tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        };
        let mut meters = self.shared.meters.lock().unwrap();
        let m = meters.entry(self.stream_id).or_default();
        if ok {
            m.sent += 1;
        } else {
            // The receiver will notice the gap and ask for an IDR.
            m.dropped += 1;
        }
    }
}

pub(crate) enum CursorEvent {
    Position { stream_id: u8, x: i32, y: i32, visible: bool, shape_id: u32 },
    Shape(CursorShape),
}

/// Cursor updates from the backend (client-side cursor, spec phase 3).
#[derive(Clone)]
pub struct CursorSink {
    pub(crate) tx: Sender<CursorEvent>,
}

impl CursorSink {
    /// Hotspot position in stream pixels of `stream_id`.
    pub fn position(&self, stream_id: u8, x: i32, y: i32, visible: bool, shape_id: u32) {
        let _ = self.tx.try_send(CursorEvent::Position { stream_id, x, y, visible, shape_id });
    }

    /// A new cursor image. Send each `shape_id` once; the engine re-sends
    /// known shapes after a reconnect.
    pub fn shape(&self, shape: CursorShape) {
        let _ = self.tx.try_send(CursorEvent::Shape(shape));
    }
}
