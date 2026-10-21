use std::fs::{File, OpenOptions, TryLockError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use annex_proto::control::{ModePref, SourcePref, VideoMode};
use annex_sender::{ConnState, Engine, EngineHandle, EngineOptions, PairingState};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

struct AppState {
    engine: EngineHandle,
}

#[derive(Serialize)]
struct MonitorView {
    /// u64 ids don't survive JavaScript numbers, so they travel as strings.
    id: String,
    name: String,
    width: u32,
    height: u32,
    refresh_hz: f32,
    enabled: bool,
    primary: bool,
    mode: &'static str,
    /// "extend" or the UUID of the mirrored Mac display.
    source: String,
}

#[derive(Serialize)]
struct StreamView {
    monitor_id: String,
    monitor_name: String,
    width: u32,
    height: u32,
    mode: &'static str,
    mirror: bool,
    live: bool,
    bitrate_mbps: f32,
    fec_percent: u8,
    fps_sent: f32,
    fps_presented: f32,
    loss_percent: f32,
    encode_ms: f32,
    e2e_ms: Option<f32>,
    downgraded: bool,
    warning: Option<String>,
}

#[derive(Serialize)]
struct View {
    state: &'static str,
    paired_name: Option<String>,
    receiver_gpu: Option<String>,
    receiver_hevc_ok: bool,
    discovered: Vec<annex_sender::FoundReceiver>,
    monitors: Vec<MonitorView>,
    mac_displays: Vec<annex_proto::control::MacDisplay>,
    streams: Vec<StreamView>,
    client_cursor: bool,
    pairing: &'static str,
    pairing_text: Option<String>,
    message: Option<String>,
    screen_recording: bool,
    autostart: bool,
    version: &'static str,
}

fn mode_str(m: VideoMode) -> &'static str {
    match m {
        VideoMode::Sharp => "Sharp",
        VideoMode::Fast => "Fast",
    }
}

fn state_str(s: ConnState) -> &'static str {
    match s {
        ConnState::NotPaired => "not_paired",
        ConnState::Searching => "searching",
        ConnState::Connecting => "connecting",
        ConnState::Connected => "connected",
    }
}

#[tauri::command]
fn status(app: AppHandle, state: State<AppState>) -> View {
    let s = state.engine.status();
    let (pairing, pairing_text) = match s.pairing {
        PairingState::Idle => ("idle", None),
        PairingState::Working => ("working", None),
        PairingState::Done(n) => ("done", Some(n)),
        PairingState::Failed(e) => ("failed", Some(e)),
    };
    View {
        state: state_str(s.state),
        paired_name: s.paired_name,
        receiver_gpu: s.receiver_gpu,
        receiver_hevc_ok: s.receiver_hevc_ok,
        discovered: s.discovered,
        monitors: s
            .monitors
            .iter()
            .map(|m| MonitorView {
                id: m.id.to_string(),
                name: m.name.clone(),
                width: m.native_w,
                height: m.native_h,
                refresh_hz: m.refresh_mhz as f32 / 1000.0,
                enabled: m.enabled,
                primary: m.primary,
                mode: match m.pinned_mode {
                    ModePref::Auto => "auto",
                    ModePref::Sharp => "sharp",
                    ModePref::Fast => "fast",
                },
                source: match &m.source {
                    SourcePref::Extend => "extend".into(),
                    SourcePref::Mirror { display_uuid } => display_uuid.clone(),
                },
            })
            .collect(),
        mac_displays: s.mac_displays,
        streams: s
            .streams
            .iter()
            .map(|t| StreamView {
                monitor_id: t.monitor_id.to_string(),
                monitor_name: t.monitor_name.clone(),
                width: t.width,
                height: t.height,
                mode: mode_str(t.mode),
                mirror: t.mirror,
                live: t.live,
                bitrate_mbps: t.bitrate_kbps as f32 / 1000.0,
                fec_percent: t.fec_percent,
                fps_sent: t.fps_sent,
                fps_presented: t.fps_presented,
                loss_percent: t.loss_percent,
                encode_ms: t.encode_ms,
                e2e_ms: t.e2e_ms,
                downgraded: t.downgraded,
                warning: t.warning.clone(),
            })
            .collect(),
        client_cursor: s.client_cursor,
        pairing,
        pairing_text,
        message: s.message,
        screen_recording: annex_mac::has_screen_recording(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn pair(state: State<AppState>, receiver_id: Option<String>, pin: String) {
    state.engine.pair(receiver_id, pin);
}

#[tauri::command]
fn forget(state: State<AppState>) {
    state.engine.forget();
}

#[tauri::command]
fn set_monitor(
    state: State<AppState>,
    id: String,
    enabled: Option<bool>,
    mode: Option<String>,
    source: Option<String>,
) -> Result<(), String> {
    let id: u64 = id.parse().map_err(|_| "bad monitor id".to_string())?;
    let mode = mode.map(|m| match m.as_str() {
        "sharp" => ModePref::Sharp,
        "fast" => ModePref::Fast,
        _ => ModePref::Auto,
    });
    let source = source.map(|s| if s == "extend" { SourcePref::Extend } else { SourcePref::Mirror { display_uuid: s } });
    state.engine.set_monitor_prefs(id, enabled, mode, source);
    Ok(())
}

#[tauri::command]
fn set_client_cursor(state: State<AppState>, on: bool) {
    state.engine.set_client_cursor(on);
}

#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    let l = app.autolaunch();
    if on { l.enable() } else { l.disable() }.map_err(|e| e.to_string())
}

#[tauri::command]
fn request_screen_recording() -> bool {
    annex_mac::request_screen_recording()
}

#[tauri::command]
fn open_screen_recording_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Annex")
        .inner_size(560.0, 720.0)
        .min_inner_size(480.0, 520.0)
        .resizable(true)
        .build();
}

fn tray_title(engine: &EngineHandle) -> String {
    let s = engine.status();
    let live = s.streams.iter().filter(|t| t.live).count();
    match s.state {
        ConnState::NotPaired => "Not paired with a PC".into(),
        ConnState::Searching => format!("Looking for {}…", s.paired_name.unwrap_or_else(|| "the PC".into())),
        ConnState::Connecting => format!("Connecting to {}…", s.paired_name.unwrap_or_else(|| "the PC".into())),
        ConnState::Connected if live > 0 => format!(
            "Streaming {live} display{} to {}",
            if live == 1 { "" } else { "s" },
            s.paired_name.unwrap_or_default()
        ),
        ConnState::Connected => format!("Connected to {}", s.paired_name.unwrap_or_default()),
    }
}

fn init_logging() {
    let dir = annex_mac::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("annex.log");
    if std::fs::metadata(&path).map(|m| m.len() > 5 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("annex.old.log"));
    }
    let filter = tracing_subscriber::EnvFilter::try_from_env("ANNEX_LOG").unwrap_or_else(|_| "info".into());
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(f)).init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

