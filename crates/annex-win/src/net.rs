//! Receiver networking: TCP control server (pairing + Noise sessions) and the
//! UDP receive thread (video + cursor).

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use annex_proto::addr::is_lan_socket;
use annex_proto::control::{ControlMsg, Hello};
use annex_proto::crypto::PacketCipher;
use annex_proto::cursor;
use annex_proto::framing::{ConnKind, read_preamble, secure_pair};
use annex_proto::noise;
use annex_proto::pairing::{PairError, PeerIdentity, pair_as_receiver};
use annex_proto::packet::MAX_DATAGRAM;
use annex_proto::reassembler::{Reassembler, ReassemblyStats, peek_stream_id};
use annex_proto::{APP_VERSION, PROTO_VERSION};
use crossbeam_channel::{Receiver, Sender, TrySendError, unbounded};
use socket2::{Domain, Protocol, Socket, Type};

use crate::app::UiCmd;
use crate::pipeline::{FrameMsg, PipeMsg};
use crate::shared::{SESSION_TIMEOUT, SessionLink, Shared};

pub enum UdpCmd {
    SetKeys { video: [u8; 32], cursor: [u8; 32] },
    ClearKeys,
    AddStream { stream_id: u8, frames: Sender<FrameMsg>, ctl: Sender<PipeMsg> },
    RemoveStream { stream_id: u8 },
}

/// What the network threads need to know about this receiver.
#[derive(Clone)]
pub struct Identity {
    pub me: PeerIdentity,
    pub keys: annex_proto::keys::StaticKeypair,
}

pub struct Net {
    pub udp: Sender<UdpCmd>,
    pub recycle: Sender<Vec<u8>>,
}

pub fn start(
    shared: Arc<Shared>,
    ident: Identity,
    paired_key: Arc<std::sync::Mutex<Option<[u8; 32]>>>,
    control_port: u16,
    video_port: u16,
) -> io::Result<Net> {
    let listener = TcpListener::bind(("0.0.0.0", control_port))?;
    let udp = bind_udp(video_port)?;
    let (udp_tx, udp_rx) = unbounded();
    let (recycle_tx, recycle_rx) = unbounded();

    {
        let shared = shared.clone();
        let udp_tx = udp_tx.clone();
        std::thread::Builder::new().name("annex-control".into()).spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let peer = match stream.peer_addr() {
                    Ok(p) if is_lan_socket(p) => p,
                    _ => continue,
                };
                let shared = shared.clone();
                let ident = ident.clone();
                let paired_key = paired_key.clone();
                let udp_tx = udp_tx.clone();
                let _ = std::thread::Builder::new().name("annex-conn".into()).spawn(move || {
                    if let Err(e) = handle_conn(stream, peer, &shared, &ident, &paired_key, &udp_tx) {
                        tracing::info!(%peer, "connection ended: {e}");
                    }
                });
            }
        })?;
    }
    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("annex-udp".into())
            .spawn(move || udp_loop(udp, shared, udp_rx, recycle_rx))?;
    }
    Ok(Net { udp: udp_tx, recycle: recycle_tx })
}

fn bind_udp(port: u16) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = s.set_recv_buffer_size(8 * 1024 * 1024);
    s.bind(&SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(20)))?;
    Ok(s)
}

fn handle_conn(
    mut stream: TcpStream,
    peer: SocketAddr,
    shared: &Arc<Shared>,
    ident: &Identity,
    paired_key: &std::sync::Mutex<Option<[u8; 32]>>,
    udp: &Sender<UdpCmd>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;
    match read_preamble(&mut stream)? {
        ConnKind::Pairing => handle_pairing(stream, shared, ident),
        ConnKind::Session => {
            let expected = *paired_key.lock().unwrap();
            let hs = noise::respond(&mut stream, &ident.keys, |k| Some(*k) == expected)?;
            tracing::info!(%peer, "session established");
            run_session(stream, hs, shared, udp)
        }
    }
}

