//! The Mac [`Backend`]: one virtual display (or mirrored display), capture,
//! packer and encoder per stream.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_core::identity::virtual_display_identity;
use annex_proto::control::{MacDisplay, StreamSource, VideoMode};
use annex_sender::{Backend, CursorSink, FrameSink, StreamRequest};
use anyhow::Context;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool};

use crate::capture::Capture;
use crate::cursor::{CursorTracker, Target};
use crate::encode::Encoder;
use crate::pack::Packer;
use crate::sys::SendBox;
use crate::vdisplay::{self, VirtualDisplay};

type PixelBuffer = SendBox<CFRetained<CVPixelBuffer>>;

/// Packing + encoding for one mode. Replaced when the mode changes.
struct Pipe {
    mode: VideoMode,
    encoder: Encoder,
    pool: SendBox<CFRetained<CVPixelBufferPool>>,
    inflight: AtomicU32,
}

/// What the capture callback needs.
struct Shared {
    width: u32,
    height: u32,
    packer: Arc<Packer>,
    pipe: Mutex<Arc<Pipe>>,
    /// Last captured picture, so a keyframe can be sent while the screen is
    /// static (SCK sends nothing then).
    last: Mutex<Option<PixelBuffer>>,
}

impl Shared {
    fn process(&self, src: &CVPixelBuffer, captured: Instant) {
        let pipe = self.pipe.lock().unwrap().clone();
        pipe.encoder.note_captured();
        // Never queue up behind a slow GPU or encoder: drop instead.
        if pipe.inflight.load(Ordering::Acquire) >= 2 {
            return;
        }
        let mut dst: *mut CVPixelBuffer = std::ptr::null_mut();
        let rc = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &pipe.pool.0, NonNull::from(&mut dst)) };
        if rc != 0 || dst.is_null() {
            tracing::debug!(rc, "encoder pool empty, dropping frame");
            return;
        }
        let dst = SendBox(unsafe { CFRetained::from_raw(NonNull::new_unchecked(dst)) });
        let src_keep = SendBox(unsafe { CFRetained::retain(NonNull::from(src)) });
        pipe.inflight.fetch_add(1, Ordering::AcqRel);
        let started = Instant::now();
        let p = pipe.clone();
        let dst_for_gpu = SendBox(dst.0.clone());
        let r = self.packer.pack(pipe.mode, src, self.width as usize, self.height as usize, &dst.0, move || {
            let (_src, dst) = (&src_keep, &dst_for_gpu);
            let pack_ms = started.elapsed().as_secs_f64() * 1e3;
            p.encoder.encode(&dst.0, captured, pack_ms);
            p.inflight.fetch_sub(1, Ordering::AcqRel);
        });
        if let Err(e) = r {
            pipe.inflight.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!("pack failed: {e:#}");
            return;
        }
        *self.last.lock().unwrap() = Some(SendBox(unsafe { CFRetained::retain(NonNull::from(src)) }));
    }

    /// Re-sends the last picture (as a keyframe if one was requested).
    fn resend(&self) {
        let last = self.last.lock().unwrap().as_ref().map(|b| SendBox(b.0.clone()));
        let idle = self.pipe.lock().unwrap().inflight.load(Ordering::Acquire) == 0;
        if let (Some(src), true) = (last, idle) {
            self.process(&src.0, Instant::now());
        }
    }
}

struct MacStream {
    req: StreamRequest,
    display_id: u32,
    shared: Arc<Shared>,
    capture: Option<Capture>,
    vdisplay: Option<VirtualDisplay>,
}

pub struct MacBackend {
    packer: Arc<Packer>,
    streams: HashMap<u8, MacStream>,
    client_cursor: bool,
    cursor_sink: Option<CursorSink>,
    cursor: Option<CursorTracker>,
    /// Identity → display id of virtual displays that may still be going away.
    retired: HashMap<(u32, u32, u32), u32>,
    machine: String,
}

impl MacBackend {
    pub fn new() -> anyhow::Result<Self> {
        Ok(MacBackend {
            packer: Arc::new(Packer::new()?),
            streams: HashMap::new(),
            client_cursor: false,
            cursor_sink: None,
            cursor: None,
            retired: HashMap::new(),
            machine: crate::sys::machine_name(),
        })
    }

