//! The menu bar menu: connection status, and each PC monitor with its
//! settings in a submenu. One thread owns the menu and keeps it current.

use std::time::Duration;

use annex_proto::control::{ModePref, SourcePref};
use annex_sender::{ConnState, EngineHandle, Status, StreamStatus};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Wry};

pub const OPEN: &str = "open";
pub const QUIT: &str = "quit";

const MODES: [(&str, &str); 3] = [
    ("auto", "Automatic"),
    ("sharp", "Sharp: exact colour, for text"),
    ("fast", "Fast: smooth, for video and motion"),
];

/// A click on a monitor setting in the menu.
pub enum Action {
    ToggleEnabled(u64),
    Mode(u64, ModePref),
    Source(u64, SourcePref),
    MirrorMatch(u64, bool),
}

/// Parses the id of a monitor item: `m:<monitor>:<what>[:<value>]`.
pub fn action(id: &str) -> Option<Action> {
    let mut parts = id.splitn(4, ':');
    if parts.next()? != "m" {
        return None;
    }
    let monitor: u64 = parts.next()?.parse().ok()?;
    match (parts.next()?, parts.next()) {
        ("on", None) => Some(Action::ToggleEnabled(monitor)),
        ("mode", Some(m)) => Some(Action::Mode(monitor, mode_pref(m))),
        ("src", Some("extend")) => Some(Action::Source(monitor, SourcePref::Extend)),
        ("src", Some(uuid)) => Some(Action::Source(monitor, SourcePref::Mirror { display_uuid: uuid.to_string() })),
        ("fit", Some(v)) => Some(Action::MirrorMatch(monitor, v == "match")),
        _ => None,
    }
}

pub fn mode_pref(m: &str) -> ModePref {
    match m {
        "sharp" => ModePref::Sharp,
        "fast" => ModePref::Fast,
        _ => ModePref::Auto,
    }
}

fn mode_key(m: ModePref) -> &'static str {
    match m {
        ModePref::Auto => "auto",
        ModePref::Sharp => "sharp",
        ModePref::Fast => "fast",
    }
}

fn source_key(s: &SourcePref) -> &str {
    match s {
        SourcePref::Extend => "extend",
        SourcePref::Mirror { display_uuid } => display_uuid,
    }
}

pub fn status_line(s: &Status) -> String {
    let pc = || s.paired_name.clone().unwrap_or_else(|| "the PC".into());
    let live = s.streams.iter().filter(|t| t.live).count();
    match s.state {
        ConnState::NotPaired => "Not paired with a PC".into(),
        ConnState::Searching => format!("Looking for {}…", pc()),
        ConnState::Connecting => format!("Connecting to {}…", pc()),
        ConnState::Connected if live > 0 => {
            format!("Streaming {live} display{} to {}", if live == 1 { "" } else { "s" }, pc())
        }
        ConnState::Connected => format!("Connected to {}", pc()),
    }
}

fn stats_line(t: &StreamStatus) -> String {
    if !t.live {
        return "Starting…".into();
    }
    let mut line = format!("{} · {:.0} fps · {:.0} Mbps", t.mode.as_str(), t.fps_presented, t.bitrate_kbps as f32 / 1000.0);
    if let Some(ms) = t.e2e_ms {
        line.push_str(&format!(" · ≈ {ms:.0} ms"));
    }
    line
}

/// Items of one monitor's submenu that change in place.
struct MonitorItems {
    id: u64,
    name: String,
    submenu: Submenu<Wry>,
    enabled: CheckMenuItem<Wry>,
    modes: Vec<(&'static str, CheckMenuItem<Wry>)>,
    sources: Vec<(String, CheckMenuItem<Wry>)>,
    fit: CheckMenuItem<Wry>,
    fill: CheckMenuItem<Wry>,
    info: MenuItem<Wry>,
    stats: MenuItem<Wry>,
}

struct Built {
    /// Changes in this rebuild the menu; everything else updates in place.
    shape: String,
    status: MenuItem<Wry>,
    monitors: Vec<MonitorItems>,
}

fn shape_of(s: &Status) -> String {
    let mut k = format!("{:?}|", s.state);
    for m in &s.monitors {
        k.push_str(&format!("{}:{};", m.id, m.name));
    }
    k.push('|');
    for d in &s.mac_displays {
        k.push_str(&format!("{}:{};", d.uuid, d.name));
    }
    k
}

fn build(app: &AppHandle, tray: &TrayIcon, s: &Status) -> tauri::Result<Built> {
    let status = MenuItem::with_id(app, "status", status_line(s), false, None::<&str>)?;
    let mut monitors = Vec::new();
    for m in &s.monitors {
        let p = |what: &str| format!("m:{}:{what}", m.id);
        let enabled = CheckMenuItem::with_id(app, p("on"), "Use as a Mac display", true, m.enabled, None::<&str>)?;
        let modes = MODES
            .iter()
            .map(|(k, label)| CheckMenuItem::with_id(app, p(&format!("mode:{k}")), *label, true, false, None::<&str>).map(|i| (*k, i)))
            .collect::<tauri::Result<Vec<_>>>()?;
        let mut sources = vec![("extend".to_string(), CheckMenuItem::with_id(app, p("src:extend"), "New display (extend)", true, false, None::<&str>)?)];
        for d in &s.mac_displays {
            let item = CheckMenuItem::with_id(app, p(&format!("src:{}", d.uuid)), format!("Mirror {}", d.name), true, false, None::<&str>)?;
            sources.push((d.uuid.clone(), item));
        }
        let fit = CheckMenuItem::with_id(app, p("fit:fit"), "Fit, with black bars on the PC", true, false, None::<&str>)?;
        let fill = CheckMenuItem::with_id(app, p("fit:match"), "Match the PC monitor (best as your main screen)", true, false, None::<&str>)?;
        let info = MenuItem::new(app, "", false, None::<&str>)?;
        let stats = MenuItem::new(app, "", false, None::<&str>)?;

        let sep = || PredefinedMenuItem::separator(app);
        let header = |t: &str| MenuItem::new(app, t, false, None::<&str>);
        let (sep1, sep2, sep3, sep4) = (sep()?, sep()?, sep()?, sep()?);
        let (h_picture, h_shows, h_mirror) = (header("Picture")?, header("Shows")?, header("Mirror shape")?);
        let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&enabled, &sep1, &h_picture];
        items.extend(modes.iter().map(|(_, i)| i as &dyn IsMenuItem<Wry>));
        items.extend([&sep2 as &dyn IsMenuItem<Wry>, &h_shows]);
        items.extend(sources.iter().map(|(_, i)| i as &dyn IsMenuItem<Wry>));
        items.extend([&sep3 as &dyn IsMenuItem<Wry>, &h_mirror, &fit, &fill, &sep4, &info, &stats]);
        let submenu = Submenu::with_items(app, &m.name, true, &items)?;
        monitors.push(MonitorItems { id: m.id, name: m.name.clone(), submenu, enabled, modes, sources, fit, fill, info, stats });
    }

