//! The menu bar icon: its look follows the connection, and a click opens the
//! popover. One thread keeps the icon and its tooltip current.

use std::time::Duration;

use annex_proto::control::ModePref;
use annex_sender::{ConnState, EngineHandle, Status};
use tauri::image::Image;
use tauri::tray::TrayIcon;

pub const ID: &str = "annex";

pub fn mode_pref(m: &str) -> ModePref {
    match m {
        "sharp" => ModePref::Sharp,
        "fast" => ModePref::Fast,
        _ => ModePref::Auto,
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

/// The icon for the state: dimmed when not paired, a dashed PC monitor while
/// looking for the PC, outlined when connected and filled while streaming.
fn icon_for(s: &Status) -> (&'static str, &'static [u8]) {
    match s.state {
        ConnState::NotPaired => ("off", include_bytes!("../icons/tray-off.png")),
        ConnState::Searching | ConnState::Connecting => ("search", include_bytes!("../icons/tray-search.png")),
        ConnState::Connected if s.streams.iter().any(|t| t.live) => ("live", include_bytes!("../icons/tray.png")),
        ConnState::Connected => ("idle", include_bytes!("../icons/tray-idle.png")),
    }
}

pub fn initial_icon() -> tauri::Result<Image<'static>> {
    Image::from_bytes(include_bytes!("../icons/tray-off.png"))
}

/// Keeps the icon and tooltip current, once a second.
pub fn run(tray: TrayIcon, engine: EngineHandle) {
    let mut shown = ("off", String::new());
    loop {
        let s = engine.status();
        let (look, png) = icon_for(&s);
        let tip = format!("Annex: {}", status_line(&s));
        if look != shown.0 {
            match Image::from_bytes(png) {
                Ok(i) => {
                    let _ = tray.set_icon(Some(i));
                    let _ = tray.set_icon_as_template(true);
                }
                Err(e) => tracing::warn!("menu bar icon: {e}"),
            }
        }
        if tip != shown.1 {
            let _ = tray.set_tooltip(Some(&tip));
        }
        shown = (look, tip);
        std::thread::sleep(Duration::from_secs(1));
    }
}
