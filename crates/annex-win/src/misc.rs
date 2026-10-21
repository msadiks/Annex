//! Small OS integrations: single instance, autostart, firewall rule, keeping
//! the display awake, logging.

use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns `None` if another instance is already running.
pub fn single_instance() -> Option<HANDLE> {
    unsafe {
        let h = CreateMutexW(None, true, w!("Local\\AnnexReceiverSingleInstance")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(h)
    }
}

/// Spec 3.4: keep the panel awake while streaming.
pub fn keep_display_awake(on: bool) {
    unsafe {
        let flags = if on { ES_CONTINUOUS | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS };
        SetThreadExecutionState(flags);
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Annex");

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
}

pub fn autostart_enabled() -> bool {
    unsafe {
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok()
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let v = wide(&format!("\"{}\"", exe_path()));
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(v.as_ptr() as *const _),
                (v.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

/// Adds an inbound allow rule for this executable on private networks
/// (prompts for elevation).
pub fn add_firewall_rule() {
    let args = format!(
        "advfirewall firewall add rule name=\"Annex\" dir=in action=allow program=\"{}\" enable=yes profile=private,domain",
        exe_path()
    );
    let a = wide(&args);
    unsafe {
        ShellExecuteW(None, w!("runas"), w!("netsh"), PCWSTR(a.as_ptr()), None, SW_HIDE);
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ANNEX_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Annex")
}

pub fn log_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Annex").join("logs")
}

pub fn open_folder(path: &std::path::Path) {
    let p = wide(&path.display().to_string());
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
    }
}

pub fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("annex.log");
    if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("annex.old.log"));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    let filter = EnvFilter::try_from_env("ANNEX_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    match file {
        Ok(f) => {
            let f = std::sync::Mutex::new(f);
            fmt().with_env_filter(filter).with_ansi(false).with_writer(f).init();
        }
        Err(_) => fmt().with_env_filter(filter).init(),
    }
}
