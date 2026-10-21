//! Mac sender: virtual displays (private CoreGraphics API), ScreenCaptureKit
//! capture, Metal packing and VideoToolbox HEVC encoding, behind the
//! platform-independent [`annex_sender::Backend`] interface.

#[cfg(target_os = "macos")]
mod backend;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod cursor;
#[cfg(target_os = "macos")]
pub mod displays;
#[cfg(target_os = "macos")]
mod encode;
#[cfg(target_os = "macos")]
mod pack;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
pub mod vdisplay;

#[cfg(target_os = "macos")]
pub use backend::MacBackend;
#[cfg(target_os = "macos")]
pub use sys::{has_screen_recording, machine_name, request_screen_recording};

/// `~/Library/Application Support/Annex/config.toml`.
pub fn config_path() -> std::path::PathBuf {
    std::env::var_os("ANNEX_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("Annex"))
        .join("config.toml")
}

/// `~/Library/Logs/Annex`.
pub fn log_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir).join("Library/Logs/Annex")
}

/// The computer name shown on the PC.
pub fn host_name() -> String {
    #[cfg(target_os = "macos")]
    {
        sys::computer_name().unwrap_or_else(|| "Mac".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Mac".into()
    }
}
