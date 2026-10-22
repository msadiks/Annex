use std::fs::{File, OpenOptions, TryLockError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use annex_proto::control::{ModePref, SourcePref, VideoMode};
use annex_sender::{ConnState, Engine, EngineHandle, EngineOptions, PairingState};
use crossbeam_channel::Sender;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWindow};
use serde::Serialize;
use tauri::menu::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{ActivationPolicy, AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::tray;

/// Passed by the login item, so a login start stays in the menu bar.
const AUTOSTART_ARG: &str = "--autostart";
const SETTINGS: &str = "settings";
const SETTINGS_ITEM: &str = "settings-window";

struct AppState {
    engine: EngineHandle,
    /// Wakes the menu bar menu after one of its settings was clicked.
    menu_poke: Sender<()>,
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
    /// Mirror takes the PC monitor's shape instead of fitting with bars.
    mirror_match: bool,
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
                mirror_match: s.mirror_match.contains(&m.id),
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
    let mode = mode.map(|m| tray::mode_pref(&m));
    let source = source.map(|s| if s == "extend" { SourcePref::Extend } else { SourcePref::Mirror { display_uuid: s } });
    state.engine.set_monitor_prefs(id, enabled, mode, source);
    let _ = state.menu_poke.try_send(());
    Ok(())
}

#[tauri::command]
fn set_mirror_match(state: State<AppState>, id: String, on: bool) -> Result<(), String> {
    let id: u64 = id.parse().map_err(|_| "bad monitor id".to_string())?;
    state.engine.set_mirror_match(id, on);
    let _ = state.menu_poke.try_send(());
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

/// Opens the settings window in front of everything. While it is open Annex
/// is a regular app, with a Dock icon and a place in Cmd+Tab.
fn show_settings(app: &AppHandle) {
    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    let window = match app.get_webview_window(SETTINGS) {
        Some(w) => w,
        None => match WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("index.html".into()))
            .title("Annex")
            .inner_size(560.0, 720.0)
            .min_inner_size(480.0, 520.0)
            .resizable(true)
            .build()
        {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("opening the settings window: {e}");
                return;
            }
        },
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    bring_to_front(&window);
}

/// `set_focus` alone leaves the window behind the active app when Annex was
/// a menu bar (accessory) app a moment ago.
fn bring_to_front(window: &WebviewWindow) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    #[allow(deprecated)] // `activate` alone is too polite to take focus from the active app.
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    if let Ok(ptr) = window.ns_window() {
        let ns_window = unsafe { &*(ptr as *const NSWindow) };
        ns_window.makeKeyAndOrderFront(None);
        ns_window.orderFrontRegardless();
    }
}

/// The app's own menu bar, shown while the settings window is open.
fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let about = AboutMetadata {
        name: Some("Annex".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: app.config().bundle.copyright.clone(),
        website: Some("https://github.com/msadiks/annex-app".into()),
        website_label: Some("github.com/msadiks/annex-app".into()),
        ..Default::default()
    };
    let annex = Submenu::with_items(
        app,
        "Annex",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About Annex"), Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, SETTINGS_ITEM, "Settings…", true, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some("Hide Annex"))?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, tray::QUIT, "Quit Annex", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    // Edit makes copy and paste work in the PIN field.
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[&PredefinedMenuItem::minimize(app, None)?, &PredefinedMenuItem::close_window(app, None)?],
    )?;
    Menu::with_items(app, &[&annex, &edit, &window])
}

fn on_menu(app: &AppHandle, e: MenuEvent) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let engine = &state.engine;
    match e.id.as_ref() {
        tray::OPEN | SETTINGS_ITEM => show_settings(app),
        tray::QUIT => {
            engine.shutdown();
            app.exit(0);
        }
        id => {
            let Some(action) = tray::action(id) else { return };
            match action {
                tray::Action::ToggleEnabled(m) => {
                    let on = engine.status().monitors.iter().find(|x| x.id == m).is_some_and(|x| !x.enabled);
                    engine.set_monitor_prefs(m, Some(on), None, None);
                }
                tray::Action::Mode(m, mode) => engine.set_monitor_prefs(m, None, Some(mode), None),
                tray::Action::Source(m, source) => engine.set_monitor_prefs(m, None, None, Some(source)),
                tray::Action::MirrorMatch(m, on) => engine.set_mirror_match(m, on),
            }
            let _ = state.menu_poke.try_send(());
        }
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
    let at_login = std::env::args().any(|a| a == AUTOSTART_ARG);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), at_login, "Annex starting");
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![AUTOSTART_ARG])))
        .menu(app_menu)
        .on_menu_event(on_menu)
        .on_window_event(|window, e| {
            // Back to a menu bar app once the window is gone.
            if window.label() == SETTINGS && matches!(e, WindowEvent::Destroyed) {
                let _ = window.app_handle().set_activation_policy(ActivationPolicy::Accessory);
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            pair,
            forget,
            set_monitor,
            set_mirror_match,
            set_client_cursor,
            set_autostart,
            request_screen_recording,
            open_screen_recording_settings
        ])
        .setup(move |app| {
            app.set_activation_policy(ActivationPolicy::Accessory);
            // Login items from 1.0 start without the flag; rewrite them.
            let login = app.autolaunch();
            if login.is_enabled().unwrap_or(false) {
                let _ = login.enable();
            }
            if !annex_mac::has_screen_recording() {
                annex_mac::request_screen_recording();
            }
            let backend = annex_mac::MacBackend::new()?;
            let engine = Engine::start(backend, EngineOptions::new(annex_mac::config_path(), annex_mac::host_name()))?;
            let (menu_poke, poke_rx) = crossbeam_channel::bounded(1);
            app.manage(AppState { engine: engine.clone(), menu_poke });

            let tray = TrayIconBuilder::with_id("annex")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .tooltip("Annex")
                .menu(&Menu::with_items(app, &[&MenuItem::new(app, "Starting…", false, None::<&str>)?])?)
                .show_menu_on_left_click(true)
                .build(app)?;
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("annex-menu".into())
                .spawn(move || tray::run(handle, tray, engine.clone(), poke_rx))?;

            // Opened by hand: show the window. At login only when Annex needs the user.
            let needs_user = app.state::<AppState>().engine.status().paired_id.is_none() || !annex_mac::has_screen_recording();
            if !at_login || needs_user {
                show_settings(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the app")
        .run(|app, event| match event {
            // Closing the settings window keeps the app in the menu bar.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Opening Annex again (Finder, Spotlight, Launchpad) shows the window.
            tauri::RunEvent::Reopen { .. } => show_settings(app),
            tauri::RunEvent::Exit => {
                if let Some(s) = app.try_state::<AppState>() {
                    s.engine.shutdown();
                }
            }
            _ => {}
        });
}
