//! UI thread: tray menu, monitor list, output windows, pairing, and the glue
//! between the session and the render threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_core::config::{self, MonitorPrefs, PairedPeer, ReceiverConfig};
use annex_proto::control::{ControlMsg, CursorShape, MacDisplay, ModePref, MonitorInfo, SourcePref, StreamConfig, VideoMode};
use annex_proto::discovery::{Advertiser, ReceiverAd};
use annex_proto::pairing::{PeerIdentity, random_pin};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, unbounded};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, VK_F12};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gpu::Gpu;
use crate::monitors::{self, MonitorDesc};
use crate::net::{self, Identity, Net, UdpCmd};
use crate::pipeline::{self, PipeMsg, PipelineHandle};
use crate::shared::{Pairing, Shared, WM_ANNEX_WAKE};
use crate::{misc, wnd};

/// Network loss: keep the windows (black) this long before unplugging (spec 3.3).
const GRACE: Duration = Duration::from_secs(30);
/// Pairing window (spec 8.2).
const PAIRING_TIME: Duration = Duration::from_secs(120);

pub enum UiCmd {
    SessionUp { peer_name: String },
    SessionDown { bye: bool },
    StartStream(StreamConfig),
    StopStream { stream_id: u8 },
    ModeChange { stream_id: u8, mode: VideoMode },
    CursorShape(Arc<CursorShape>),
    MacDisplays(Vec<MacDisplay>),
    SetMonitorPrefs { monitor_id: u64, enabled: Option<bool>, pinned_mode: Option<ModePref>, source: Option<SourcePref> },
    Paired(PeerIdentity),
    PairingEnded(Option<String>),
    Warning(String),
    Menu(String),
}

struct Output {
    hwnd: HWND,
    pipe: Option<PipelineHandle>,
    stream_id: Option<u8>,
    rect: RECT,
}

struct App {
    shared: Arc<Shared>,
    net: Net,
    cfg: ReceiverConfig,
    cfg_path: PathBuf,
    paired_key: Arc<Mutex<Option<[u8; 32]>>>,
    rx: Receiver<UiCmd>,
    tray: TrayIcon,
    monitors: Vec<MonitorDesc>,
    outputs: HashMap<u64, Output>,
    gpus: HashMap<(u32, i32), Arc<Gpu>>,
    session_peer: Option<String>,
    grace_until: Option<Instant>,
    mac_displays: Vec<MacDisplay>,
    pin: Option<(HWND, Instant)>,
    pin_close_at: Option<Instant>,
    advertiser: Option<Advertiser>,
    shapes: HashMap<u32, Arc<CursorShape>>,
    warning: Option<String>,
    quit: bool,
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    misc::init_logging();
    tracing::info!(version = annex_proto::APP_VERSION, "Annex receiver starting");
    let Some(_instance) = misc::single_instance() else {
        wnd::message_box("Annex is already running. Look for its icon in the notification area.");
        return Ok(());
    };
    crate::ffmpeg::quiet_logs();

