//! The Mac [`Backend`]: one virtual display (or mirrored display), capture,
//! packer and encoder per stream.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_core::identity::{mirror_display_identity, virtual_display_identity};
use annex_proto::control::{MacDisplay, StreamSource, VideoMode};
use annex_sender::{Backend, CursorSink, FrameSink, StreamRequest};
use anyhow::Context;
use dispatch2::{DispatchQueue, DispatchRetained, DispatchTime};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool};

use crate::capture::Capture;
use crate::cursor::{CursorTracker, Target};
use crate::displays::Mirrored;
use crate::encode::Encoder;
use crate::pack::Packer;
use crate::sys::SendBox;
use crate::vdisplay::{self, VirtualDisplay};

type PixelBuffer = SendBox<CFRetained<CVPixelBuffer>>;

/// Frames closer together than this wait for the gap to pass. SCK is asked
/// for up to 120 fps (see capture.rs); this keeps a 120 Hz display at ~60
/// while frame jitter on a 60 Hz display never delays anything.
const MIN_GAP: Duration = Duration::from_millis(12);

#[derive(Default)]
struct Pace {
    last: Option<Instant>,
    /// Newest frame that came too soon, sent when the gap has passed.
    held: Option<(PixelBuffer, Instant)>,
    armed: bool,
}

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
    /// Newest frame that arrived while the pipe was busy. Sent when a frame
    /// finishes, so the last change before the screen goes static isn't lost.
    pending: Mutex<Option<(PixelBuffer, Instant)>>,
    pace: Mutex<Pace>,
    timer: DispatchRetained<DispatchQueue>,
}

impl Shared {
    fn on_capture(self: &Arc<Self>, src: &CVPixelBuffer, captured: Instant) {
        let now = Instant::now();
        let mut pace = self.pace.lock().unwrap();
        let wait = pace.last.map(|t| (t + MIN_GAP).saturating_duration_since(now)).unwrap_or_default();
        if !wait.is_zero() {
            pace.held = Some((SendBox(unsafe { CFRetained::retain(NonNull::from(src)) }), captured));
            if !pace.armed {
                pace.armed = true;
                let this = self.clone();
                let when = DispatchTime::try_from(wait).unwrap_or(DispatchTime::NOW);
                let _ = self.timer.after(when, move || this.send_held());
            }
            return;
        }
        pace.last = Some(now);
        pace.held = None;
        drop(pace);
        self.pipe.lock().unwrap().encoder.note_captured();
        self.process(src, captured);
    }

    fn send_held(self: &Arc<Self>) {
        let mut pace = self.pace.lock().unwrap();
        pace.armed = false;
        if let Some((buf, captured)) = pace.held.take() {
            pace.last = Some(Instant::now());
            drop(pace);
            self.pipe.lock().unwrap().encoder.note_captured();
            self.process(&buf.0, captured);
        }
    }

