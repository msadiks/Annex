//! The sender engine, independent of how frames are made.
//!
//! It finds the paired receiver, runs the session, decides which streams to
//! run in which mode, and packetizes, protects, encrypts and paces the
//! encoded frames. A [`Backend`] supplies the platform part: on the Mac that
//! is virtual displays + ScreenCaptureKit + Metal + VideoToolbox (`annex-mac`);
//! the test sender plays files.

mod engine;
mod sink;
mod transport;

pub use engine::{ConnState, Engine, EngineHandle, EngineOptions, FoundReceiver, PairingState, Status, StreamStatus};
pub use sink::{CursorSink, EncodedFrame, FrameSink};

use annex_proto::control::{MacDisplay, StreamSource, VideoMode};

/// Everything a backend needs to produce one stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamRequest {
    pub stream_id: u8,
    pub monitor_id: u64,
    pub monitor_name: String,
    pub receiver_id: String,
    /// Stable virtual display slot for this monitor (identity, spec 3.3).
    pub slot: u32,
    pub source: StreamSource,
    /// Mirror only: show the Mac display through a virtual display of the
    /// picture size, so it takes the PC monitor's shape instead of being
    /// fitted with black bars.
    pub match_mirror: bool,
    /// Picture size (before Sharp doubling).
    pub width: u32,
    pub height: u32,
    pub mode: VideoMode,
    pub bitrate_kbps: u32,
}

/// Platform part of the sender. Called from the engine thread only.
pub trait Backend: Send + 'static {
    /// Displays that can be mirrored.
    fn mac_displays(&mut self) -> Vec<MacDisplay>;

    /// Starts a stream, or reconfigures one with the same id. Frames go to
    /// `sink`. A size or source change must replace the virtual display; a
    /// mode change only the encoder.
    fn start_stream(&mut self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<()>;

    /// Stops a stream and removes its virtual display ("unplug").
    fn stop_stream(&mut self, stream_id: u8);

    /// Encode the next frame as an IDR (and resend one even if nothing changes).
    fn request_idr(&mut self, stream_id: u8);

    /// New target bitrate, applied at the next frame boundary.
    fn set_bitrate(&mut self, stream_id: u8, kbps: u32);

    /// Client-side cursor on or off (off = draw the cursor into the video).
    fn set_client_cursor(&mut self, _on: bool, _sink: Option<CursorSink>) {}

    /// Chip / machine name for `Hello`.
    fn machine_name(&self) -> String;
}
