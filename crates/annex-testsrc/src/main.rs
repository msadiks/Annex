//! Test sender. Lets the Windows receiver be checked without a Mac.
//!
//! ```text
//! annex-testsrc gen --size 1920x1080 --mode sharp --out media
//! annex-testsrc run --addr 192.168.1.20 --media media --pin 123456
//! ```
//!
//! `gen` renders a test pattern (moving bar, 1-pixel coloured stripes,
//! coloured text), packs it exactly like the Mac does and encodes it with
//! FFmpeg/libx265. `run` pairs (if `--pin` is given) and plays the files to
//! the receiver through the real sender engine.

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_core::pack::{pack_fast, pack_sharp};
use annex_proto::control::{CursorShape, MacDisplay, ModePref, VideoMode};
use annex_sender::{Backend, CursorSink, Engine, EngineOptions, FrameSink, StreamRequest};
use anyhow::{Context, bail};

fn usage() -> ! {
    eprintln!(
        "usage:\n  annex-testsrc gen --size WxH --mode sharp|fast --out DIR [--frames N] [--kbps N]\n  \
         annex-testsrc run --addr HOST[:PORT] --media DIR [--pin PIN] [--enable 0,1] [--pin-mode sharp|fast]\n                           \
         [--video-port N] [--config FILE] [--seconds N]"
    );
    std::process::exit(2)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("ANNEX_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    let opt = |name: &str| -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    match cmd.as_str() {
        "gen" => {
            let size = opt("--size").unwrap_or_else(|| usage());
            let (w, h) = size.split_once('x').unwrap_or_else(|| usage());
            let (w, h): (u32, u32) = (w.parse()?, h.parse()?);
            let mode = parse_mode(&opt("--mode").unwrap_or_else(|| usage()))?;
            let out = PathBuf::from(opt("--out").unwrap_or_else(|| usage()));
            let frames = opt("--frames").map(|v| v.parse()).transpose()?.unwrap_or(120);
            let kbps = opt("--kbps").map(|v| v.parse()).transpose()?;
            generate(w, h, mode, frames, kbps, &out)
        }
        "run" => {
            let addr = opt("--addr").unwrap_or_else(|| usage());
            let addr = resolve(&addr)?;
            let media = PathBuf::from(opt("--media").unwrap_or_else(|| usage()));
            let config = PathBuf::from(opt("--config").unwrap_or_else(|| "annex-testsrc.toml".into()));
            let mut o = EngineOptions::new(config, "Annex test sender".into());
            o.manual_receiver = Some(addr);
            if let Some(p) = opt("--video-port") {
                o.manual_video_port = p.parse()?;
            }
            let seconds: Option<u64> = opt("--seconds").map(|v| v.parse()).transpose()?;
            let enable: Vec<usize> = opt("--enable")
                .map(|v| v.split(',').map(|i| i.trim().parse()).collect::<Result<_, _>>())
                .transpose()?
                .unwrap_or_default();
            let pin_mode = opt("--pin-mode").map(|m| parse_mode(&m)).transpose()?;
            run(o, media, opt("--pin"), &enable, pin_mode, seconds)
        }
        _ => usage(),
    }
}

fn parse_mode(s: &str) -> anyhow::Result<VideoMode> {
    match s {
        "sharp" => Ok(VideoMode::Sharp),
        "fast" => Ok(VideoMode::Fast),
        _ => bail!("mode must be sharp or fast"),
    }
}

fn resolve(s: &str) -> anyhow::Result<SocketAddr> {
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, annex_proto::DEFAULT_CONTROL_PORT));
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{}", annex_proto::DEFAULT_CONTROL_PORT) };
    with_port.to_socket_addrs()?.find(|a| a.is_ipv4()).context("could not resolve the receiver address")
}

fn media_name(w: u32, h: u32, mode: VideoMode) -> String {
    format!("{w}x{h}-{}.hevc", mode.as_str().to_ascii_lowercase())
}

// ----- gen ----------------------------------------------------------------

