//! Session state machine and stream management (spec 5 and 8).
//!
//! One engine thread owns all state and the backend. Network reads, connects
//! and pairing run on helper threads and report back as events.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_core::abr::AbrController;
use annex_core::config::{self, SenderConfig};
use annex_core::modes::{EncoderBudget, MonitorDemand, assign_modes};
use annex_core::resolution::{fit_within, stream_size};
use annex_core::stats::us_since;
use annex_proto::control::{
    ControlMsg, CursorShape, Hello, MacDisplay, ModePref, MonitorInfo, ReceiverStreamStats, SenderStreamStats,
    SourcePref, StreamConfig, StreamSource, VideoMode,
};
use annex_proto::crypto::PacketCipher;
use annex_proto::cursor::{self, CursorPos};
use annex_proto::addr::is_lan_addr;
use annex_proto::discovery::{Browser, DiscoveredReceiver};
use annex_proto::framing::secure_pair;
use annex_proto::pairing::{self, PairError, PeerIdentity};
use annex_proto::{APP_VERSION, DEFAULT_VIDEO_PORT, MAX_STREAMS, PROTO_VERSION, noise};
use crossbeam_channel::{Receiver, Sender, bounded, select, tick, unbounded};
use serde::Serialize;

use crate::sink::{CursorEvent, CursorSink, FrameSink, SinkShared, StreamMeters};
use crate::transport::{self, FecTable, SendMeters, Transport, TransportCmd};
use crate::{Backend, StreamRequest};

/// Silence on the control channel after which the session counts as lost.
const SESSION_TIMEOUT: Duration = Duration::from_secs(4);
/// Virtual displays survive a dropped connection this long (spec 3.4).
const GRACE: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Sharp is lowered to Fast after this many seconds in a row of the encoder
/// not keeping up: frames skipped while busy, or encodes so slow a queue
/// builds. A long but pipelined encode alone is fine.
const SLOW_ENCODE_MS: f64 = 33.0;
const SLOW_SECS: u32 = 3;
const MAX_SHAPES: usize = 32;

pub struct EngineOptions {
    pub config_path: PathBuf,
    /// Shown on the PC during pairing and in its menu.
    pub host_name: String,
    /// Skip discovery and use this receiver's control address.
    pub manual_receiver: Option<SocketAddr>,
    /// Video port for `manual_receiver`.
    pub manual_video_port: u16,
}