fn handle_pairing(mut stream: TcpStream, shared: &Arc<Shared>, ident: &Identity) -> io::Result<()> {
    let pin = {
        let p = shared.pairing.lock().unwrap();
        p.as_ref().filter(|p| Instant::now() < p.until).map(|p| p.pin.clone())
    };
    match pair_as_receiver(&mut stream, pin.as_deref(), &ident.me) {
        Ok(peer) => {
            tracing::info!(name = %peer.name, "paired");
            *shared.pairing.lock().unwrap() = None;
            shared.ui(UiCmd::Paired(peer));
            Ok(())
        }
        Err(PairError::WrongPin) => {
            let mut p = shared.pairing.lock().unwrap();
            if let Some(pp) = p.as_mut() {
                pp.attempts += 1;
                if pp.attempts >= 3 {
                    *p = None;
                    drop(p);
                    shared.ui(UiCmd::PairingEnded(Some("Too many wrong PINs. Start pairing again.".into())));
                }
            }
            Err(io::Error::other("wrong PIN"))
        }
        Err(PairError::Rejected(r)) => Err(io::Error::other(r)),
        Err(PairError::Io(e)) => Err(e),
    }
}

static SESSION_IDS: AtomicU64 = AtomicU64::new(1);

fn run_session(
    stream: TcpStream,
    hs: noise::Handshake,
    shared: &Arc<Shared>,
    udp: &Sender<UdpCmd>,
) -> io::Result<()> {
    let id = SESSION_IDS.fetch_add(1, Ordering::Relaxed);
    stream.set_read_timeout(Some(SESSION_TIMEOUT))?;
    let (mut reader, mut writer) = secure_pair(hs.transport, stream.try_clone()?, stream.try_clone()?);
    let (out_tx, out_rx) = unbounded::<ControlMsg>();
    let _ = udp.send(UdpCmd::SetKeys { video: hs.video_key, cursor: hs.cursor_key });
    shared.set_session(SessionLink { id, outbox: out_tx.clone(), socket: stream.try_clone()? });

    out_tx
        .send(ControlMsg::Hello(Hello {
            proto_version: PROTO_VERSION,
            app_version: APP_VERSION.to_string(),
            os: "Windows".into(),
            host_name: shared.host_name.clone(),
            gpu_name: shared.gpu_name.lock().unwrap().clone(),
            hevc_decode_ok: shared.hevc_ok.load(Ordering::Relaxed),
            max_streams: annex_proto::MAX_STREAMS,
        }))
        .ok();

    // Writer: outbox, plus heartbeat, clock ping and stats every second.
    let wshared = shared.clone();
    let writer_thread = std::thread::Builder::new().name("annex-ctl-tx".into()).spawn(move || {
        let mut next_tick = Instant::now();
        loop {
            let timeout = next_tick.saturating_duration_since(Instant::now());
            match out_rx.recv_timeout(timeout) {
                Ok(m) => {
                    let bye = matches!(m, ControlMsg::Bye);
                    if writer.send(&m).is_err() || bye {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    next_tick += Duration::from_secs(1);
                    let msgs = [
                        ControlMsg::Heartbeat,
                        ControlMsg::ClockPing { t1: wshared.now_us() },
                        ControlMsg::Stats(wshared.stats_report()),
                    ];
                    if msgs.iter().any(|m| writer.send(m).is_err()) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = writer.get_mut().shutdown(std::net::Shutdown::Both);
    })?;

    let mut bye = false;
    let result = loop {
        let msg = match reader.recv() {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        match msg {
            ControlMsg::Hello(h) => {
                tracing::info!(host = %h.host_name, version = %h.app_version, chip = %h.gpu_name, "sender hello");
                shared.ui(UiCmd::SessionUp { peer_name: h.host_name });
            }
            ControlMsg::StreamConfig(cfg) => shared.ui(UiCmd::StartStream(cfg)),
            ControlMsg::StreamStop { stream_id } => shared.ui(UiCmd::StopStream { stream_id }),
            ControlMsg::ModeChange { stream_id, mode } => shared.ui(UiCmd::ModeChange { stream_id, mode }),
            ControlMsg::SenderStats(s) => shared.report_sender(s),
            ControlMsg::ClockPong { t1, t2, t3 } => shared.clock_add(t1, t2, t3, shared.now_us()),
            ControlMsg::ClockPing { t1 } => {
                let t = shared.now_us();
                let _ = out_tx.send(ControlMsg::ClockPong { t1, t2: t, t3: t });
            }
            ControlMsg::CursorShape(s) => shared.ui(UiCmd::CursorShape(Arc::new(s))),
            ControlMsg::MacDisplays(d) => shared.ui(UiCmd::MacDisplays(d)),
            ControlMsg::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source } => {
                shared.ui(UiCmd::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source })
            }
            ControlMsg::Warning { text, .. } => shared.ui(UiCmd::Warning(text)),
            ControlMsg::Bye => {
                bye = true;
                break Ok(());
            }
            ControlMsg::Heartbeat => {}
            other => tracing::debug!("ignoring {} from sender", other.kind()),
        }
    };
    drop(out_tx);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = writer_thread.join();
    if shared.clear_session(id) {
        let _ = udp.send(UdpCmd::ClearKeys);
        shared.ui(UiCmd::SessionDown { bye });
    }
    result
}

struct UdpStream {
    reasm: Reassembler,
    frames: Sender<FrameMsg>,
    ctl: Sender<PipeMsg>,
    last_stats: ReassemblyStats,
}

fn udp_loop(sock: UdpSocket, shared: Arc<Shared>, cmds: Receiver<UdpCmd>, recycle: Receiver<Vec<u8>>) {
    let mut buf = vec![0u8; MAX_DATAGRAM + 64];
    let mut keys: Option<([u8; 32], PacketCipher)> = None;
    let mut streams: HashMap<u8, UdpStream> = HashMap::new();
    let mut pool: Vec<Vec<u8>> = (0..8).map(|_| Vec::with_capacity(512 * 1024)).collect();
    let mut sender_addr: Option<SocketAddr> = None;
    let mut last_report = Instant::now();

    loop {
        while let Ok(c) = cmds.try_recv() {
            match c {
                UdpCmd::SetKeys { video, cursor } => {
                    keys = Some((video, PacketCipher::new(&cursor)));
                    streams.clear();
                    sender_addr = None;
                }
                UdpCmd::ClearKeys => {
                    keys = None;
                    streams.clear();
                }
                UdpCmd::AddStream { stream_id, frames, ctl } => {
                    if let Some((vk, _)) = &keys {
                        streams.insert(
                            stream_id,
                            UdpStream { reasm: Reassembler::new(stream_id, vk), frames, ctl, last_stats: ReassemblyStats::default() },
                        );
                    }
                }
                UdpCmd::RemoveStream { stream_id } => {
                    streams.remove(&stream_id);
                    shared.forget_stream(stream_id);
                }
            }
        }
        while let Ok(b) = recycle.try_recv() {
            if pool.len() < 16 {
                pool.push(b);
            }
        }

        let now;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                now = Instant::now();
                if !is_lan_socket(from) {
                    continue;
                }
                let d = &mut buf[..n];
                if d.first() == Some(&cursor::CURSOR_MAGIC) {
                    if let Some((_, cc)) = &keys {
                        if let Some(pos) = cursor::decode(d, cc) {
                            if let Some(s) = streams.get(&pos.stream_id) {
                                let _ = s.ctl.send(PipeMsg::Cursor(pos));
                            }
                        }
                    }
                    continue;
                }
                let Some(sid) = peek_stream_id(d) else { continue };
                let Some(s) = streams.get_mut(&sid) else { continue };
                if s.reasm.push(d, now).is_ok() && sender_addr != Some(from) {
                    tracing::info!(%from, "video from sender");
                    sender_addr = Some(from);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                now = Instant::now();
            }
            Err(e) => {
                tracing::warn!("udp recv: {e}");
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        }

        // Deliver completed frames, handle timeouts and IDR requests.
        for (sid, s) in streams.iter_mut() {
            loop {
                let mut out = pool.pop().unwrap_or_else(|| Vec::with_capacity(512 * 1024));
                match s.reasm.pop(&mut out, now) {
                    Some(del) => match s.frames.try_send(FrameMsg { stream_id: *sid, au: out, del }) {
                        Ok(()) => {}
                        Err(TrySendError::Full(m)) | Err(TrySendError::Disconnected(m)) => {
                            // Renderer is behind: drop and resync at the next IDR.
                            pool.push(m.au);
                            s.reasm.require_idr(now);
                        }
                    },
                    None => {
                        pool.push(out);
                        break;
                    }
                }
            }
            if let Some(last_good_seq) = s.reasm.idr_request(now) {
                shared.send(ControlMsg::IdrRequest { stream_id: *sid, last_good_seq });
            }
        }

        if now.duration_since(last_report) >= Duration::from_secs(1) {
            last_report = now;
            for (sid, s) in streams.iter_mut() {
                let st = s.reasm.stats();
                shared.report_net(*sid, st.delta(&s.last_stats), s.frames.len() as u8);
                s.last_stats = st;
            }
        }
    }
}