    let menu = Menu::new(app)?;
    menu.append(&status)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if !monitors.is_empty() {
        menu.append(&MenuItem::new(app, "PC monitors", false, None::<&str>)?)?;
        for m in &monitors {
            menu.append(&m.submenu)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    } else if s.state == ConnState::Connected {
        menu.append(&MenuItem::new(app, "The PC reported no monitors", false, None::<&str>)?)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    let open_label = if s.state == ConnState::NotPaired { "Pair with a PC…" } else { "Open Annex…" };
    menu.append(&MenuItem::with_id(app, OPEN, open_label, true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, QUIT, "Quit Annex", true, None::<&str>)?)?;
    tray.set_menu(Some(menu))?;
    Ok(Built { shape: shape_of(s), status, monitors })
}

fn update(b: &Built, s: &Status) -> tauri::Result<()> {
    b.status.set_text(status_line(s))?;
    for items in &b.monitors {
        let Some(m) = s.monitors.iter().find(|m| m.id == items.id) else { continue };
        let stream = s.streams.iter().find(|t| t.monitor_id == m.id);
        let state = match stream {
            Some(t) if t.live => format!("{:.0} fps", t.fps_presented),
            Some(_) => "starting…".into(),
            None if m.enabled => "on".into(),
            None => "off".into(),
        };
        items.submenu.set_text(format!("{}  ·  {state}", items.name))?;
        items.enabled.set_checked(m.enabled)?;
        let mode = mode_key(m.pinned_mode);
        for (k, i) in &items.modes {
            i.set_checked(*k == mode)?;
        }
        let source = source_key(&m.source);
        for (k, i) in &items.sources {
            i.set_checked(k == source)?;
        }
        let mirror = matches!(m.source, SourcePref::Mirror { .. });
        let matched = s.mirror_match.contains(&m.id);
        items.fit.set_enabled(mirror)?;
        items.fill.set_enabled(mirror)?;
        items.fit.set_checked(mirror && !matched)?;
        items.fill.set_checked(mirror && matched)?;
        let main = if m.primary { " · main" } else { "" };
        items.info.set_text(format!("{}×{} · {:.0} Hz{main}", m.native_w, m.native_h, m.refresh_mhz as f32 / 1000.0))?;
        items.stats.set_text(match stream {
            Some(t) => stats_line(t),
            None => "Not streaming".into(),
        })?;
    }
    Ok(())
}

/// Keeps the menu current: once a second, and shortly after `poke` (a click
/// on one of its settings, which the PC confirms within a moment).
pub fn run(app: AppHandle, tray: TrayIcon, engine: EngineHandle, poke: Receiver<()>) {
    let mut built: Option<Built> = None;
    let mut tooltip = String::new();
    loop {
        let s = engine.status();
        let fresh = match &built {
            Some(b) if b.shape == shape_of(&s) => None,
            _ => Some(build(&app, &tray, &s)),
        };
        match fresh {
            Some(Ok(b)) => built = Some(b),
            Some(Err(e)) => tracing::warn!("building the menu bar menu: {e}"),
            None => {}
        }
        if let Some(b) = &built
            && let Err(e) = update(b, &s)
        {
            tracing::warn!("updating the menu bar menu: {e}");
        }
        let tip = format!("Annex: {}", status_line(&s));
        if tip != tooltip {
            let _ = tray.set_tooltip(Some(&tip));
            tooltip = tip;
        }
        match poke.recv_timeout(Duration::from_secs(1)) {
            Ok(()) => std::thread::sleep(Duration::from_millis(250)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