fn generate(w: u32, h: u32, mode: VideoMode, frames: u32, kbps: Option<u32>, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let path = out.join(media_name(w, h, mode));
    let (ew, eh) = match mode {
        VideoMode::Sharp => (w * 2, h * 2),
        VideoMode::Fast => (w, h),
    };
    let kbps = kbps.unwrap_or_else(|| annex_core::modes::default_bitrate_kbps(w, h, mode));
    let params = "keyint=30:min-keyint=30:scenecut=0:open-gop=0:bframes=0:aud=1:repeat-headers=1:\
                  range=full:colorprim=bt709:transfer=bt709:colormatrix=bt709:log-level=error";
    let mut ff = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "nv12"])
        .args(["-s", &format!("{ew}x{eh}"), "-r", "60", "-i", "-"])
        .args(["-c:v", "libx265", "-preset", "veryfast", "-tune", "zerolatency", "-x265-params", params])
        .args(["-b:v", &format!("{kbps}k"), "-pix_fmt", "yuv420p", "-f", "hevc"])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .context("running ffmpeg (needs ffmpeg with libx265 on PATH)")?;
    let mut stdin = ff.stdin.take().unwrap();
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    for n in 0..frames {
        draw_pattern(&mut bgra, w as usize, h as usize, n, mode);
        let nv = match mode {
            VideoMode::Sharp => pack_sharp(&bgra, w as usize, h as usize, w as usize * 4),
            VideoMode::Fast => pack_fast(&bgra, w as usize, h as usize, w as usize * 4),
        };
        stdin.write_all(&nv.y)?;
        stdin.write_all(&nv.uv)?;
    }
    drop(stdin);
    if !ff.wait()?.success() {
        bail!("ffmpeg failed");
    }
    println!("wrote {}", path.display());
    Ok(())
}

fn fill(buf: &mut [u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 3]) {
    let h = buf.len() / (w * 4);
    for y in y0.min(h)..y1.min(h) {
        for x in x0.min(w)..x1.min(w) {
            let i = (y * w + x) * 4;
            buf[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
}

fn text(buf: &mut [u8], w: usize, x: usize, y: usize, scale: usize, s: &str, fg: [u8; 3]) {
    for (ci, ch) in s.chars().enumerate() {
        let glyph = font8x8::legacy::BASIC_LEGACY.get(ch as usize).copied().unwrap_or([0; 8]);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    let px = x + (ci * 8 + col) * scale;
                    let py = y + row * scale;
                    fill(buf, w, px, py, px + scale, py + scale, fg);
                }
            }
        }
    }
}

/// A pattern where Sharp and Fast look clearly different: 1-pixel red/blue
/// stripes and small coloured text smear in 4:2:0 but stay crisp in Sharp.
fn draw_pattern(buf: &mut [u8], w: usize, h: usize, n: u32, mode: VideoMode) {
    for y in 0..h {
        let g = (24 + y * 40 / h) as u8;
        for x in 0..w {
            let i = (y * w + x) * 4;
            buf[i..i + 4].copy_from_slice(&[g + 10, g, g, 255]);
        }
    }
    // 1-pixel grid every 64 px.
    for x in (0..w).step_by(64) {
        fill(buf, w, x, 0, x + 1, h, [70, 70, 70]);
    }
    for y in (0..h).step_by(64) {
        fill(buf, w, 0, y, w, y + 1, [70, 70, 70]);
    }
    // Chroma test: alternating 1-px red and blue columns, then rows.
    let (bx, by, bw, bh) = (w / 16, h / 3, w / 4, h / 6);
    for x in bx..bx + bw {
        let c = if (x - bx) % 2 == 0 { [255, 0, 0] } else { [0, 0, 255] };
        fill(buf, w, x, by, x + 1, by + bh / 2, c);
    }
    for y in by + bh / 2..by + bh {
        let c = if (y - by) % 2 == 0 { [0, 200, 0] } else { [255, 0, 255] };
        fill(buf, w, bx, y, bx + bw, y + 1, c);
    }
    // Coloured text, small and large.
    let tx = w / 16 + w / 4 + 40;
    fill(buf, w, tx - 8, by - 8, w - w / 16, by + 90, [250, 250, 250]);
    text(buf, w, tx, by, 1, "Sharp keeps this red text crisp; Fast smears it.", [220, 0, 0]);
    text(buf, w, tx, by + 14, 1, "Blue text on white: the quick brown fox 0123456789", [0, 0, 230]);
    text(buf, w, tx, by + 30, 2, "Annex test pattern", [0, 140, 0]);
    let mode_s = mode.as_str();
    text(buf, w, w / 16, h / 10, 4, &format!("Annex {w}x{h} {mode_s}"), [240, 240, 240]);
    text(buf, w, w / 16, h / 10 + 48, 3, &format!("frame {n:03}"), [255, 200, 40]);
    // Moving bar: one sweep per 2 s (shows stutter and tearing).
    let period = 120;
    let bar_x = (n % period) as usize * (w - 24) / period as usize;
    fill(buf, w, bar_x, h * 2 / 3, bar_x + 24, h * 2 / 3 + h / 6, [255, 255, 255]);
    fill(buf, w, 0, h - 4, (n % period) as usize * w / period as usize, h, [0, 160, 255]);
}

