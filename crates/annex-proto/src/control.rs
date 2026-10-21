//! Control-channel messages (spec 5.4 and 8.4).
//!
//! Serialized with `postcard` and carried inside the Noise transport on TCP.
//! New variants must be appended at the end to keep older peers decoding the
//! ones they know.

use serde::{Deserialize, Serialize};

/// Codec packing of a stream.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoMode {
    /// Exact 4:4:4 carried as a doubled 4:2:0 frame (spec 4.1).
    Sharp,
    /// Native 4:2:0 (spec 4.2).
    Fast,
}

impl VideoMode {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoMode::Sharp => "Sharp",
            VideoMode::Fast => "Fast",
        }
    }
}

/// What the user asked for on a monitor.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModePref {
    #[default]
    Auto,
    Sharp,
    Fast,
}

impl ModePref {
    pub fn pinned(self) -> Option<VideoMode> {
        match self {
            ModePref::Auto => None,
            ModePref::Sharp => Some(VideoMode::Sharp),
            ModePref::Fast => Some(VideoMode::Fast),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ModePref::Auto => "Auto",
            ModePref::Sharp => "Sharp",
            ModePref::Fast => "Fast",
        }
    }
}

/// Which Mac picture a monitor shows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum SourcePref {
    /// A new virtual display.
    #[default]
    Extend,
    /// Mirror of an existing Mac display, identified by its CoreGraphics UUID.
    Mirror { display_uuid: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub proto_version: u16,
    pub app_version: String,
    pub os: String,
    pub host_name: String,
    /// Receiver: GPU used for decoding. Sender: chip name.
    pub gpu_name: String,
    /// Receiver: D3D11VA HEVC Main decode is available.
    pub hevc_decode_ok: bool,
    pub max_streams: u8,
}

/// A Windows monitor as reported by the receiver: its current desktop mode is
/// the "EDID" the Mac sees.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable id (hash of friendly name + EDID identity / device path).
    pub id: u64,
    pub name: String,
    pub native_w: u32,
    pub native_h: u32,
    pub refresh_mhz: u32,
    pub enabled: bool,
    /// Lower is more important. The first enabled monitor gets Sharp first.
    pub priority: u8,
    pub pinned_mode: ModePref,
    pub source: SourcePref,
    pub primary: bool,
}

/// A Mac display that can be mirrored.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MacDisplay {
    pub uuid: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub builtin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum StreamSource {
    Extend,
    Mirror { display_uuid: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub stream_id: u8,
    pub monitor_id: u64,
    pub source: StreamSource,
    /// Picture size in pixels (before Sharp doubling).
    pub stream_w: u32,
    pub stream_h: u32,
    pub mode: VideoMode,
    pub bitrate_kbps: u32,
    pub fec_percent: u8,
}

/// Receiver → sender, once per second per stream.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ReceiverStreamStats {
    pub stream_id: u8,
    pub packets_received: u32,
    pub packets_lost: u32,
    pub frames_presented: u32,
    pub frames_repaired: u32,
    pub frames_lost: u32,
    pub frames_skipped: u32,
    pub bytes_received: u64,
    pub decode_ms: f32,
    pub render_ms: f32,
    /// Estimated capture → present latency, negative if unknown.
    pub e2e_ms: f32,
    /// First packet → frame complete.
    pub network_ms: f32,
    pub queue_depth: u8,
}

/// Sender → receiver, once per second per stream (for the stats overlay).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SenderStreamStats {
    pub stream_id: u8,
    pub fps_captured: f32,
    pub fps_sent: f32,
    pub pack_ms: f32,
    pub encode_ms: f32,
    pub send_ms: f32,
    pub bitrate_kbps: u32,
    pub fec_percent: u8,
    pub mode: Option<VideoMode>,
    /// Mode was lowered by the encoder budget.
    pub downgraded: bool,
}

/// A cursor image. Positions travel separately over UDP (see [`crate::cursor`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub shape_id: u32,
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Straight (non-premultiplied) RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ControlMsg {
    Hello(Hello),
    /// Receiver → sender: full monitor list, on connect and on any change.
    Monitors(Vec<MonitorInfo>),
    /// Sender → receiver: displays available for mirroring.
    MacDisplays(Vec<MacDisplay>),
    /// Sender → receiver: start or reconfigure a stream.
    StreamConfig(StreamConfig),
    /// Receiver → sender: decoder and window for the stream are ready.
    StreamReady { stream_id: u8 },
    /// Sender → receiver: stream ended (display unplugged).
    StreamStop { stream_id: u8 },
    /// Sender → receiver: Sharp ↔ Fast switch; takes effect at the next IDR.
    ModeChange { stream_id: u8, mode: VideoMode },
    /// Receiver → sender: frames were lost, send a keyframe.
    IdrRequest { stream_id: u8, last_good_seq: u32 },
    Stats(Vec<ReceiverStreamStats>),
    SenderStats(Vec<SenderStreamStats>),
    Heartbeat,
    /// NTP-style clock sync. `t1` is the requester's clock (µs).
    ClockPing { t1: u64 },
    /// `t2` = responder receive time, `t3` = responder send time (responder clock, µs).
    ClockPong { t1: u64, t2: u64, t3: u64 },
    CursorShape(CursorShape),
    /// Sender → receiver: user changed a monitor setting from the Mac menu.
    SetMonitorPrefs {
        monitor_id: u64,
        enabled: Option<bool>,
        pinned_mode: Option<ModePref>,
        source: Option<SourcePref>,
    },
    /// A condition worth showing the user (e.g. pinned Sharp can't keep 60 fps).
    Warning { stream_id: Option<u8>, text: String },
    /// Orderly shutdown: unplug everything now, no grace period.
    Bye,
}