    let cfg_path = misc::data_dir().join("config.toml");
    let mut cfg: ReceiverConfig = config::load_or_create(&cfg_path, ReceiverConfig::fresh).context("loading config")?;
    let host_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into());

    wnd::register_classes()?;
    let hidden = wnd::create_hidden()?;
    let (ui_tx, ui_rx) = unbounded();
    let shared = Arc::new(Shared::new(ui_tx, host_name.clone(), cfg.show_stats));
    shared.set_ui_hwnd(hidden);

    let monitors = monitors::enumerate();
    let mut gpus = HashMap::new();
    let primary_luid = monitors.first().map(|m| m.adapter_luid);
    match Gpu::new(primary_luid) {
        Ok(g) => {
            *shared.gpu_name.lock().unwrap() = g.name.clone();
            shared.hevc_ok.store(g.hevc_ok, std::sync::atomic::Ordering::Relaxed);
            if !g.hevc_ok {
                tracing::warn!("this GPU reports no D3D11 HEVC Main decoder");
            }
            gpus.insert((g.luid.LowPart, g.luid.HighPart), g);
        }
        Err(e) => tracing::error!("GPU init failed: {e:#}"),
    }

    let paired_key = Arc::new(Mutex::new(cfg.paired.as_ref().map(|p| p.public_key)));
    let ident = Identity {
        me: PeerIdentity { id: cfg.receiver_id.clone(), name: host_name.clone(), public_key: cfg.keys.public },
        keys: cfg.keys.clone(),
    };
    let net = match net::start(shared.clone(), ident, paired_key.clone(), cfg.control_port, cfg.video_port) {
        Ok(n) => n,
        Err(e) => {
            wnd::message_box(&format!(
                "Annex can't listen on ports {} (TCP) and {} (UDP): {e}",
                cfg.control_port, cfg.video_port
            ));
            return Err(e.into());
        }
    };
    let advertiser = Advertiser::start(ReceiverAd {
        id: cfg.receiver_id.clone(),
        name: host_name.clone(),
        control_port: cfg.control_port,
        video_port: cfg.video_port,
        pairing: false,
    })
    .map_err(|e| tracing::error!("mDNS advertise failed: {e}"))
    .ok();

    // Prefs for monitors seen for the first time.
    for (i, m) in monitors.iter().enumerate() {
        if !cfg.monitors.iter().any(|p| p.id == m.id) {
            cfg.monitors.push(MonitorPrefs {
                id: m.id,
                name: m.name.clone(),
                enabled: false,
                priority: i as u8,
                pinned_mode: ModePref::Auto,
                source: SourcePref::Extend,
            });
        }
    }
    let _ = config::save(&cfg_path, &cfg);

    {
        let shared = shared.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| shared.ui(UiCmd::Menu(e.id.0))));
    }
    let tray = TrayIconBuilder::new()
        .with_tooltip("Annex")
        .with_icon(tray_icon_image())
        .with_menu(Box::new(Menu::new()))
        .build()
        .context("creating tray icon")?;

    unsafe {
        let _ = RegisterHotKey(Some(hidden), 1, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, VK_F12.0 as u32);
        SetTimer(Some(hidden), 1, 250, None);
    }

    let mut app = App {
        shared,
        net,
        cfg,
        cfg_path,
        paired_key,
        rx: ui_rx,
        tray,
        monitors,
        outputs: HashMap::new(),
        gpus,
        session_peer: None,
        grace_until: None,
        mac_displays: Vec::new(),
        pin: None,
        pin_close_at: None,
        advertiser,
        shapes: HashMap::new(),
        warning: None,
        quit: false,
    };
    app.refresh_tray();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd == hidden {
                match msg.message {
                    WM_ANNEX_WAKE => app.drain(),
                    wnd::WM_ANNEX_DISPLAY_CHANGED => app.on_display_change(),
                    wnd::WM_ANNEX_CANCEL_PAIRING => app.stop_pairing(None),
                    WM_HOTKEY => app.toggle_stats(),
                    WM_TIMER => app.on_timer(),
                    _ => {}
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if app.quit {
                break;
            }
        }
    }
    app.shutdown();
    Ok(())
}

fn tray_icon_image() -> Icon {
    // A small monitor glyph: rounded frame with a stand, drawn procedurally.
    const S: u32 = 32;
    let mut px = vec![0u8; (S * S * 4) as usize];
    let mut put = |x: u32, y: u32, a: u8| {
        let o = ((y * S + x) * 4) as usize;
        px[o..o + 4].copy_from_slice(&[0x2b, 0x8c, 0xff, a]);
    };
    for y in 4..22 {
        for x in 2..30 {
            let edge = y < 6 || y > 19 || x < 4 || x > 27;
            put(x, y, if edge { 255 } else { 90 });
        }
    }
    for y in 22..26 {
        for x in 14..18 {
            put(x, y, 255);
        }
    }
    for y in 26..28 {
        for x in 9..23 {
            put(x, y, 255);
        }
    }
    Icon::from_rgba(px, S, S).expect("valid icon")
}