    fn new_pipe(&self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<Arc<Pipe>> {
        let (w, h) = match req.mode {
            VideoMode::Sharp => (req.width * 2, req.height * 2),
            VideoMode::Fast => (req.width, req.height),
        };
        let encoder = Encoder::new(w, h, req.bitrate_kbps, sink)?;
        let pool = encoder.pool().context("the encoder has no pixel buffer pool")?;
        Ok(Arc::new(Pipe { mode: req.mode, encoder, pool: SendBox(pool), inflight: AtomicU32::new(0) }))
    }

    fn cursor_targets(&self) -> HashMap<u8, Target> {
        self.streams
            .iter()
            .map(|(id, s)| (*id, Target { display_id: s.display_id, width: s.req.width, height: s.req.height }))
            .collect()
    }

    fn update_cursor(&mut self) {
        let targets = self.cursor_targets();
        match (&self.cursor, self.client_cursor, &self.cursor_sink) {
            (Some(c), true, _) => c.set_targets(targets),
            (None, true, Some(sink)) => self.cursor = Some(CursorTracker::start(sink.clone(), targets)),
            (_, false, _) => self.cursor = None,
            _ => {}
        }
    }

    fn create(&mut self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<MacStream> {
        let (display_id, vdisplay) = match &req.source {
            StreamSource::Extend => {
                let identity = virtual_display_identity(&req.receiver_id, req.slot);
                if let Some(old) = self.retired.remove(&identity) {
                    vdisplay::wait_gone(old, Duration::from_secs(2));
                }
                let vd = VirtualDisplay::create(&format!("Annex: {}", req.monitor_name), req.width, req.height, identity)?;
                (vd.id, Some(vd))
            }
            StreamSource::Mirror { display_uuid } => {
                let id = crate::displays::id_of(display_uuid).context("the Mac display to mirror is not connected")?;
                (id, None)
            }
        };
        let pipe = self.new_pipe(req, sink)?;
        let shared = Arc::new(Shared {
            width: req.width,
            height: req.height,
            packer: self.packer.clone(),
            pipe: Mutex::new(pipe),
            last: Mutex::new(None),
        });
        let cb = shared.clone();
        let capture = Capture::start(
            display_id,
            req.width,
            req.height,
            !self.client_cursor,
            Box::new(move |buf, t| cb.process(buf, t)),
        )?;
        Ok(MacStream { req: req.clone(), display_id, shared, capture: Some(capture), vdisplay })
    }
}

fn same_display(a: &StreamRequest, b: &StreamRequest) -> bool {
    a.source == b.source && a.width == b.width && a.height == b.height && a.slot == b.slot && a.receiver_id == b.receiver_id
}

impl Backend for MacBackend {
    fn mac_displays(&mut self) -> Vec<MacDisplay> {
        crate::displays::list()
    }

    fn start_stream(&mut self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<()> {
        if let Some(s) = self.streams.get(&req.stream_id)
            && same_display(&s.req, req)
        {
            if s.req.mode == req.mode {
                // Reconnect: same display, new session.
                let pipe = s.shared.pipe.lock().unwrap().clone();
                pipe.encoder.set_bitrate(req.bitrate_kbps);
                pipe.encoder.set_sink(sink);
            } else {
                // Sharp ↔ Fast: new packer/encoder setup, same display.
                let pipe = self.new_pipe(req, sink)?;
                *s.shared.pipe.lock().unwrap() = pipe;
            }
            let shared = s.shared.clone();
            self.streams.get_mut(&req.stream_id).unwrap().req = req.clone();
            shared.resend();
            return Ok(());
        }
        self.stop_stream(req.stream_id);
        let s = self.create(req, sink)?;
        self.streams.insert(req.stream_id, s);
        self.update_cursor();
        Ok(())
    }

    fn stop_stream(&mut self, stream_id: u8) {
        if let Some(mut s) = self.streams.remove(&stream_id) {
            s.capture.take();
            if let Some(vd) = s.vdisplay.take() {
                self.retired.insert(virtual_display_identity(&s.req.receiver_id, s.req.slot), vd.id);
            }
            self.update_cursor();
        }
    }

    fn request_idr(&mut self, stream_id: u8) {
        if let Some(s) = self.streams.get(&stream_id) {
            s.shared.pipe.lock().unwrap().encoder.request_key();
            s.shared.resend();
        }
    }

    fn set_bitrate(&mut self, stream_id: u8, kbps: u32) {
        if let Some(s) = self.streams.get(&stream_id) {
            s.shared.pipe.lock().unwrap().encoder.set_bitrate(kbps);
        }
    }

    fn set_client_cursor(&mut self, on: bool, sink: Option<CursorSink>) {
        self.client_cursor = on;
        self.cursor_sink = sink;
        self.cursor = None;
        self.update_cursor();
        for s in self.streams.values() {
            if let Some(c) = &s.capture {
                c.set_shows_cursor(!on);
            }
        }
    }

    fn machine_name(&self) -> String {
        self.machine.clone()
    }
}
