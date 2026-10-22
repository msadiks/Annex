//! Annex receiver: a Windows app (window + tray icon) that shows Mac displays
//! on this PC's monitors.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod ffmpeg;
#[cfg(windows)]
mod firewall;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod gui;
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("annex {}", annex_proto::APP_VERSION);
        return;
    }
    // Run elevated by the installer, the uninstaller, or the window's
    // "Allow" button.
    let exe = std::env::current_exe().unwrap_or_default();
    match args.first().map(String::as_str) {
        Some(a @ ("--allow-firewall" | "--allow-firewall=private")) => {
            let (tcp, udp) = firewall::ports(&args);
            std::process::exit(if firewall::allow(&exe, a == "--allow-firewall", tcp, udp) { 0 } else { 1 })
        }
        Some("--remove-firewall") => {
            firewall::remove(&exe);
            return;
        }
        _ => {}
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
