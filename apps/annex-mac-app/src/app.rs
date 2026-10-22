use std::fs::{File, OpenOptions, TryLockError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use annex_proto::control::{ModePref, SourcePref, VideoMode};
use annex_sender::{ConnState, Engine, EngineHandle, EngineOptions, PairingState};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWindow};
use serde::Serialize;
use tauri::menu::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::window::Color;
use tauri::{
    ActivationPolicy, AppHandle, Manager, State, Theme, TitleBarStyle, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::{popover, tray};

/// Passed by the login item, so a login start stays in the menu bar.
const AUTOSTART_ARG: &str = "--autostart";
pub const SETTINGS: &str = "settings";
const SETTINGS_ITEM: &str = "settings-window";
const QUIT_ITEM: &str = "quit";
const ISSUES_URL: &str = "https://github.com/msadiks/annex-app/issues/new/choose";

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
    paired_id: Option<String>,
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
    /// Seconds left before the displays kept for a lost PC are unplugged.
    grace_secs: Option<u64>,
    kept_displays: usize,
    /// The PC's IP address and ports while connected.
    address: Option<String>,
    ports: Option<String>,
    mac_name: String,
    log_path: String,
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
        paired_id: s.paired_id,
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
        grace_secs: s.grace_until.map(|t| t.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u64),
        kept_displays: s.streams.len(),
        address: s.peer_addr.map(|a| a.ip().to_string()),
        ports: s.peer_addr.map(|a| match s.video_addr {
            Some(v) => format!("TCP {} · UDP {}", a.port(), v.port()),
            None => format!("TCP {}", a.port()),
        }),
        mac_name: annex_mac::host_name(),
        log_path: annex_mac::log_dir().join("annex.log").display().to_string(),
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
    Ok(())
}

#[tauri::command]
fn set_mirror_match(state: State<AppState>, id: String, on: bool) -> Result<(), String> {
    let id: u64 = id.parse().map_err(|_| "bad monitor id".to_string())?;
    state.engine.set_mirror_match(id, on);
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

fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[tauri::command]
fn open_displays_settings() {
    open_url("x-apple.systempreferences:com.apple.Displays-Settings.extension");
}

#[tauri::command]
fn reveal_log() {
    let log = annex_mac::log_dir().join("annex.log");
    let _ = std::process::Command::new("open").arg("-R").arg(log).spawn();
}

#[tauri::command]
fn report_problem() {
    open_url(ISSUES_URL);
}

/// Opens the window from the popover, on `page` when given.
#[tauri::command]
fn open_annex(app: AppHandle, page: Option<String>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        popover::hide(&handle, true);
        show_settings(&handle, page.as_deref());
    });
}

#[tauri::command]
fn popover_size(app: AppHandle, height: f64) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || popover::set_height(&handle, height));
}

#[tauri::command]
fn popover_hide(app: AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || popover::hide(&handle, false));
}

#[tauri::command]
fn quit(app: AppHandle, state: State<AppState>) {
    state.engine.shutdown();
    app.exit(0);
}

/// Opens the settings window in front of everything, on `page` when given.
/// While it is open Annex is a regular app, with a Dock icon and a place in
/// Cmd+Tab.
fn show_settings(app: &AppHandle, page: Option<&str>) {
    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    let window = match app.get_webview_window(SETTINGS) {
        Some(w) => {
            if let Some(p) = page {
                let _ = w.eval(format!("window.annexGo && annexGo({})", serde_json::to_string(p).unwrap_or_default()));
            }
            w
        }
        None => match WebviewWindowBuilder::new(
            app,
            SETTINGS,
            WebviewUrl::App(page.map_or("index.html".into(), |p| format!("index.html?page={p}")).into()),
        )
        .title("Annex")
        .inner_size(1100.0, 760.0)
        .min_inner_size(860.0, 600.0)
        .resizable(true)
        .theme(Some(Theme::Dark))
        .background_color(Color(10, 10, 10, 255))
        .title_bar_style(TitleBarStyle::Overlay)
        .hidden_title(true)
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
            &MenuItem::with_id(app, QUIT_ITEM, "Quit Annex", true, Some("CmdOrCtrl+Q"))?,
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
    match e.id.as_ref() {
        SETTINGS_ITEM => show_settings(app, None),
        QUIT_ITEM => {
            if let Some(state) = app.try_state::<AppState>() {
                state.engine.shutdown();
            }
            app.exit(0);
        }
        _ => {}
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
        .on_window_event(|window, e| match (window.label(), e) {
            // Back to a menu bar app once the window is gone.
            (SETTINGS, WindowEvent::Destroyed) => {
                let _ = window.app_handle().set_activation_policy(ActivationPolicy::Accessory);
            }
            // Like a menu, the popover goes away when you click elsewhere.
            (popover::LABEL, WindowEvent::Focused(false)) => popover::hide(window.app_handle(), true),
            _ => {}
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
            open_screen_recording_settings,
            open_displays_settings,
            reveal_log,
            report_problem,
            open_annex,
            popover_size,
            popover_hide,
            quit
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
            app.manage(AppState { engine: engine.clone() });

            popover::create(app.handle())?;
            let tray = TrayIconBuilder::with_id(tray::ID)
                .icon(tray::initial_icon()?)
                .icon_as_template(true)
                .tooltip("Annex")
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left | MouseButton::Right, button_state: MouseButtonState::Up, .. } = e {
                        popover::toggle(tray.app_handle());
                    }
                })
                .build(app)?;
            std::thread::Builder::new().name("annex-tray".into()).spawn(move || tray::run(tray, engine))?;

            // Opened by hand: show the window. At login only when Annex needs the user.
            let needs_user = app.state::<AppState>().engine.status().paired_id.is_none() || !annex_mac::has_screen_recording();
            if !at_login || needs_user {
                show_settings(app.handle(), None);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the app")
        .run(|app, event| match event {
            // Closing the settings window keeps the app in the menu bar.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Opening Annex again (Finder, Spotlight, Launchpad) shows the window.
            tauri::RunEvent::Reopen { .. } => show_settings(app, None),
            tauri::RunEvent::Exit => {
                if let Some(s) = app.try_state::<AppState>() {
                    s.engine.shutdown();
                }
            }
            _ => {}
        });
}
