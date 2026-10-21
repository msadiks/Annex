//! State shared between the UI thread, the network threads and the render
//! threads. Nothing here is on the per-packet path.

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use annex_core::stats::ClockSync;
use annex_proto::control::{ControlMsg, ReceiverStreamStats, SenderStreamStats, StreamConfig};
use annex_proto::reassembler::ReassemblyStats;
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::app::UiCmd;

/// Posted to the UI's hidden window when `UiCmd`s are queued.
pub const WM_ANNEX_WAKE: u32 = WM_APP + 1;

pub struct SessionLink {
    pub id: u64,
    pub outbox: Sender<ControlMsg>,
    pub socket: TcpStream,
}

#[derive(Default, Clone)]
struct Board {
    fps: f32,
    presented: u32,
    decode_ms: Option<f64>,
    render_ms: Option<f64>,
    e2e_ms: Option<f64>,
    network_ms: Option<f64>,
    net: ReassemblyStats,
    queue_depth: u8,
    sender: Option<SenderStreamStats>,
}

pub struct Pairing {
    pub pin: String,
    pub until: Instant,
    pub attempts: u8,
}

pub struct Shared {
    pub epoch: Instant,
    pub host_name: String,
    session: Mutex<Option<SessionLink>>,
    clock: Mutex<ClockSync>,
    board: Mutex<HashMap<u8, Board>>,
    show_stats: AtomicBool,
    ui_tx: Sender<UiCmd>,
    ui_hwnd: AtomicIsize,
    pub pairing: Mutex<Option<Pairing>>,
    pub gpu_name: Mutex<String>,
    pub hevc_ok: AtomicBool,
}

impl Shared {
    pub fn new(ui_tx: Sender<UiCmd>, host_name: String, show_stats: bool) -> Self {
        Shared {
            epoch: Instant::now(),
            host_name,
            session: Mutex::new(None),
            clock: Mutex::new(ClockSync::default()),
            board: Mutex::new(HashMap::new()),
            show_stats: AtomicBool::new(show_stats),
            ui_tx,
            ui_hwnd: AtomicIsize::new(0),
            pairing: Mutex::new(None),
            gpu_name: Mutex::new(String::new()),
            hevc_ok: AtomicBool::new(false),
        }
    }

    pub fn now_us(&self) -> u64 {
        annex_core::stats::us_since(self.epoch, Instant::now())
    }