/// The PC takes one session at a time, so two copies of Annex would keep
/// kicking each other off. Waits a moment in case the last copy is quitting.
fn another_copy_running() -> bool {
    static LOCK: OnceLock<File> = OnceLock::new();
    let path = annex_mac::config_path().with_file_name("annex.lock");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(file) = OpenOptions::new().create(true).truncate(false).write(true).open(&path) else {
        return false;
    };
    for _ in 0..30 {
        match file.try_lock() {
            Ok(()) => {
                let _ = LOCK.set(file);
                return false;
            }
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(100)),
            Err(TryLockError::Error(_)) => return false,
        }
    }
    true
}

pub fn run() {
    init_logging();
    if another_copy_running() {
        tracing::info!("Annex is already running");
        return;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Annex starting");
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            status,
            pair,
            forget,
            set_monitor,
            set_client_cursor,
            set_autostart,
            request_screen_recording,
            open_screen_recording_settings
        ])
        .setup(|app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            if !annex_mac::has_screen_recording() {
                annex_mac::request_screen_recording();
            }
            let backend = annex_mac::MacBackend::new()?;
            let engine = Engine::start(backend, EngineOptions::new(annex_mac::config_path(), annex_mac::host_name()))?;
            app.manage(AppState { engine: engine.clone() });

            let status_item = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Open Annex…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Annex", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&status_item, &PredefinedMenuItem::separator(app)?, &open, &quit])?;
            let quit_engine = engine.clone();
            TrayIconBuilder::with_id("annex")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .tooltip("Annex")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, e| match e.id.as_ref() {
                    "open" => show_settings(app),
                    "quit" => {
                        quit_engine.shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            let poll = engine.clone();
            std::thread::spawn(move || {
                let mut last = String::new();
                loop {
                    let t = tray_title(&poll);
                    if t != last {
                        let _ = status_item.set_text(&t);
                        last = t;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            });

            if engine.status().paired_id.is_none() || !annex_mac::has_screen_recording() {
                show_settings(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the app")
        .run(|app, event| match event {
            // Closing the settings window keeps the app in the menu bar.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            tauri::RunEvent::Exit => {
                if let Some(s) = app.try_state::<AppState>() {
                    s.engine.shutdown();
                }
            }
            _ => {}
        });
}