// ----- run ----------------------------------------------------------------

struct Clip {
    aus: Vec<Vec<u8>>,
    idr: Vec<bool>,
}

/// Splits an Annex B stream at access unit delimiters (x265 `aud=1`).
fn load_clip(path: &Path) -> anyhow::Result<Clip> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let nal_type = (data[i + 3] >> 1) & 0x3f;
            let at = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((at, nal_type));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut clip = Clip { aus: Vec::new(), idr: Vec::new() };
    let aud: Vec<usize> = starts.iter().filter(|(_, t)| *t == 35).map(|(a, _)| *a).collect();
    if aud.is_empty() {
        bail!("{} has no access unit delimiters; regenerate it with `gen`", path.display());
    }
    for (k, &a) in aud.iter().enumerate() {
        let end = aud.get(k + 1).copied().unwrap_or(data.len());
        let irap = starts.iter().any(|(p, t)| *p >= a && *p < end && (16..=21).contains(t));
        clip.aus.push(data[a..end].to_vec());
        clip.idr.push(irap);
    }
    Ok(clip)
}

struct Player {
    stop: Arc<AtomicBool>,
    idr: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TestBackend {
    media: PathBuf,
    players: Arc<Mutex<HashMap<u8, Player>>>,
    sizes: Arc<Mutex<HashMap<u8, (u32, u32)>>>,
    cursor_stop: Arc<AtomicBool>,
}

impl Backend for TestBackend {
    fn mac_displays(&mut self) -> Vec<MacDisplay> {
        vec![MacDisplay { uuid: "test-display".into(), name: "Test display".into(), width: 1920, height: 1080, builtin: true }]
    }

    fn start_stream(&mut self, req: &StreamRequest, sink: FrameSink) -> anyhow::Result<()> {
        self.stop_stream(req.stream_id);
        let path = self.media.join(media_name(req.width, req.height, req.mode));
        if !path.exists() {
            bail!(
                "missing {}: run `annex-testsrc gen --size {}x{} --mode {} --out {}`",
                path.display(),
                req.width,
                req.height,
                req.mode.as_str().to_ascii_lowercase(),
                self.media.display()
            );
        }
        let clip = load_clip(&path)?;
        tracing::info!(stream = req.stream_id, file = %path.display(), frames = clip.aus.len(), "playing");
        let stop = Arc::new(AtomicBool::new(false));
        let idr = Arc::new(AtomicBool::new(false));
        let (s2, i2) = (stop.clone(), idr.clone());
        std::thread::spawn(move || play(clip, sink, s2, i2));
        self.players.lock().unwrap().insert(req.stream_id, Player { stop, idr });
        self.sizes.lock().unwrap().insert(req.stream_id, (req.width, req.height));
        Ok(())
    }

    fn stop_stream(&mut self, stream_id: u8) {
        if let Some(p) = self.players.lock().unwrap().remove(&stream_id) {
            p.stop.store(true, Ordering::Relaxed);
        }
        self.sizes.lock().unwrap().remove(&stream_id);
    }

    fn request_idr(&mut self, stream_id: u8) {
        if let Some(p) = self.players.lock().unwrap().get(&stream_id) {
            p.idr.store(true, Ordering::Relaxed);
        }
    }

    fn set_bitrate(&mut self, stream_id: u8, kbps: u32) {
        tracing::info!(stream = stream_id, kbps, "bitrate change (ignored by the test sender)");
    }

