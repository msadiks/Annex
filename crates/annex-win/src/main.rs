//! Annex receiver: a Windows tray app that shows Mac displays on this PC's
//! monitors.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod ffmpeg;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod misc;
#[cfg(windows)]
mod monitors;
#[cfg(windows)]
mod net;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod pipeline;
#[cfg(windows)]
mod shared;
#[cfg(windows)]
mod wnd;

#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("annex {}", annex_proto::APP_VERSION);
        return;
    }
    if let Err(e) = app::run() {
        tracing::error!("fatal: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The Annex receiver runs on Windows.");
}