impl ControlMsg {
    pub fn kind(&self) -> &'static str {
        match self {
            ControlMsg::Hello(_) => "Hello",
            ControlMsg::Monitors(_) => "Monitors",
            ControlMsg::MacDisplays(_) => "MacDisplays",
            ControlMsg::StreamConfig(_) => "StreamConfig",
            ControlMsg::StreamReady { .. } => "StreamReady",
            ControlMsg::StreamStop { .. } => "StreamStop",
            ControlMsg::ModeChange { .. } => "ModeChange",
            ControlMsg::IdrRequest { .. } => "IdrRequest",
            ControlMsg::Stats(_) => "Stats",
            ControlMsg::SenderStats(_) => "SenderStats",
            ControlMsg::Heartbeat => "Heartbeat",
            ControlMsg::ClockPing { .. } => "ClockPing",
            ControlMsg::ClockPong { .. } => "ClockPong",
            ControlMsg::CursorShape(_) => "CursorShape",
            ControlMsg::SetMonitorPrefs { .. } => "SetMonitorPrefs",
            ControlMsg::Warning { .. } => "Warning",
            ControlMsg::Bye => "Bye",
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("control messages always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_variants() {
        let msgs = vec![
            ControlMsg::Hello(Hello {
                proto_version: 1,
                app_version: "1.0.0".into(),
                os: "Windows 11".into(),
                host_name: "pc".into(),
                gpu_name: "RTX".into(),
                hevc_decode_ok: true,
                max_streams: 3,
            }),
            ControlMsg::Monitors(vec![MonitorInfo {
                id: 42,
                name: "DELL U2720Q".into(),
                native_w: 3840,
                native_h: 2160,
                refresh_mhz: 60_000,
                enabled: true,
                priority: 0,
                pinned_mode: ModePref::Sharp,
                source: SourcePref::Mirror { display_uuid: "abc".into() },
                primary: true,
            }]),
            ControlMsg::MacDisplays(vec![MacDisplay {
                uuid: "u".into(),
                name: "Built-in".into(),
                width: 3024,
                height: 1964,
                builtin: true,
            }]),
            ControlMsg::StreamConfig(StreamConfig {
                stream_id: 1,
                monitor_id: 42,
                source: StreamSource::Extend,
                stream_w: 1920,
                stream_h: 1080,
                mode: VideoMode::Sharp,
                bitrate_kbps: 80_000,
                fec_percent: 10,
            }),
            ControlMsg::StreamReady { stream_id: 1 },
            ControlMsg::StreamStop { stream_id: 1 },
            ControlMsg::ModeChange { stream_id: 1, mode: VideoMode::Fast },
            ControlMsg::IdrRequest { stream_id: 1, last_good_seq: 77 },
            ControlMsg::Stats(vec![ReceiverStreamStats { stream_id: 1, e2e_ms: 31.5, ..Default::default() }]),
            ControlMsg::SenderStats(vec![SenderStreamStats { stream_id: 1, mode: Some(VideoMode::Sharp), ..Default::default() }]),
            ControlMsg::Heartbeat,
            ControlMsg::ClockPing { t1: 1 },
            ControlMsg::ClockPong { t1: 1, t2: 2, t3: 3 },
            ControlMsg::CursorShape(CursorShape { shape_id: 3, width: 1, height: 1, hot_x: 0, hot_y: 0, rgba: vec![1, 2, 3, 4] }),
            ControlMsg::SetMonitorPrefs { monitor_id: 42, enabled: Some(false), pinned_mode: None, source: Some(SourcePref::Extend) },
            ControlMsg::Warning { stream_id: Some(1), text: "slow".into() },
            ControlMsg::Bye,
        ];
        for m in msgs {
            let b = m.encode();
            assert_eq!(ControlMsg::decode(&b).unwrap(), m, "{}", m.kind());
        }
    }

    #[test]
    fn variant_tags_are_stable() {
        // postcard encodes the variant index first; peers rely on these staying put.
        assert_eq!(ControlMsg::Heartbeat.encode(), vec![10]);
        assert_eq!(ControlMsg::Bye.encode(), vec![16]);
    }
}