    fn process(self: &Arc<Self>, src: &CVPixelBuffer, captured: Instant) {
        let pipe = self.pipe.lock().unwrap().clone();
        // Never queue up behind a slow GPU or encoder: keep only the newest.
        if pipe.inflight.load(Ordering::Acquire) >= 2 {
            *self.pending.lock().unwrap() = Some((SendBox(unsafe { CFRetained::retain(NonNull::from(src)) }), captured));
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
        let this = self.clone();
        let dst_for_gpu = SendBox(dst.0.clone());
        let r = self.packer.pack(pipe.mode, src, self.width as usize, self.height as usize, &dst.0, move || {
            let (_src, dst) = (&src_keep, &dst_for_gpu);
            let pack_ms = started.elapsed().as_secs_f64() * 1e3;
            p.encoder.encode(&dst.0, captured, pack_ms);
            p.inflight.fetch_sub(1, Ordering::AcqRel);
            let next = this.pending.lock().unwrap().take();
            if let Some((buf, t)) = next {
                this.process(&buf.0, t);
            }
        });
        if let Err(e) = r {
            pipe.inflight.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!("pack failed: {e:#}");
            return;
        }
        *self.last.lock().unwrap() = Some(SendBox(unsafe { CFRetained::retain(NonNull::from(src)) }));
    }

    /// Re-sends the last picture (as a keyframe if one was requested).
    fn resend(self: &Arc<Self>) {
        let last = self.last.lock().unwrap().as_ref().map(|b| SendBox(b.0.clone()));
        let idle = self.pipe.lock().unwrap().inflight.load(Ordering::Acquire) == 0;
        if let (Some(src), true) = (last, idle) {
            self.process(&src.0, Instant::now());
        }
    }
}

struct MacStream {
    req: StreamRequest,
    /// The display being captured.
    display_id: u32,
    shared: Arc<Shared>,
    capture: Option<Capture>,
    vdisplay: Option<VirtualDisplay>,
    /// Mac display made to mirror `vdisplay` (mirror matched to the PC
    /// monitor's shape).
    mirrored: Option<Mirrored>,
}

/// What macOS remembers a stream's virtual display by.
fn identity(req: &StreamRequest) -> (u32, u32, u32) {
    if req.match_mirror {
        mirror_display_identity(&req.receiver_id, req.slot)
    } else {
        virtual_display_identity(&req.receiver_id, req.slot)
    }
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

    fn new_vdisplay(&mut self, req: &StreamRequest, name: String) -> anyhow::Result<VirtualDisplay> {
        let identity = identity(req);
        if let Some(old) = self.retired.remove(&identity) {
            vdisplay::wait_gone(old, Duration::from_secs(2));
        }
        VirtualDisplay::create_distinct(&name, req.width, req.height, identity)
    }

    fn create(&mut self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<MacStream> {
        let mut mirrored = None;
        let (display_id, vdisplay) = match &req.source {
            StreamSource::Extend => {
                let vd = self.new_vdisplay(req, format!("Annex: {}", req.monitor_name))?;
                (vd.id, Some(vd))
            }
            StreamSource::Mirror { display_uuid } => {
                let id = crate::displays::id_of(display_uuid).context("the Mac display to mirror is not connected")?;
                // Another stream may already match this Mac display to its
                // monitor; then this one shows that stream's picture. What
                // macOS reports the display mirrors is not trusted for this:
                // it can be left over from an earlier stream.
                let matched_by =
                    self.streams.values().find(|s| s.mirrored.is_some_and(|m| m.display == id)).map(|s| s.display_id);
                match matched_by {
                    None if req.match_mirror => {
                        // The Mac display mirrors a virtual display of the PC
                        // monitor's shape, so the picture fills the monitor.
                        let vd = self.new_vdisplay(req, format!("Annex mirror: {}", req.monitor_name))?;
                        mirrored = Some(Mirrored::start(id, vd.id)?);
                        (vd.id, Some(vd))
                    }
                    Some(shown) => (shown, None),
                    None => (crate::displays::content_of(id), None),
                }
            }
        };
        let pipe = self.new_pipe(req, sink)?;
        let shared = Arc::new(Shared {
            width: req.width,
            height: req.height,
            packer: self.packer.clone(),
            pipe: Mutex::new(pipe),
            last: Mutex::new(None),
            pending: Mutex::new(None),
            pace: Mutex::new(Pace::default()),
            timer: DispatchQueue::new("annex.pace", None),
        });
        let cb = shared.clone();
        let capture = Capture::start(
            display_id,
            req.width,
            req.height,
            !self.client_cursor,
            Box::new(move |buf, t| cb.on_capture(buf, t)),
        )?;
        Ok(MacStream { req: req.clone(), display_id, shared, capture: Some(capture), vdisplay, mirrored })
    }

    /// Points plain mirrors of a Mac display at whatever shows its picture
    /// now, after a stream started or stopped matching it to a PC monitor.
    fn recapture_mirrors_of(&mut self, display_uuid: &str) {
        let Some(id) = crate::displays::id_of(display_uuid) else { return };
        let target = crate::displays::content_of(id);
        let shows_cursor = !self.client_cursor;
        for (stream_id, s) in self.streams.iter_mut() {
            let plain_mirror = s.vdisplay.is_none()
                && matches!(&s.req.source, StreamSource::Mirror { display_uuid: u } if u == display_uuid);
            if !plain_mirror || s.display_id == target {
                continue;
            }
            if let Err(e) = s.recapture(target, shows_cursor) {
                tracing::warn!(stream = stream_id, "could not follow the mirrored display: {e:#}");
            }
        }
        self.update_cursor();
    }

    /// Waits for macOS to finish rearranging displays after a virtual display
    /// came or went, keeps matched Mac displays mirroring theirs, and captures
    /// those afresh: a capture running across the change can keep showing a
    /// matched virtual display as an empty extended desktop.
    fn resettle_matches(&mut self) {
        let matches: Vec<Mirrored> = self.streams.values().filter_map(|s| s.mirrored).collect();
        let shows_cursor = !self.client_cursor;
        for m in matches {
            if let Err(e) = m.settle() {
                tracing::warn!("{e:#}");
                continue;
            }
            for (stream_id, s) in self.streams.iter_mut().filter(|(_, s)| s.display_id == m.master) {
                if let Err(e) = s.recapture(m.master, shows_cursor) {
                    tracing::warn!(stream = stream_id, "could not capture the matched display again: {e:#}");
                }
            }
        }
    }
}

impl MacStream {
    /// Starts capturing `target` afresh.
    fn recapture(&mut self, target: u32, shows_cursor: bool) -> anyhow::Result<()> {
        self.capture.take();
        let cb = self.shared.clone();
        let c = Capture::start(target, self.req.width, self.req.height, shows_cursor, Box::new(move |buf, t| cb.on_capture(buf, t)))?;
        self.capture = Some(c);
        self.display_id = target;
        Ok(())
    }
}

fn mirrored_uuid(req: &StreamRequest) -> Option<String> {
    match &req.source {
        StreamSource::Mirror { display_uuid } => Some(display_uuid.clone()),
        StreamSource::Extend => None,
    }
}

fn same_display(a: &StreamRequest, b: &StreamRequest) -> bool {
    a.source == b.source
        && a.match_mirror == b.match_mirror
        && a.width == b.width
        && a.height == b.height
        && a.slot == b.slot
        && a.receiver_id == b.receiver_id
}

impl Backend for MacBackend {
    fn mac_displays(&mut self) -> Vec<MacDisplay> {
        // macOS brings back the arrangement it remembers for a set of
        // displays whenever one comes or goes, which can turn a match around
        // or undo it (a display plugged into the Mac, System Settings).
        if self.streams.values().any(|s| s.mirrored.is_some_and(|m| !m.holds())) {
            tracing::warn!("macOS changed the display mirroring; restoring it");
            self.resettle_matches();
        }
        let shows_cursor = !self.client_cursor;
        for (stream_id, s) in self.streams.iter_mut() {
            if s.capture.as_ref().is_some_and(|c| c.stopped()) {
                let target = s.display_id;
                if let Err(e) = s.recapture(target, shows_cursor) {
                    tracing::warn!(stream = stream_id, "could not restart the capture: {e:#}");
                }
            }
        }
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
        let matched = s.mirrored.is_some();
        let extended = s.vdisplay.is_some() && !matched;
        self.streams.insert(req.stream_id, s);
        self.update_cursor();
        if matched && let Some(uuid) = mirrored_uuid(req) {
            self.recapture_mirrors_of(&uuid);
        }
        if extended {
            self.resettle_matches();
        }
        Ok(())
    }

    fn stop_stream(&mut self, stream_id: u8) {
        if let Some(mut s) = self.streams.remove(&stream_id) {
            s.capture.take();
            let unmatched = s.mirrored.take().inspect(|m| {
                if let Err(e) = m.stop() {
                    tracing::warn!("{e:#}");
                }
            });
            let extended = s.vdisplay.is_some() && unmatched.is_none();
            if let Some(vd) = s.vdisplay.take() {
                self.retired.insert(identity(&s.req), vd.id);
            }
            self.update_cursor();
            if unmatched.is_some() && let Some(uuid) = mirrored_uuid(&s.req) {
                self.recapture_mirrors_of(&uuid);
            }
            if extended {
                self.resettle_matches();
            }
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