impl EngineOptions {
    pub fn new(config_path: PathBuf, host_name: String) -> Self {
        EngineOptions { config_path, host_name, manual_receiver: None, manual_video_port: DEFAULT_VIDEO_PORT }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum ConnState {
    #[default]
    NotPaired,
    Searching,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum PairingState {
    Idle,
    Working,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundReceiver {
    pub id: String,
    pub name: String,
    pub pairing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamStatus {
    pub stream_id: u8,
    pub monitor_id: u64,
    pub monitor_name: String,
    pub width: u32,
    pub height: u32,
    pub mode: VideoMode,
    pub mirror: bool,
    pub live: bool,
    pub bitrate_kbps: u32,
    pub fec_percent: u8,
    pub fps_sent: f32,
    pub fps_presented: f32,
    pub loss_percent: f32,
    pub encode_ms: f32,
    pub e2e_ms: Option<f32>,
    pub downgraded: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: ConnState,
    pub paired_id: Option<String>,
    pub paired_name: Option<String>,
    pub receiver_gpu: Option<String>,
    pub receiver_hevc_ok: bool,
    pub discovered: Vec<FoundReceiver>,
    pub monitors: Vec<MonitorInfo>,
    pub mac_displays: Vec<MacDisplay>,
    pub streams: Vec<StreamStatus>,
    pub client_cursor: bool,
    /// Monitors whose mirror matches the PC monitor's shape (Mac-side setting).
    pub mirror_match: Vec<u64>,
    pub pairing: PairingState,
    /// Last thing worth telling the user.
    pub message: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            state: ConnState::NotPaired,
            paired_id: None,
            paired_name: None,
            receiver_gpu: None,
            receiver_hevc_ok: true,
            discovered: Vec::new(),
            monitors: Vec::new(),
            mac_displays: Vec::new(),
            streams: Vec::new(),
            client_cursor: true,
            mirror_match: Vec::new(),
            pairing: PairingState::Idle,
            message: None,
        }
    }
}

enum Cmd {
    Pair { receiver_id: Option<String>, pin: String },
    Forget,
    SetMonitorPrefs { monitor_id: u64, enabled: Option<bool>, pinned_mode: Option<ModePref>, source: Option<SourcePref> },
    SetClientCursor(bool),
    SetMirrorMatch { monitor_id: u64, on: bool },
    Shutdown(Sender<()>),
}

enum Ev {
    Connected { stream: TcpStream, hs: Box<noise::Handshake>, video: SocketAddr, peer_id: String, name: String },
    ConnectFailed(String),
    Ctl { session: u64, msg: ControlMsg },
    Closed { session: u64, error: Option<String> },
    Paired(Result<PeerIdentity, String>),
}

/// Handle for the UI. Cheap to clone.
#[derive(Clone)]
pub struct EngineHandle {
    cmd: Sender<Cmd>,
    status: Arc<Mutex<Status>>,
    version: Arc<AtomicU64>,
}

impl EngineHandle {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Increments whenever the status changes.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Pairs with a receiver showing `pin`. `receiver_id` picks one of
    /// several PCs; `None` takes the one in pairing mode.
    pub fn pair(&self, receiver_id: Option<String>, pin: String) {
        let _ = self.cmd.send(Cmd::Pair { receiver_id, pin });
    }

    pub fn forget(&self) {
        let _ = self.cmd.send(Cmd::Forget);
    }

    /// Changes a PC monitor's settings. The PC stores them and sends the
    /// updated monitor list back.
    pub fn set_monitor_prefs(
        &self,
        monitor_id: u64,
        enabled: Option<bool>,
        pinned_mode: Option<ModePref>,
        source: Option<SourcePref>,
    ) {
        let _ = self.cmd.send(Cmd::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source });
    }

    pub fn set_client_cursor(&self, on: bool) {
        let _ = self.cmd.send(Cmd::SetClientCursor(on));
    }

    /// Mirror mode for a monitor: `true` switches the Mac display to the
    /// monitor's shape, `false` fits it with black bars.
    pub fn set_mirror_match(&self, monitor_id: u64, on: bool) {
        let _ = self.cmd.send(Cmd::SetMirrorMatch { monitor_id, on });
    }

    /// Tells the PC goodbye and unplugs every display. Blocks up to a second.
    pub fn shutdown(&self) {
        let (tx, rx) = bounded(1);
        if self.cmd.send(Cmd::Shutdown(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(1));
        }
    }
}

struct Session {
    id: u64,
    outbox: Sender<ControlMsg>,
    socket: TcpStream,
    video: SocketAddr,
    cursor: PacketCipher,
    writer: Option<std::thread::JoinHandle<()>>,
}

struct Stream {
    stream_id: u8,
    req: StreamRequest,
    live: Arc<AtomicBool>,
    generation: Arc<AtomicU32>,
    /// Session the receiver was last configured in.
    configured: u64,
    abr: AbrController,
    downgraded: bool,
    warning: Option<String>,
    rx: Option<ReceiverStreamStats>,
    fps_sent: f32,
    encode_ms: f32,
    slow_secs: u32,
    cursor_seq: u32,
}

pub struct Engine<B: Backend> {
    backend: B,
    opts: EngineOptions,
    cfg: SenderConfig,
    me: PeerIdentity,
    budget: EncoderBudget,
    machine: String,

    cmd_rx: Receiver<Cmd>,
    ev_tx: Sender<Ev>,
    ev_rx: Receiver<Ev>,
    cursor_rx: Receiver<CursorEvent>,
    cursor_tx: Sender<CursorEvent>,

    sink: Arc<SinkShared>,
    transport: Transport,
    fec: Arc<FecTable>,
    send_meters: Arc<SendMeters>,

    browser: Option<Browser>,
    session: Option<Session>,
    next_session: u64,
    connecting: bool,
    next_connect: Instant,
    backoff: Duration,
    grace_until: Option<Instant>,

    monitors: Vec<MonitorInfo>,
    mac_displays: Vec<MacDisplay>,
    streams: BTreeMap<u64, Stream>,
    force_fast: HashSet<u64>,
    /// Seconds since start, for periodic stats in the log.
    seconds: u64,
    shapes: VecDeque<CursorShape>,
    last_cursor: HashMap<u8, CursorPos>,

    status: Arc<Mutex<Status>>,
    version: Arc<AtomicU64>,
    st: Status,
}

impl<B: Backend> Engine<B> {
    /// Loads (or creates) the config and starts the engine thread.
    pub fn start(backend: B, opts: EngineOptions) -> anyhow::Result<EngineHandle> {
        let cfg: SenderConfig = config::load_or_create(&opts.config_path, SenderConfig::fresh)?;
        let (cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = unbounded();
        let (cursor_tx, cursor_rx) = bounded(256);
        let (frame_tx, frame_rx) = bounded(8);
        let (pool_tx, pool_rx) = bounded(16);
        let fec = Arc::new(FecTable::default());
        let send_meters = Arc::new(SendMeters::default());
        let transport = transport::start(frame_rx, pool_tx, fec.clone(), send_meters.clone())?;
        let sink = Arc::new(SinkShared { epoch: Instant::now(), tx: frame_tx, pool: pool_rx, meters: Mutex::new(HashMap::new()) });
        let browser = if opts.manual_receiver.is_none() {
            match Browser::start() {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::error!("discovery: {e}");
                    None
                }
            }
        } else {
            None
        };
        let machine = backend.machine_name();
        let budget = cfg.encoder_budget_mpx.map(|m| EncoderBudget { mpx_per_sec: m }).unwrap_or_else(|| EncoderBudget::for_chip(&machine));
        let me = PeerIdentity { id: cfg.sender_id.clone(), name: opts.host_name.clone(), public_key: cfg.keys.public };
        let status = Arc::new(Mutex::new(Status::default()));
        let version = Arc::new(AtomicU64::new(0));
        let handle = EngineHandle { cmd: cmd_tx, status: status.clone(), version: version.clone() };

        let mut engine = Engine {
            backend,
            opts,
            cfg,
            me,
            budget,
            machine,
            cmd_rx,
            ev_tx,
            ev_rx,
            cursor_rx,
            cursor_tx,
            sink,
            transport,
            fec,
            send_meters,
            browser,
            session: None,
            next_session: 1,
            connecting: false,
            next_connect: Instant::now(),
            backoff: Duration::from_millis(500),
            grace_until: None,
            monitors: Vec::new(),
            mac_displays: Vec::new(),
            streams: BTreeMap::new(),
            force_fast: HashSet::new(),
            seconds: 0,
            shapes: VecDeque::new(),
            last_cursor: HashMap::new(),
            status,
            version,
            st: Status::default(),
        };
        std::thread::Builder::new().name("annex-engine".into()).spawn(move || engine.run())?;
        Ok(handle)
    }

    fn run(&mut self) {
        tracing::info!(machine = %self.machine, budget = self.budget.mpx_per_sec, "sender engine started");
        if self.cfg.client_cursor {
            self.backend.set_client_cursor(true, Some(CursorSink { tx: self.cursor_tx.clone() }));
        }
        self.mac_displays = self.backend.mac_displays();
        let fast = tick(Duration::from_millis(100));
        let second = tick(Duration::from_secs(1));
        let mut displays_check = Instant::now();
        self.publish();
        loop {
            select! {
                recv(self.cmd_rx) -> c => match c {
                    Ok(Cmd::Shutdown(ack)) => {
                        self.shutdown();
                        let _ = ack.send(());
                        return;
                    }
                    Ok(c) => self.on_cmd(c),
                    Err(_) => {
                        self.shutdown();
                        return;
                    }
                },
                recv(self.ev_rx) -> e => if let Ok(e) = e { self.on_event(e) },
                recv(self.cursor_rx) -> c => if let Ok(c) = c { self.on_cursor(c) },
                recv(fast) -> _ => self.on_fast_tick(),
                recv(second) -> _ => {
                    self.on_second();
                    if displays_check.elapsed() >= Duration::from_secs(3) {
                        displays_check = Instant::now();
                        let d = self.backend.mac_displays();
                        if d != self.mac_displays {
                            self.mac_displays = d;
                            self.send(ControlMsg::MacDisplays(self.mac_displays.clone()));
                            self.reconcile();
                        }
                    }
                },
            }
        }
    }

    // ----- status -------------------------------------------------------

    fn publish(&mut self) {
        let st = &mut self.st;
        st.paired_id = self.cfg.paired.as_ref().map(|p| p.id.clone());
        st.paired_name = self.cfg.paired.as_ref().map(|p| p.name.clone());
        st.state = if self.cfg.paired.is_none() {
            ConnState::NotPaired
        } else if self.session.is_some() {
            ConnState::Connected
        } else if self.connecting {
            ConnState::Connecting
        } else {
            ConnState::Searching
        };
        st.monitors = self.monitors.clone();
        st.mac_displays = self.mac_displays.clone();
        st.client_cursor = self.cfg.client_cursor;
        st.mirror_match = self.cfg.mirror_match.iter().filter_map(|id| id.parse().ok()).collect();
        st.streams = self
            .streams
            .iter()
            .map(|(_, s)| {
                let rx = s.rx.as_ref();
                let lost = rx.map(|r| r.packets_lost as f32).unwrap_or(0.0);
                let got = rx.map(|r| r.packets_received as f32).unwrap_or(0.0);
                StreamStatus {
                    stream_id: s.stream_id,
                    monitor_id: s.req.monitor_id,
                    monitor_name: s.req.monitor_name.clone(),
                    width: s.req.width,
                    height: s.req.height,
                    mode: s.req.mode,
                    mirror: matches!(s.req.source, StreamSource::Mirror { .. }),
                    live: s.live.load(Ordering::Relaxed),
                    bitrate_kbps: s.abr.bitrate_kbps(),
                    fec_percent: s.abr.fec_percent(),
                    fps_sent: s.fps_sent,
                    fps_presented: rx.map(|r| r.frames_presented as f32).unwrap_or(0.0),
                    loss_percent: if got + lost > 0.0 { lost * 100.0 / (got + lost) } else { 0.0 },
                    encode_ms: s.encode_ms,
                    e2e_ms: rx.map(|r| r.e2e_ms).filter(|v| *v >= 0.0),
                    downgraded: s.downgraded,
                    warning: s.warning.clone(),
                }
            })
            .collect();
        if let Some(b) = &self.browser {
            st.discovered = b
                .receivers()
                .into_iter()
                .map(|r| FoundReceiver { id: r.id, name: r.name, pairing: r.pairing })
                .collect();
        }
        *self.status.lock().unwrap() = st.clone();
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!("{msg}");
        self.st.message = Some(msg);
    }

    fn save_config(&mut self) {
        if let Err(e) = config::save(&self.opts.config_path, &self.cfg) {
            tracing::error!("saving config: {e}");
        }
    }

    // ----- commands -----------------------------------------------------

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Pair { receiver_id, pin } => self.start_pairing(receiver_id, pin),
            Cmd::Forget => {
                self.end_session(true);
                self.stop_all_streams();
                self.cfg.paired = None;
                self.save_config();
                self.monitors.clear();
                self.st.pairing = PairingState::Idle;
                self.st.receiver_gpu = None;
                self.note("Forgot the paired PC");
            }
            Cmd::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source } => {
                self.send(ControlMsg::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source });
            }
            Cmd::SetClientCursor(on) => {
                self.cfg.client_cursor = on;
                self.save_config();
                let sink = on.then(|| CursorSink { tx: self.cursor_tx.clone() });
                self.backend.set_client_cursor(on, sink);
            }
            Cmd::SetMirrorMatch { monitor_id, on } => {
                let key = monitor_id.to_string();
                let changed = if on { self.cfg.mirror_match.insert(key) } else { self.cfg.mirror_match.remove(&key) };
                if changed {
                    self.save_config();
                    self.reconcile();
                }
            }
            Cmd::Shutdown(_) => unreachable!(),
        }
        self.publish();
    }

    fn start_pairing(&mut self, receiver_id: Option<String>, pin: String) {
        let target = if let Some(a) = self.opts.manual_receiver {
            Some((vec![a.ip()], None, a.port()))
        } else {
            self.browser.as_mut().map(|b| {
                b.poll();
                b.receivers()
            });
            let found = self.browser.as_ref().map(|b| b.receivers()).unwrap_or_default();
            let pick = match &receiver_id {
                Some(id) => found.into_iter().find(|r| &r.id == id),
                None => {
                    let pairing: Vec<_> = found.iter().filter(|r| r.pairing).cloned().collect();
                    if pairing.len() == 1 { pairing.into_iter().next() } else { found.into_iter().find(|r| r.pairing) }
                }
            };
            pick.map(|r| (r.addrs, Some(r.host), r.control_port))
        };
        let Some(addrs) = target else {
            self.st.pairing = PairingState::Failed("No PC in pairing mode found. Choose \"Pair with a Mac\" on the PC first.".into());
            return;
        };
        self.st.pairing = PairingState::Working;
        let me = self.me.clone();
        let ev = self.ev_tx.clone();
        std::thread::spawn(move || {
            let mut last = String::from("could not reach the PC");
            let addrs: Vec<SocketAddr> = match addrs {
                (ips, Some(host), port) => candidate_ips(&ips, &host).into_iter().map(|ip| SocketAddr::new(ip, port)).collect(),
                (ips, None, port) => ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect(),
            };
            for a in addrs {
                let mut s = match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                    Ok(s) => s,
                    Err(e) => {
                        last = format!("could not reach the PC: {e}");
                        continue;
                    }
                };
                let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
                let r = pairing::pair_as_sender(&mut s, &pin, &me).map_err(|e| match e {
                    PairError::WrongPin => "Wrong PIN".to_string(),
                    PairError::Rejected(r) => r,
                    PairError::Io(e) => format!("pairing failed: {e}"),
                });
                let _ = ev.send(Ev::Paired(r));
                return;
            }
            let _ = ev.send(Ev::Paired(Err(last)));
        });
    }

    fn shutdown(&mut self) {
        self.end_session(true);
        self.stop_all_streams();
        self.backend.set_client_cursor(false, None);
    }

    // ----- events -------------------------------------------------------

    fn on_event(&mut self, e: Ev) {
        match e {
            Ev::Connected { stream, hs, video, peer_id, name } => {
                self.connecting = false;
                self.backoff = Duration::from_millis(500);
                if self.cfg.paired.as_ref().map(|p| &p.id) != Some(&peer_id) {
                    // Forgotten (or re-paired) while the connect was under way.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                } else if let Err(e) = self.begin_session(stream, *hs, video) {
                    self.note(format!("Session setup failed: {e}"));
                } else {
                    tracing::info!(%video, receiver = %name, "connected");
                    self.st.message = None;
                }
            }
            Ev::ConnectFailed(why) => {
                self.connecting = false;
                self.next_connect = Instant::now() + self.backoff;
                self.backoff = (self.backoff * 2).min(Duration::from_secs(5));
                tracing::debug!("connect failed: {why}");
                if self.cfg.paired.is_some() {
                    self.st.message = Some(why);
                }
            }
            Ev::Ctl { session, msg } => {
                if self.session.as_ref().map(|s| s.id) == Some(session) {
                    self.on_ctl(msg);
                }
            }
            Ev::Closed { session, error } => {
                if self.session.as_ref().map(|s| s.id) == Some(session) {
                    if let Some(e) = error {
                        tracing::info!("session lost: {e}");
                    }
                    self.end_session(false);
                    self.grace_until = Some(Instant::now() + GRACE);
                }
            }
            Ev::Paired(r) => match r {
                Ok(peer) => {
                    tracing::info!(peer = %peer.name, "paired");
                    self.end_session(true);
                    self.stop_all_streams();
                    self.st.pairing = PairingState::Done(peer.name.clone());
                    self.cfg.paired = Some(peer.into());
                    self.cfg.slots.clear();
                    self.save_config();
                    self.next_connect = Instant::now();
                }
                Err(e) => self.st.pairing = PairingState::Failed(e),
            },
        }
        self.publish();
    }

    fn begin_session(&mut self, stream: TcpStream, hs: noise::Handshake, video: SocketAddr) -> std::io::Result<()> {
        let id = self.next_session;
        self.next_session += 1;
        stream.set_read_timeout(Some(SESSION_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = secure_pair(hs.transport, stream.try_clone()?, stream.try_clone()?);
        let (out_tx, out_rx) = unbounded::<ControlMsg>();

        let writer_thread = std::thread::Builder::new().name("annex-ctl-tx".into()).spawn(move || {
            let mut next = Instant::now() + Duration::from_secs(1);
            loop {
                match out_rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                    Ok(m) => {
                        let bye = matches!(m, ControlMsg::Bye);
                        if writer.send(&m).is_err() || bye {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        next += Duration::from_secs(1);
                        if writer.send(&ControlMsg::Heartbeat).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = writer.get_mut().shutdown(std::net::Shutdown::Both);
        })?;

        let ev = self.ev_tx.clone();
        let pong = out_tx.clone();
        let epoch = self.sink.epoch;
        std::thread::Builder::new().name("annex-ctl-rx".into()).spawn(move || {
            let error = loop {
                match reader.recv() {
                    Ok(ControlMsg::ClockPing { t1 }) => {
                        let t = us_since(epoch, Instant::now());
                        let _ = pong.send(ControlMsg::ClockPong { t1, t2: t, t3: t });
                    }
                    Ok(ControlMsg::Heartbeat) => {}
                    Ok(msg) => {
                        let bye = matches!(msg, ControlMsg::Bye);
                        if ev.send(Ev::Ctl { session: id, msg }).is_err() || bye {
                            break None;
                        }
                    }
                    Err(e) => break Some(e.to_string()),
                }
            };
            let _ = ev.send(Ev::Closed { session: id, error });
        })?;

        let _ = self.transport.cmd.send(TransportCmd::Session { video_key: hs.video_key, dest: video });
        self.session = Some(Session {
            id,
            outbox: out_tx,
            socket: stream,
            video,
            cursor: PacketCipher::new(&hs.cursor_key),
            writer: Some(writer_thread),
        });
        self.grace_until = None;
        self.force_fast.clear();
        self.send(ControlMsg::Hello(Hello {
            proto_version: PROTO_VERSION,
            app_version: APP_VERSION.to_string(),
            os: "macOS".into(),
            host_name: self.opts.host_name.clone(),
            gpu_name: self.machine.clone(),
            hevc_decode_ok: false,
            max_streams: MAX_STREAMS,
        }));
        self.send(ControlMsg::MacDisplays(self.mac_displays.clone()));
        for s in self.shapes.clone() {
            self.send(ControlMsg::CursorShape(s));
        }
        Ok(())
    }

    /// Closes the session. With `bye`, the PC unplugs everything at once.
    fn end_session(&mut self, bye: bool) {
        let Some(mut s) = self.session.take() else { return };
        if bye {
            let _ = s.outbox.send(ControlMsg::Bye);
            if let Some(w) = s.writer.take() {
                // The writer exits right after sending Bye.
                let deadline = Instant::now() + Duration::from_millis(500);
                while !w.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        let _ = s.socket.shutdown(std::net::Shutdown::Both);
        let _ = self.transport.cmd.send(TransportCmd::Idle);
        for st in self.streams.values_mut() {
            st.live.store(false, Ordering::Release);
        }
        self.st.receiver_gpu = None;
    }

    fn send(&self, msg: ControlMsg) {
        if let Some(s) = &self.session {
            let _ = s.outbox.send(msg);
        }
    }

    fn on_ctl(&mut self, msg: ControlMsg) {
        match msg {
            ControlMsg::Hello(h) => {
                tracing::info!(host = %h.host_name, gpu = %h.gpu_name, version = %h.app_version, "receiver hello");
                if h.proto_version != PROTO_VERSION {
                    self.note(format!("The PC runs Annex {}; update both sides to the same version", h.app_version));
                }
                self.st.receiver_gpu = Some(h.gpu_name);
                self.st.receiver_hevc_ok = h.hevc_decode_ok;
                if !h.hevc_decode_ok {
                    self.note("The PC's graphics driver reports no HEVC decoding");
                }
            }
            ControlMsg::Monitors(list) => {
                self.monitors = list;
                self.reconcile();
            }
            ControlMsg::StreamReady { stream_id } => {
                if let Some(s) = self.streams.values().find(|s| s.stream_id == stream_id) {
                    s.live.store(true, Ordering::Release);
                    self.backend.request_idr(stream_id);
                }
            }
            ControlMsg::IdrRequest { stream_id, .. } => {
                if self.streams.values().any(|s| s.stream_id == stream_id) {
                    self.backend.request_idr(stream_id);
                }
            }
            ControlMsg::Stats(list) => self.on_receiver_stats(list),
            ControlMsg::Warning { text, .. } => self.note(text),
            ControlMsg::Bye => {
                self.end_session(false);
                self.stop_all_streams();
                self.note("The PC closed the connection");
            }
            other => tracing::debug!("ignoring {} from receiver", other.kind()),
        }
    }

    fn on_receiver_stats(&mut self, list: Vec<ReceiverStreamStats>) {
        for r in list {
            let Some(s) = self.streams.values_mut().find(|s| s.stream_id == r.stream_id) else { continue };
            let total = r.packets_received + r.packets_lost;
            if s.live.load(Ordering::Relaxed) && total > 0 {
                let loss = r.packets_lost as f64 / total as f64;
                if let Some((kbps, fec)) = s.abr.on_report(loss) {
                    tracing::info!(stream = s.stream_id, kbps, fec, loss, "bitrate adapted");
                    self.backend.set_bitrate(s.stream_id, kbps);
                    self.fec.set(s.stream_id, fec);
                }
            }
            s.rx = Some(r);
        }
    }

    // ----- streams ------------------------------------------------------

    /// Picture size and source for a monitor (spec 3.1, mirror letterboxing).
    /// `matched` collects the Mac displays already matched to a monitor's
    /// shape: a display can take only one shape at a time.
    fn picture_for(&self, m: &MonitorInfo, matched: &mut HashSet<String>) -> Picture {
        let (w, h) = stream_size(m.native_w, m.native_h);
        if let SourcePref::Mirror { display_uuid } = &m.source {
            if let Some(d) = self.mac_displays.iter().find(|d| &d.uuid == display_uuid) {
                let source = StreamSource::Mirror { display_uuid: display_uuid.clone() };
                let mut note = None;
                if self.cfg.mirror_match.contains(&m.id.to_string()) {
                    if matched.insert(display_uuid.clone()) {
                        return Picture { width: w, height: h, source, match_mirror: true, note };
                    }
                    note = Some("Another PC monitor already matches this Mac display, so this one fits it".into());
                }
                let (mw, mh) = fit_within(d.width, d.height, w, h);
                return Picture { width: mw, height: mh, source, match_mirror: false, note };
            }
            let note = Some("The mirrored Mac display is not connected; extending instead".into());
            return Picture { width: w, height: h, source: StreamSource::Extend, match_mirror: false, note };
        }
        Picture { width: w, height: h, source: StreamSource::Extend, match_mirror: false, note: None }
    }

    /// Makes the running streams match the PC's monitor list.
    fn reconcile(&mut self) {
        let Some(session) = self.session.as_ref().map(|s| s.id) else { return };
        let Some(receiver_id) = self.cfg.paired.as_ref().map(|p| p.id.clone()) else { return };
        let mut wanted: Vec<&MonitorInfo> = self.monitors.iter().filter(|m| m.enabled).collect();
        wanted.sort_by_key(|m| (m.priority, m.id));
        wanted.truncate(MAX_STREAMS as usize);
        let wanted: Vec<MonitorInfo> = wanted.into_iter().cloned().collect();

        let mut matched = HashSet::new();
        let pictures: Vec<Picture> = wanted.iter().map(|m| self.picture_for(m, &mut matched)).collect();
        let demands: Vec<MonitorDemand> = wanted
            .iter()
            .zip(&pictures)
            .map(|(m, p)| MonitorDemand {
                monitor_id: m.id,
                stream_w: p.width,
                stream_h: p.height,
                priority: m.priority,
                pinned: m.pinned_mode.pinned(),
                force_fast: self.force_fast.contains(&m.id),
            })
            .collect();
        let assigned = assign_modes(&demands, &self.budget);

        let keep: HashSet<u64> = wanted.iter().map(|m| m.id).collect();
        let gone: Vec<u64> = self.streams.keys().filter(|id| !keep.contains(id)).copied().collect();
        for id in gone {
            self.stop_stream(id);
        }

        let mut slots_changed = false;
        for (m, p) in wanted.iter().zip(pictures) {
            let Some(a) = assigned.iter().find(|a| a.monitor_id == m.id) else { continue };
            let before = self.cfg.slots.len();
            let slot = self.cfg.slot_for(m.id);
            slots_changed |= self.cfg.slots.len() != before;
            let req = StreamRequest {
                stream_id: 0,
                monitor_id: m.id,
                monitor_name: m.name.clone(),
                receiver_id: receiver_id.clone(),
                slot,
                source: p.source,
                match_mirror: p.match_mirror,
                width: p.width,
                height: p.height,
                mode: a.mode,
                bitrate_kbps: a.bitrate_kbps,
            };
            let warning = a.warning.clone().or(p.note);
            match self.streams.get_mut(&m.id) {
                Some(s) if same_picture(&s.req, &req) => {
                    s.warning = warning;
                    s.downgraded = a.downgraded;
                    s.req.monitor_name = req.monitor_name;
                    if s.configured != session {
                        self.restart_stream(m.id);
                    }
                }
                Some(s) => {
                    s.req = StreamRequest { stream_id: s.stream_id, ..req };
                    s.abr = AbrController::new(a.bitrate_kbps, a.bitrate_kbps);
                    s.warning = warning;
                    s.downgraded = a.downgraded;
                    self.restart_stream(m.id);
                }
                None => {
                    let stream_id = self.free_stream_id();
                    self.streams.insert(
                        m.id,
                        Stream {
                            stream_id,
                            req: StreamRequest { stream_id, ..req },
                            live: Arc::new(AtomicBool::new(false)),
                            generation: Arc::new(AtomicU32::new(0)),
                            configured: 0,
                            abr: AbrController::new(a.bitrate_kbps, a.bitrate_kbps),
                            downgraded: a.downgraded,
                            warning,
                            rx: None,
                            fps_sent: 0.0,
                            encode_ms: 0.0,
                            slow_secs: 0,
                            cursor_seq: 0,
                        },
                    );
                    self.restart_stream(m.id);
                }
            }
        }
        if slots_changed {
            self.save_config();
        }
        self.publish();
    }

    fn free_stream_id(&self) -> u8 {
        (1..=250u8).find(|id| !self.streams.values().any(|s| s.stream_id == *id)).unwrap_or(1)
    }

    /// (Re)starts the backend stream and (re)configures the receiver.
    fn restart_stream(&mut self, monitor_id: u64) {
        let Some(session) = self.session.as_ref().map(|s| s.id) else { return };
        let Some(s) = self.streams.get_mut(&monitor_id) else { return };
        let generation = s.generation.fetch_add(1, Ordering::AcqRel) + 1;
        s.live.store(false, Ordering::Release);
        s.configured = session;
        s.slow_secs = 0;
        let sink = FrameSink {
            stream_id: s.stream_id,
            shared: self.sink.clone(),
            live: s.live.clone(),
            generation: s.generation.clone(),
            my_generation: generation,
        };
        self.sink.meters.lock().unwrap().insert(s.stream_id, StreamMeters::default());
        self.fec.set(s.stream_id, s.abr.fec_percent());
        let mut req = s.req.clone();
        req.bitrate_kbps = s.abr.bitrate_kbps();
        let cfg = StreamConfig {
            stream_id: s.stream_id,
            monitor_id,
            source: req.source.clone(),
            stream_w: req.width,
            stream_h: req.height,
            mode: req.mode,
            bitrate_kbps: req.bitrate_kbps,
            fec_percent: s.abr.fec_percent(),
        };
        tracing::info!(stream = s.stream_id, monitor = %req.monitor_name, w = req.width, h = req.height, mode = req.mode.as_str(), "starting stream");
        match self.backend.start_stream(&req, sink) {
            Ok(()) => self.send(ControlMsg::StreamConfig(cfg)),
            Err(e) => {
                let id = s.stream_id;
                tracing::error!(stream = id, "backend could not start the stream: {e:#}");
                s.warning = Some(format!("Could not start this display: {e}"));
                self.backend.stop_stream(id);
            }
        }
    }

    fn stop_stream(&mut self, monitor_id: u64) {
        if let Some(s) = self.streams.remove(&monitor_id) {
            tracing::info!(stream = s.stream_id, monitor = %s.req.monitor_name, "stopping stream");
            s.live.store(false, Ordering::Release);
            s.generation.fetch_add(1, Ordering::AcqRel);
            self.backend.stop_stream(s.stream_id);
            self.send(ControlMsg::StreamStop { stream_id: s.stream_id });
            self.sink.meters.lock().unwrap().remove(&s.stream_id);
            self.last_cursor.remove(&s.stream_id);
        }
    }

    fn stop_all_streams(&mut self) {
        let ids: Vec<u64> = self.streams.keys().copied().collect();
        for id in ids {
            self.stop_stream(id);
        }
        self.grace_until = None;
    }

    // ----- cursor -------------------------------------------------------

    fn on_cursor(&mut self, c: CursorEvent) {
        match c {
            CursorEvent::Position { stream_id, x, y, visible, shape_id } => {
                let Some(s) = self.streams.values_mut().find(|s| s.stream_id == stream_id) else { return };
                s.cursor_seq = s.cursor_seq.wrapping_add(1);
                let pos = CursorPos { stream_id, visible, seq: s.cursor_seq, x, y, shape_id };
                let live = s.live.load(Ordering::Relaxed);
                self.last_cursor.insert(stream_id, pos);
                if live {
                    self.send_cursor(&pos);
                }
            }
            CursorEvent::Shape(shape) => {
                if let Some(i) = self.shapes.iter().position(|s| s.shape_id == shape.shape_id) {
                    self.shapes.remove(i);
                }
                if self.shapes.len() >= MAX_SHAPES {
                    self.shapes.pop_front();
                }
                self.shapes.push_back(shape.clone());
                self.send(ControlMsg::CursorShape(shape));
            }
        }
    }

    fn send_cursor(&self, pos: &CursorPos) {
        if let Some(s) = &self.session {
            let pkt = cursor::encode(pos, &s.cursor);
            let _ = self.transport.socket.send_to(&pkt, s.video);
        }
    }

    // ----- timers -------------------------------------------------------

    fn on_fast_tick(&mut self) {
        let mut changed = false;
        if let Some(b) = &mut self.browser {
            changed |= b.poll();
        }
        if self.session.is_none() && !self.connecting && self.cfg.paired.is_some() && Instant::now() >= self.next_connect {
            self.try_connect();
            changed = true;
        }
        if let Some(t) = self.grace_until
            && self.session.is_none()
            && Instant::now() >= t
        {
            tracing::info!("grace period over, unplugging displays");
            self.stop_all_streams();
            changed = true;
        }
        if changed {
            self.publish();
        }
    }

    fn try_connect(&mut self) {
        let Some(peer) = self.cfg.paired.clone() else { return };
        let targets = if let Some(a) = self.opts.manual_receiver {
            Targets::Manual(vec![(a, SocketAddr::new(a.ip(), self.opts.manual_video_port))])
        } else {
            match self.browser.as_ref().and_then(|b| b.find(&peer.id)) {
                Some(r) => Targets::Found(r),
                None => {
                    self.next_connect = Instant::now() + Duration::from_secs(1);
                    return;
                }
            }
        };
        self.connecting = true;
        let keys = self.cfg.keys.clone();
        let ev = self.ev_tx.clone();
        let name = peer.name.clone();
        let peer_id = peer.id.clone();
        std::thread::spawn(move || {
            let targets: Vec<(SocketAddr, SocketAddr)> = match targets {
                Targets::Manual(t) => t,
                Targets::Found(r) => candidate_ips(&r.addrs, &r.host)
                    .into_iter()
                    .map(|ip| (SocketAddr::new(ip, r.control_port), SocketAddr::new(ip, r.video_port)))
                    .collect(),
            };
            // Report the failure of the best address, not of the last fallback.
            let mut first: Option<String> = None;
            for (ctl, video) in targets {
                let mut s = match TcpStream::connect_timeout(&ctl, CONNECT_TIMEOUT) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!(%ctl, "connect failed: {e}");
                        first.get_or_insert_with(|| format!("PC not reachable at {}: {e}", ctl.ip()));
                        continue;
                    }
                };
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = s.set_nodelay(true);
                match noise::initiate(&mut s, &keys, &peer.public_key) {
                    Ok(hs) => {
                        let _ = ev.send(Ev::Connected { stream: s, hs: Box::new(hs), video, peer_id, name });
                        return;
                    }
                    Err(e) => {
                        let _ = ev.send(Ev::ConnectFailed(format!("The PC refused the connection ({e}). If it was reset, pair again.")));
                        return;
                    }
                }
            }
            let _ = ev.send(Ev::ConnectFailed(first.unwrap_or_else(|| "PC not reachable".into())));
        });
    }

    fn on_second(&mut self) {
        let mut meters = std::mem::take(&mut *self.sink.meters.lock().unwrap());
        let bytes = std::mem::take(&mut *self.send_meters.bytes.lock().unwrap());
        let mut report = Vec::new();
        let mut reassign = false;
        self.seconds += 1;
        let log_stats = self.seconds % 10 == 0;
        for s in self.streams.values_mut() {
            let m = meters.entry(s.stream_id).or_default();
            let encode = m.encode_ms.take();
            let pack = m.pack_ms.take();
            s.fps_sent = m.sent as f32;
            s.encode_ms = encode.unwrap_or(0.0) as f32;
            if log_stats {
                let rx = s.rx.as_ref();
                tracing::info!(
                    stream = s.stream_id,
                    mode = s.req.mode.as_str(),
                    captured = m.captured,
                    sent = m.sent,
                    shown = rx.map(|r| r.frames_presented).unwrap_or(0),
                    pack_ms = format_args!("{:.1}", pack.unwrap_or(0.0)),
                    encode_ms = format_args!("{:.1}", s.encode_ms),
                    kbps = bytes.get(&s.stream_id).copied().unwrap_or(0) * 8 / 1000,
                    lost = rx.map(|r| r.packets_lost).unwrap_or(0),
                    e2e_ms = format_args!("{:.1}", rx.map(|r| r.e2e_ms).unwrap_or(-1.0)),
                    "stream stats"
                );
            }
            // Spec 4.3: an encoder that can't keep Sharp at 60 fps drops to Fast.
            let skipping = m.captured >= 20 && m.sent * 10 < m.captured * 9;
            let slow = skipping || encode.unwrap_or(0.0) > SLOW_ENCODE_MS;
            if s.req.mode == VideoMode::Sharp && s.live.load(Ordering::Relaxed) && slow {
                s.slow_secs += 1;
                if s.slow_secs == SLOW_SECS && self.force_fast.insert(s.req.monitor_id) {
                    tracing::warn!(stream = s.stream_id, encode_ms = s.encode_ms, captured = m.captured, sent = m.sent, "encoder too slow for Sharp");
                    reassign = true;
                }
            } else {
                s.slow_secs = 0;
            }
            report.push(SenderStreamStats {
                stream_id: s.stream_id,
                fps_captured: m.captured as f32,
                fps_sent: m.sent as f32,
                pack_ms: pack.unwrap_or(0.0) as f32,
                encode_ms: s.encode_ms,
                send_ms: 0.0,
                bitrate_kbps: (bytes.get(&s.stream_id).copied().unwrap_or(0) * 8 / 1000) as u32,
                fec_percent: s.abr.fec_percent(),
                mode: Some(s.req.mode),
                downgraded: s.downgraded,
            });
        }
        // Keep the meters for live streams (the sinks write into them).
        {
            let mut live = self.sink.meters.lock().unwrap();
            for s in self.streams.values() {
                live.entry(s.stream_id).or_default();
            }
        }
        if !report.is_empty() {
            self.send(ControlMsg::SenderStats(report));
        }
        // Repeat the cursor position so a lost packet doesn't leave it stale.
        let positions: Vec<CursorPos> = self.last_cursor.values().copied().collect();
        for mut p in positions {
            if let Some(s) = self.streams.values_mut().find(|s| s.stream_id == p.stream_id)
                && s.live.load(Ordering::Relaxed)
            {
                s.cursor_seq = s.cursor_seq.wrapping_add(1);
                p.seq = s.cursor_seq;
                self.send_cursor(&p);
            }
        }
        if reassign {
            self.reconcile();
        } else {
            self.publish();
        }
    }
}

/// What a stream shows and at which size.
struct Picture {
    width: u32,
    height: u32,
    source: StreamSource,
    match_mirror: bool,
    note: Option<String>,
}

/// Where to connect: a typed-in address, or a PC found over mDNS.
enum Targets {
    Manual(Vec<(SocketAddr, SocketAddr)>),
    Found(DiscoveredReceiver),
}

/// Addresses to try for a PC found over mDNS, best first. Runs off the engine
/// thread because it may block on the system resolver.
///
/// The receiver listens on IPv4 only. mDNS answers can arrive piecemeal, so a
/// PC may be known by its IPv6 addresses alone for a while; asking the system
/// resolver for its host name fills in the IPv4 address. Real LAN addresses
/// go before the 169.254.x.x addresses of the PC's unconnected adapters.
fn candidate_ips(addrs: &[IpAddr], host: &str) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = addrs.iter().copied().filter(IpAddr::is_ipv4).collect();
    let routable = |ip: &IpAddr| matches!(ip, IpAddr::V4(v4) if !v4.is_link_local());
    if !ips.iter().any(routable)
        && let Ok(resolved) = (host.trim_end_matches('.'), 0).to_socket_addrs()
    {
        for ip in resolved.map(|a| a.ip()) {
            if ip.is_ipv4() && is_lan_addr(ip) && !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips.sort_by_key(|ip| !routable(ip));
    ips
}

fn same_picture(a: &StreamRequest, b: &StreamRequest) -> bool {
    a.source == b.source
        && a.match_mirror == b.match_mirror
        && a.width == b.width
        && a.height == b.height
        && a.mode == b.mode
        && a.slot == b.slot
}