    fn set_client_cursor(&mut self, on: bool, sink: Option<CursorSink>) {
        self.cursor_stop.store(true, Ordering::Relaxed);
        let (true, Some(sink)) = (on, sink) else { return };
        let stop = Arc::new(AtomicBool::new(false));
        self.cursor_stop = stop.clone();
        let sizes = self.sizes.clone();
        std::thread::spawn(move || {
            sink.shape(arrow_shape());
            let start = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let t = start.elapsed().as_secs_f64();
                for (id, (w, h)) in sizes.lock().unwrap().iter() {
                    let x = *w as f64 / 2.0 + t.cos() * *w as f64 / 4.0;
                    let y = *h as f64 / 2.0 + t.sin() * *h as f64 / 4.0;
                    sink.position(*id, x as i32, y as i32, true, 1);
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        });
    }

    fn machine_name(&self) -> String {
        "Test sender".into()
    }
}

fn arrow_shape() -> CursorShape {
    let (w, h) = (16u16, 24u16);
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            if x <= y * 2 / 3 && y < 20 {
                let edge = x == 0 || x == y * 2 / 3 || y == 19;
                let c = if edge { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
                rgba[(y * w as usize + x) * 4..][..4].copy_from_slice(&c);
            }
        }
    }
    CursorShape { shape_id: 1, width: w, height: h, hot_x: 0, hot_y: 0, rgba }
}

fn play(clip: Clip, sink: FrameSink, stop: Arc<AtomicBool>, idr: Arc<AtomicBool>) {
    let frame = Duration::from_nanos(1_000_000_000 / 60);
    let start = Instant::now();
    let mut n: u64 = 0;
    let mut i = 0usize;
    while !stop.load(Ordering::Relaxed) && sink.is_current() {
        if idr.swap(false, Ordering::Relaxed) {
            // Jump to the next keyframe (the clip has one every 30 frames).
            i = (i..clip.aus.len()).chain(0..i).find(|k| clip.idr[*k]).unwrap_or(0);
        }
        let now = Instant::now();
        sink.note_captured();
        sink.push(&clip.aus[i], clip.idr[i], now, 0.0, 0.0);
        i = (i + 1) % clip.aus.len();
        n += 1;
        let due = start + frame * n as u32;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }
}

fn run(
    opts: EngineOptions,
    media: PathBuf,
    pin: Option<String>,
    enable: &[usize],
    pin_mode: Option<VideoMode>,
    seconds: Option<u64>,
) -> anyhow::Result<()> {
    let backend = TestBackend {
        media,
        players: Arc::default(),
        sizes: Arc::default(),
        cursor_stop: Arc::new(AtomicBool::new(false)),
    };
    let engine = Engine::start(backend, opts)?;
    if let Some(pin) = pin
        && engine.status().paired_id.is_none()
    {
        engine.pair(None, pin);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            match engine.status().pairing {
                annex_sender::PairingState::Done(name) => {
                    println!("paired with {name}");
                    break;
                }
                annex_sender::PairingState::Failed(e) => bail!("pairing failed: {e}"),
                _ => {}
            }
        }
    }
    // Turn on PC monitors through the protocol, like the Mac menu does.
    if !enable.is_empty() || pin_mode.is_some() {
        let wait = Instant::now() + Duration::from_secs(15);
        while engine.status().monitors.is_empty() && Instant::now() < wait {
            std::thread::sleep(Duration::from_millis(200));
        }
        let monitors = engine.status().monitors;
        for (i, m) in monitors.iter().enumerate() {
            println!("monitor {i}: {} {}x{} @{:.2} Hz{}", m.name, m.native_w, m.native_h, m.refresh_mhz as f64 / 1000.0, if m.primary { " (primary)" } else { "" });
        }
        let mode = pin_mode.map(|m| match m {
            VideoMode::Sharp => ModePref::Sharp,
            VideoMode::Fast => ModePref::Fast,
        });
        for &i in enable {
            match monitors.get(i) {
                Some(m) => engine.set_monitor_prefs(m.id, Some(true), mode, None),
                None => eprintln!("no monitor {i}"),
            }
        }
    }
    let until = seconds.map(|s| Instant::now() + Duration::from_secs(s));
    let mut last = String::new();
    while until.is_none_or(|u| Instant::now() < u) {
        std::thread::sleep(Duration::from_secs(2));
        let s = engine.status();
        let mut line = format!("{:?}", s.state);
        for st in &s.streams {
            line += &format!(
                " | #{} {} {}x{} {} live={} sent={:.0}fps shown={:.0}fps loss={:.2}% e2e={}",
                st.stream_id,
                st.monitor_name,
                st.width,
                st.height,
                st.mode.as_str(),
                st.live,
                st.fps_sent,
                st.fps_presented,
                st.loss_percent,
                st.e2e_ms.map(|v| format!("{v:.1}ms")).unwrap_or("-".into())
            );
        }
        if let Some(m) = &s.message {
            line += &format!(" | {m}");
        }
        if line != last {
            println!("{line}");
            last = line;
        }
    }
    engine.shutdown();
    Ok(())
}
