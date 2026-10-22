//! Small OS integrations: single instance, autostart, keeping the display
//! awake, data folders (installed or portable), logging.

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
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

/// Passed by the login item: start in the tray without opening the window.
pub const BACKGROUND_ARG: &str = "--background";

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
}

fn autostart_value() -> Option<String> {
    unsafe {
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)).ok().ok()?;
        let mut buf = vec![0u16; size as usize / 2 + 1];
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
        let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

pub fn autostart_enabled() -> bool {
    autostart_value().is_some()
}

/// 1.0 wrote the login item without `--background`, which now opens the
/// window at every login.
pub fn migrate_autostart() {
    if let Some(v) = autostart_value()
        && !v.contains(BACKGROUND_ARG)
        && v.trim_matches('"').eq_ignore_ascii_case(&exe_path())
    {
        set_autostart(true);
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let v = wide(&format!("\"{}\" {BACKGROUND_ARG}", exe_path()));
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

/// Portable mode: with `portable.txt` next to Annex.exe, settings, logs and
/// caches live in a `data` folder beside it instead of the user profile.
pub fn portable_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        dir.join("portable.txt").is_file().then(|| dir.join("data"))
    })
    .as_ref()
}

/// Settings: `%APPDATA%\Annex`.
pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ANNEX_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(p) = portable_dir() {
        return p.clone();
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Annex")
}

/// Logs and the window's web view cache: `%LOCALAPPDATA%\Annex`.
pub fn local_dir() -> PathBuf {
    if let Some(p) = portable_dir() {
        return p.clone();
    }
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Annex")
}

pub fn log_dir() -> PathBuf {
    local_dir().join("logs")
}

pub fn open_folder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    open_url(&path.display().to_string());
}

/// Opens a folder, a web page or an `ms-settings:` page.
pub fn open_url(target: &str) {
    let p = wide(target);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
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