    pub fn set_ui_hwnd(&self, hwnd: HWND) {
        self.ui_hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    /// Queues a command for the UI thread and wakes it.
    pub fn ui(&self, cmd: UiCmd) {
        let _ = self.ui_tx.send(cmd);
        let h = self.ui_hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_ANNEX_WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Sends a control message on the current session, if any.
    pub fn send(&self, msg: ControlMsg) {
        if let Some(s) = self.session.lock().unwrap().as_ref() {
            let _ = s.outbox.send(msg);
        }
    }

    /// Installs a new session, closing the previous one.
    pub fn set_session(&self, link: SessionLink) {
        let old = self.session.lock().unwrap().replace(link);
        if let Some(old) = old {
            let _ = old.socket.shutdown(std::net::Shutdown::Both);
        }
        *self.clock.lock().unwrap() = ClockSync::default();
    }

    /// Clears the session if it is still `id`. Returns true if it was.
    pub fn clear_session(&self, id: u64) -> bool {
        let mut s = self.session.lock().unwrap();
        if s.as_ref().map(|l| l.id) == Some(id) {
            *s = None;
            true
        } else {
            false
        }
    }

    pub fn clock_add(&self, t1: u64, t2: u64, t3: u64, t4: u64) {
        self.clock.lock().unwrap().add(t1, t2, t3, t4);
    }

    /// Sender clock minus our clock (µs).
    pub fn clock_offset_us(&self) -> Option<i64> {
        self.clock.lock().unwrap().offset_us()
    }

    pub fn show_stats(&self) -> bool {
        self.show_stats.load(Ordering::Relaxed)
    }

    pub fn set_show_stats(&self, on: bool) {
        self.show_stats.store(on, Ordering::Relaxed);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn report_render(
        &self,
        stream_id: u8,
        fps: f32,
        presented: u32,
        decode_ms: Option<f64>,
        render_ms: Option<f64>,
        e2e_ms: Option<f64>,
        network_ms: Option<f64>,
        _panel: (u32, u32),
    ) {
        let mut b = self.board.lock().unwrap();
        let e = b.entry(stream_id).or_default();
        e.fps = fps;
        e.presented = presented;
        e.decode_ms = decode_ms;
        e.render_ms = render_ms;
        e.e2e_ms = e2e_ms;
        e.network_ms = network_ms;
    }

    pub fn report_net(&self, stream_id: u8, delta: ReassemblyStats, queue_depth: u8) {
        let mut b = self.board.lock().unwrap();
        let e = b.entry(stream_id).or_default();
        e.net = delta;
        e.queue_depth = queue_depth;
    }

    pub fn report_sender(&self, stats: Vec<SenderStreamStats>) {
        let mut b = self.board.lock().unwrap();
        for s in stats {
            let id = s.stream_id;
            b.entry(id).or_default().sender = Some(s);
        }
    }

    pub fn forget_stream(&self, stream_id: u8) {
        self.board.lock().unwrap().remove(&stream_id);
    }

    /// The once-per-second report for the sender.
    pub fn stats_report(&self) -> Vec<ReceiverStreamStats> {
        let b = self.board.lock().unwrap();
        b.iter()
            .map(|(id, e)| ReceiverStreamStats {
                stream_id: *id,
                packets_received: e.net.packets as u32,
                packets_lost: e.net.shards_missing as u32,
                frames_presented: e.presented,
                frames_repaired: e.net.frames_repaired as u32,
                frames_lost: e.net.frames_lost as u32,
                frames_skipped: e.net.frames_skipped as u32,
                bytes_received: e.net.bytes,
                decode_ms: e.decode_ms.unwrap_or(-1.0) as f32,
                render_ms: e.render_ms.unwrap_or(-1.0) as f32,
                e2e_ms: e.e2e_ms.unwrap_or(-1.0) as f32,
                network_ms: e.network_ms.unwrap_or(-1.0) as f32,
                queue_depth: e.queue_depth,
            })
            .collect()
    }

    pub fn overlay_text(
        &self,
        stream_id: u8,
        cfg: &StreamConfig,
        panel: (u32, u32),
    ) -> Vec<String> {
        let b = self.board.lock().unwrap();
        let e = b.get(&stream_id).cloned().unwrap_or_default();
        let ms = |v: Option<f64>| v.map(|v| format!("{v:.1} ms")).unwrap_or_else(|| "-".into());
        let mbps = e.net.bytes as f64 * 8.0 / 1e6;
        let loss = e.net.loss_ratio() * 100.0;
        let mut lines = vec![
            format!(
                "Annex  {}  {}x{} -> {}x{}",
                cfg.mode.as_str(),
                cfg.stream_w,
                cfg.stream_h,
                panel.0,
                panel.1
            ),
            format!("fps {:.1}   {:.1} Mbps   queue {}", e.fps, mbps, e.queue_depth),
            format!(
                "loss {:.2}%  repaired {}  lost {}  skipped {}",
                loss, e.net.frames_repaired, e.net.frames_lost, e.net.frames_skipped
            ),
            format!("net {}  decode {}  render {}", ms(e.network_ms), ms(e.decode_ms), ms(e.render_ms)),
        ];
        if let Some(s) = &e.sender {
            lines.push(format!(
                "mac: encode {:.1} ms  pack {:.1} ms  {} kbps  FEC {}%{}",
                s.encode_ms,
                s.pack_ms,
                s.bitrate_kbps,
                s.fec_percent,
                if s.downgraded { "  (downgraded)" } else { "" }
            ));
        }
        lines.push(format!("capture -> present ~{}", ms(e.e2e_ms)));
        lines
    }
}

/// How long the session may be silent before it counts as lost.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(4);