impl App {
    fn drain(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            self.handle(cmd);
        }
    }

    fn handle(&mut self, cmd: UiCmd) {
        match cmd {
            UiCmd::SessionUp { peer_name } => {
                tracing::info!(peer = %peer_name, "Mac connected");
                self.session_peer = Some(peer_name);
                self.grace_until = None;
                self.send_monitors();
                self.refresh_tray();
            }
            UiCmd::SessionDown { bye } => {
                tracing::info!(bye, "Mac disconnected");
                self.session_peer = None;
                self.mac_displays.clear();
                for o in self.outputs.values_mut() {
                    o.stream_id = None;
                }
                if bye {
                    self.close_all_outputs();
                } else if !self.outputs.is_empty() {
                    self.grace_until = Some(Instant::now() + GRACE);
                }
                self.refresh_tray();
            }
            UiCmd::StartStream(cfg) => self.start_stream(cfg),
            UiCmd::StopStream { stream_id } => {
                if let Some(id) = self.outputs.iter().find(|(_, o)| o.stream_id == Some(stream_id)).map(|(k, _)| *k) {
                    self.close_output(id);
                }
            }
            UiCmd::ModeChange { stream_id, mode } => {
                if let Some(o) = self.outputs.values().find(|o| o.stream_id == Some(stream_id)) {
                    if let Some(p) = &o.pipe {
                        p.send(PipeMsg::Mode(mode));
                    }
                }
            }
            UiCmd::CursorShape(s) => {
                for o in self.outputs.values() {
                    if let Some(p) = &o.pipe {
                        p.send(PipeMsg::CursorShape(s.clone()));
                    }
                }
                if self.shapes.len() > 64 {
                    self.shapes.clear();
                }
                self.shapes.insert(s.shape_id, s);
            }
            UiCmd::MacDisplays(d) => {
                self.mac_displays = d;
                self.refresh_tray();
            }
            UiCmd::SetMonitorPrefs { monitor_id, enabled, pinned_mode, source } => {
                self.update_prefs(monitor_id, |p| {
                    if let Some(e) = enabled {
                        p.enabled = e;
                    }
                    if let Some(m) = pinned_mode {
                        p.pinned_mode = m;
                    }
                    if let Some(s) = source.clone() {
                        p.source = s;
                    }
                });
            }
            UiCmd::Paired(peer) => {
                *self.paired_key.lock().unwrap() = Some(peer.public_key);
                self.cfg.paired = Some(PairedPeer::from(peer));
                self.save();
                self.show_pin_message("Paired", "Your Mac is paired. It connects automatically from now on.");
                self.stop_pairing_mode();
                self.refresh_tray();
            }
            UiCmd::PairingEnded(reason) => {
                self.stop_pairing_mode();
                if let Some(r) = reason {
                    self.show_pin_message("Not paired", &r);
                } else {
                    self.close_pin();
                }
            }
            UiCmd::Warning(t) => {
                tracing::warn!("sender warning: {t}");
                self.warning = Some(t);
                self.refresh_tray();
            }
            UiCmd::Menu(id) => self.on_menu(&id),
        }
    }

    fn prefs(&self, id: u64) -> Option<&MonitorPrefs> {
        self.cfg.monitors.iter().find(|p| p.id == id)
    }

    fn update_prefs(&mut self, id: u64, f: impl FnOnce(&mut MonitorPrefs)) {
        let Some(p) = self.cfg.monitors.iter_mut().find(|p| p.id == id) else { return };
        let before = p.clone();
        f(p);
        let after = p.clone();
        if before == after {
            return;
        }
        self.save();
        if !after.enabled {
            self.close_output(id);
        }
        self.send_monitors();
        self.refresh_tray();
    }

    fn save(&self) {
        if let Err(e) = config::save(&self.cfg_path, &self.cfg) {
            tracing::error!("saving config: {e}");
        }
    }

    fn monitor_infos(&self) -> Vec<MonitorInfo> {
        self.monitors
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let p = self.prefs(m.id);
                MonitorInfo {
                    id: m.id,
                    name: m.name.clone(),
                    native_w: m.width(),
                    native_h: m.height(),
                    refresh_mhz: m.refresh_mhz,
                    enabled: p.map(|p| p.enabled).unwrap_or(false),
                    priority: i as u8,
                    pinned_mode: p.map(|p| p.pinned_mode).unwrap_or_default(),
                    source: p.map(|p| p.source.clone()).unwrap_or_default(),
                    primary: m.primary,
                }
            })
            .collect()
    }

    fn send_monitors(&self) {
        self.shared.send(ControlMsg::Monitors(self.monitor_infos()));
    }

    fn gpu_for(&mut self, m: &MonitorDesc) -> Option<Arc<Gpu>> {
        let key = (m.adapter_luid.LowPart, m.adapter_luid.HighPart);
        if let Some(g) = self.gpus.get(&key) {
            return Some(g.clone());
        }
        match Gpu::new(Some(m.adapter_luid)) {
            Ok(g) => {
                self.gpus.insert(key, g.clone());
                Some(g)
            }
            Err(e) => {
                tracing::error!("GPU for {} failed: {e:#}", m.name);
                None
            }
        }
    }

    fn start_stream(&mut self, cfg: StreamConfig) {
        let Some(m) = self.monitors.iter().find(|m| m.id == cfg.monitor_id).cloned() else {
            tracing::warn!(monitor = cfg.monitor_id, "stream for unknown monitor");
            self.send_monitors();
            return;
        };
        if !self.prefs(m.id).map(|p| p.enabled).unwrap_or(false) {
            self.send_monitors();
            return;
        }
        if !self.outputs.contains_key(&m.id) {
            let Some(gpu) = self.gpu_for(&m) else { return };
            let hwnd = match wnd::create_output(m.rect) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("output window: {e}");
                    return;
                }
            };
            let pipe = pipeline::spawn(hwnd, (m.width(), m.height()), gpu, self.shared.clone(), self.net.recycle.clone());
            for s in self.shapes.values() {
                pipe.send(PipeMsg::CursorShape(s.clone()));
            }
            self.outputs.insert(m.id, Output { hwnd, pipe: Some(pipe), stream_id: None, rect: m.rect });
        }
        let out = self.outputs.get_mut(&m.id).unwrap();
        let pipe = out.pipe.as_ref().unwrap();
        if let Some(old) = out.stream_id.filter(|s| *s != cfg.stream_id) {
            let _ = self.net.udp.send(UdpCmd::RemoveStream { stream_id: old });
        }
        let _ = self.net.udp.send(UdpCmd::AddStream {
            stream_id: cfg.stream_id,
            frames: pipe.frames.clone(),
            ctl: pipe.ctl.clone(),
        });
        tracing::info!(stream = cfg.stream_id, monitor = %m.name, w = cfg.stream_w, h = cfg.stream_h, mode = cfg.mode.as_str(), "stream start");
        out.stream_id = Some(cfg.stream_id);
        pipe.send(PipeMsg::Attach(cfg));
        self.grace_until = None;
        self.update_power();
    }

    fn close_output(&mut self, monitor_id: u64) {
        if let Some(mut o) = self.outputs.remove(&monitor_id) {
            if let Some(s) = o.stream_id {
                let _ = self.net.udp.send(UdpCmd::RemoveStream { stream_id: s });
            }
            drop(o.pipe.take());
            wnd::destroy(o.hwnd);
        }
        self.update_power();
    }

    fn close_all_outputs(&mut self) {
        let ids: Vec<u64> = self.outputs.keys().copied().collect();
        for id in ids {
            self.close_output(id);
        }
    }

    fn update_power(&self) {
        misc::keep_display_awake(!self.outputs.is_empty());
    }

    fn on_display_change(&mut self) {
        let fresh = monitors::enumerate();
        let changed: Vec<u64> = self
            .outputs
            .iter()
            .filter(|(id, o)| {
                fresh.iter().find(|m| m.id == **id).map(|m| m.rect != o.rect).unwrap_or(true)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in changed {
            self.close_output(id);
        }
        for (i, m) in fresh.iter().enumerate() {
            if !self.cfg.monitors.iter().any(|p| p.id == m.id) {
                self.cfg.monitors.push(MonitorPrefs {
                    id: m.id,
                    name: m.name.clone(),
                    enabled: false,
                    priority: i as u8,
                    pinned_mode: ModePref::Auto,
                    source: SourcePref::Extend,
                });
            }
        }
        self.monitors = fresh;
        self.save();
        self.send_monitors();
        self.refresh_tray();
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        if let Some(t) = self.grace_until {
            if now >= t {
                tracing::info!("grace period over: unplugging");
                self.grace_until = None;
                self.close_all_outputs();
            }
        }
        if let Some(t) = self.pin_close_at {
            if now >= t {
                self.close_pin();
            }
        }
        if let Some((hwnd, until)) = self.pin {
            if self.pin_close_at.is_none() {
                if now >= until {
                    self.stop_pairing_mode();
                    self.show_pin_message("Pairing timed out", "Choose Pair with a Mac… in the Annex menu to try again.");
                } else {
                    let left = until - now;
                    wnd::set_pin_text(&self.pin_display(), &self.pin_hint(left));
                    wnd::invalidate(hwnd);
                }
            }
        }
    }

    fn pin_display(&self) -> String {
        let p = self.shared.pairing.lock().unwrap();
        p.as_ref().map(|p| format!("{} {}", &p.pin[..3], &p.pin[3..])).unwrap_or_default()
    }

    fn pin_hint(&self, left: Duration) -> String {
        format!(
            "Enter this PIN on your Mac: Annex menu > Pair with {}.\nExpires in {}:{:02}. Press Esc to cancel.",
            self.shared.host_name,
            left.as_secs() / 60,
            left.as_secs() % 60
        )
    }

    fn start_pairing(&mut self) {
        let pin = std::env::var("ANNEX_DEV_PIN").ok().filter(|p| p.len() == 6).unwrap_or_else(random_pin);
        let until = Instant::now() + PAIRING_TIME;
        *self.shared.pairing.lock().unwrap() = Some(Pairing { pin, until, attempts: 0 });
        self.advertise(true);
        let rect = self.monitors.iter().find(|m| m.primary).or(self.monitors.first()).map(|m| m.rect).unwrap_or(RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        });
        self.close_pin();
        let hint = self.pin_hint(PAIRING_TIME);
        match wnd::show_pin(rect, &self.pin_display(), &hint) {
            Ok(h) => self.pin = Some((h, until)),
            Err(e) => tracing::error!("PIN window: {e}"),
        }
    }

    fn stop_pairing_mode(&mut self) {
        *self.shared.pairing.lock().unwrap() = None;
        self.advertise(false);
    }

    fn stop_pairing(&mut self, reason: Option<&str>) {
        self.stop_pairing_mode();
        match reason {
            Some(r) => self.show_pin_message("Pairing stopped", r),
            None => self.close_pin(),
        }
    }

    fn show_pin_message(&mut self, title: &str, text: &str) {
        if let Some((hwnd, _)) = self.pin {
            wnd::set_pin_text(title, text);
            wnd::invalidate(hwnd);
            self.pin_close_at = Some(Instant::now() + Duration::from_secs(3));
        }
    }

    fn close_pin(&mut self) {
        if let Some((hwnd, _)) = self.pin.take() {
            wnd::destroy(hwnd);
        }
        self.pin_close_at = None;
    }

    fn advertise(&mut self, pairing: bool) {
        if let Some(a) = &mut self.advertiser {
            let _ = a.update(ReceiverAd {
                id: self.cfg.receiver_id.clone(),
                name: self.shared.host_name.clone(),
                control_port: self.cfg.control_port,
                video_port: self.cfg.video_port,
                pairing,
            });
        }
    }

    fn toggle_stats(&mut self) {
        let on = !self.shared.show_stats();
        self.shared.set_show_stats(on);
        self.cfg.show_stats = on;
        self.save();
        for o in self.outputs.values() {
            if let Some(p) = &o.pipe {
                p.send(PipeMsg::ShowStats(on));
            }
        }
        self.refresh_tray();
    }

    fn on_menu(&mut self, id: &str) {
        let parts: Vec<&str> = id.split(':').collect();
        match parts.as_slice() {
            ["mon", mid] => {
                if let Ok(mid) = mid.parse::<u64>() {
                    let on = !self.prefs(mid).map(|p| p.enabled).unwrap_or(false);
                    self.update_prefs(mid, |p| p.enabled = on);
                }
            }
            ["mode", mid, m] => {
                if let Ok(mid) = mid.parse::<u64>() {
                    let mode = match *m {
                        "sharp" => ModePref::Sharp,
                        "fast" => ModePref::Fast,
                        _ => ModePref::Auto,
                    };
                    self.update_prefs(mid, |p| p.pinned_mode = mode);
                }
            }
            ["src", mid, "extend"] => {
                if let Ok(mid) = mid.parse::<u64>() {
                    self.update_prefs(mid, |p| p.source = SourcePref::Extend);
                }
            }
            ["src", mid, "mirror", idx] => {
                if let (Ok(mid), Ok(idx)) = (mid.parse::<u64>(), idx.parse::<usize>()) {
                    if let Some(d) = self.mac_displays.get(idx) {
                        let uuid = d.uuid.clone();
                        self.update_prefs(mid, |p| p.source = SourcePref::Mirror { display_uuid: uuid });
                    }
                }
            }
            ["pair"] => self.start_pairing(),
            ["forget"] => {
                self.shared.send(ControlMsg::Bye);
                self.cfg.paired = None;
                *self.paired_key.lock().unwrap() = None;
                self.save();
                self.refresh_tray();
            }
            ["stats"] => self.toggle_stats(),
            ["autostart"] => {
                misc::set_autostart(!misc::autostart_enabled());
                self.refresh_tray();
            }
            ["firewall"] => misc::add_firewall_rule(),
            ["logs"] => misc::open_folder(&misc::log_dir()),
            ["quit"] => self.quit = true,
            _ => {}
        }
    }

    fn status_line(&self) -> String {
        match (&self.cfg.paired, &self.session_peer) {
            (None, _) => "Not paired with a Mac".into(),
            (Some(p), Some(_)) => format!("Connected to {}", p.name),
            (Some(p), None) => format!("Waiting for {}", p.name),
        }
    }

    fn refresh_tray(&mut self) {
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id("status", self.status_line(), false, None));
        if let Some(w) = &self.warning {
            let _ = menu.append(&MenuItem::with_id("warning", format!("⚠ {w}"), false, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        for m in &self.monitors {
            let p = self.prefs(m.id).cloned();
            let enabled = p.as_ref().map(|p| p.enabled).unwrap_or(false);
            let label = format!("{}  ({}×{})", m.name, m.width(), m.height());
            let _ = menu.append(&CheckMenuItem::with_id(format!("mon:{}", m.id), label, true, enabled, None));
        }
        let settings = Submenu::with_id("settings", "Display settings", true);
        for m in &self.monitors {
            let p = self.prefs(m.id).cloned();
            let pinned = p.as_ref().map(|p| p.pinned_mode).unwrap_or_default();
            let source = p.as_ref().map(|p| p.source.clone()).unwrap_or_default();
            let sub = Submenu::with_id(format!("set:{}", m.id), &m.name, true);
            let mode = Submenu::with_id(format!("modes:{}", m.id), "Mode", true);
            for (key, label, v) in [("auto", "Auto", ModePref::Auto), ("sharp", "Sharp (exact colour)", ModePref::Sharp), ("fast", "Fast", ModePref::Fast)] {
                let _ = mode.append(&CheckMenuItem::with_id(format!("mode:{}:{key}", m.id), label, true, pinned == v, None));
            }
            let src = Submenu::with_id(format!("srcs:{}", m.id), "Source", true);
            let _ = src.append(&CheckMenuItem::with_id(
                format!("src:{}:extend", m.id),
                "Extend (new display)",
                true,
                source == SourcePref::Extend,
                None,
            ));
            for (i, d) in self.mac_displays.iter().enumerate() {
                let on = matches!(&source, SourcePref::Mirror { display_uuid } if *display_uuid == d.uuid);
                let _ = src.append(&CheckMenuItem::with_id(format!("src:{}:mirror:{i}", m.id), format!("Mirror {}", d.name), true, on, None));
            }
            let _ = sub.append(&mode);
            let _ = sub.append(&src);
            let _ = settings.append(&sub);
        }
        let _ = menu.append(&settings);
        let _ = menu.append(&PredefinedMenuItem::separator());
        match &self.cfg.paired {
            None => {
                let _ = menu.append(&MenuItem::with_id("pair", "Pair with a Mac…", true, None));
            }
            Some(p) => {
                let _ = menu.append(&MenuItem::with_id("pair", "Pair with a different Mac…", true, None));
                let _ = menu.append(&MenuItem::with_id("forget", format!("Forget {}", p.name), true, None));
            }
        }
        let _ = menu.append(&CheckMenuItem::with_id("stats", "Stats overlay\tCtrl+Shift+F12", true, self.shared.show_stats(), None));
        let _ = menu.append(&CheckMenuItem::with_id("autostart", "Start at login", true, misc::autostart_enabled(), None));
        let _ = menu.append(&MenuItem::with_id("firewall", "Allow through Windows Firewall…", true, None));
        let _ = menu.append(&MenuItem::with_id("logs", "Open log folder", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "Quit Annex", true, None));
        self.tray.set_menu(Some(Box::new(menu)));
        let _ = self.tray.set_tooltip(Some(format!("Annex: {}", self.status_line())));
    }

    fn shutdown(&mut self) {
        self.shared.send(ControlMsg::Bye);
        std::thread::sleep(Duration::from_millis(150));
        self.close_all_outputs();
        self.close_pin();
        self.advertiser = None;
        misc::keep_display_awake(false);
        tracing::info!("Annex receiver stopped");
    }
}
