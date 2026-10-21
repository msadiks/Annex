//! Annex menu bar app: a tray icon and a settings window around the sender
//! engine. The webview is never on the video path.

#[cfg(target_os = "macos")]
mod app;

#[cfg(target_os = "macos")]
fn main() {
    app::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The Annex menu bar app runs on macOS only.");
}
