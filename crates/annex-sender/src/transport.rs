//! Packetizer thread and paced UDP send thread (spec 5.2, 8.5): one of each
//! for all streams.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_proto::packetizer::{FrameInput, PacketBatch, Packetizer};
use crossbeam_channel::{Receiver, Sender, bounded, select};
use socket2::{Domain, Protocol, Socket, Type};

use crate::sink::EncodedFrame;

/// A frame's packets are spread over about this long (helps Wi-Fi).
const PACING_WINDOW: Duration = Duration::from_micros(2000);
const PACING_SLICES: usize = 8;

pub(crate) enum TransportCmd {
    /// New session: fresh key (sequence numbers restart with it) and destination.
    Session { video_key: [u8; 32], dest: SocketAddr },
    /// Disconnected: drop frames until the next session.
    Idle,
}

/// Per-stream FEC percentage, set by the engine (ABR).
#[derive(Default)]
pub(crate) struct FecTable {
    pub pct: Mutex<HashMap<u8, Arc<AtomicU8>>>,
}

impl FecTable {
    pub fn set(&self, stream_id: u8, pct: u8) {
        self.pct
            .lock()
            .unwrap()
            .entry(stream_id)
            .or_insert_with(|| Arc::new(AtomicU8::new(pct)))
            .store(pct, Ordering::Relaxed);
    }

    fn get(&self, stream_id: u8) -> u8 {
        self.pct
            .lock()
            .unwrap()
            .get(&stream_id)
            .map(|a| a.load(Ordering::Relaxed))
            .unwrap_or(annex_core::abr::DEFAULT_FEC)
    }
}

/// Bytes sent per stream since the last read.
#[derive(Default)]
pub(crate) struct SendMeters {
    pub bytes: Mutex<HashMap<u8, u64>>,
}

pub(crate) struct Transport {
    pub cmd: Sender<TransportCmd>,
    pub socket: Arc<UdpSocket>,
}

pub(crate) fn start(
    frames: Receiver<EncodedFrame>,
    recycle: Sender<Vec<u8>>,
    fec: Arc<FecTable>,
    meters: Arc<SendMeters>,
) -> std::io::Result<Transport> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = s.set_send_buffer_size(4 * 1024 * 1024);
    s.bind(&SocketAddr::from(([0, 0, 0, 0], 0)).into())?;
    let socket: Arc<UdpSocket> = Arc::new(s.into());

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<TransportCmd>();
    let (batch_tx, batch_rx) = bounded::<(PacketBatch, Option<SocketAddr>)>(6);
    let (batch_ret_tx, batch_ret_rx) = bounded::<PacketBatch>(8);
    for _ in 0..6 {
        let _ = batch_ret_tx.send(PacketBatch::with_capacity(256));
    }

    std::thread::Builder::new().name("annex-packetize".into()).spawn(move || {
        let mut pk: Option<(Packetizer, SocketAddr)> = None;
        loop {
            select! {
                recv(cmd_rx) -> c => match c {
                    Ok(TransportCmd::Session { video_key, dest }) => pk = Some((Packetizer::new(&video_key), dest)),
                    Ok(TransportCmd::Idle) => pk = None,
                    Err(_) => return,
                },
                recv(frames) -> f => {
                    let Ok(f) = f else { return };
                    if let Some((p, dest)) = &mut pk {
                        let mut batch = batch_ret_rx.try_recv().unwrap_or_default();
                        let input = FrameInput {
                            stream_id: f.stream_id,
                            au: &f.data,
                            idr: f.idr,
                            capture_ts: f.capture_us as u32,
                            fec_percent: fec.get(f.stream_id),
                        };
                        match p.packetize(input, &mut batch) {
                            Ok(_) => { let _ = batch_tx.send((batch, Some(*dest))); }
                            Err(e) => tracing::warn!("packetize: {e}"),
                        }
                    }
                    let _ = recycle.try_send(f.data);
                }
            }
        }
    })?;

    let sock = socket.clone();
    std::thread::Builder::new().name("annex-send".into()).spawn(move || {
        while let Ok((batch, dest)) = batch_rx.recv() {
            if let Some(dest) = dest {
                let n = batch.len();
                let per_slice = n.div_ceil(PACING_SLICES).max(1);
                let start = Instant::now();
                for (i, d) in batch.iter().enumerate() {
                    if i > 0 && i % per_slice == 0 && n > PACING_SLICES {
                        let due = start + PACING_WINDOW.mul_f64((i / per_slice) as f64 / PACING_SLICES as f64);
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        }
                    }
                    if let Err(e) = sock.send_to(d, dest) {
                        if e.kind() != std::io::ErrorKind::WouldBlock {
                            tracing::debug!("udp send: {e}");
                        }
                    }
                }
                *meters.bytes.lock().unwrap().entry(batch.stream_id).or_default() += batch.wire_bytes() as u64;
            }
            let _ = batch_ret_tx.try_send(batch);
        }
    })?;

    Ok(Transport { cmd: cmd_tx, socket })
}
