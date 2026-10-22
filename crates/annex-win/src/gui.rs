//! The Annex window: a WebView2 page (ui/) in a normal top-level window. The
//! page posts JSON commands back (`UiCmd::Gui`) and gets the whole state
//! pushed to it as JSON whenever something changes.

use std::cell::RefCell;
use std::num::NonZeroIsize;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, EnumDisplayMonitors, GetMonitorInfoW, HBRUSH, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HRESULT, PCSTR, PCWSTR, PWSTR, s, w};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use crate::app::UiCmd;
use crate::misc;
use crate::shared::Shared;

/// Posted to the hidden window when the user closes the Annex window.
pub const WM_ANNEX_GUI_CLOSED: u32 = crate::shared::WM_ANNEX_WAKE + 3;

const WIDTH: i32 = 460;
const HEIGHT: i32 = 700;
const MIN_W: i32 = 380;
const MIN_H: i32 = 480;

thread_local! {
    static HIDDEN: RefCell<HWND> = const { RefCell::new(HWND(std::ptr::null_mut())) };
}

pub struct Gui {
    pub hwnd: HWND,
    /// Dropped before the window is destroyed.
    webview: Option<WebView>,
    _context: WebContext,
}

struct Handle(HWND);

impl HasWindowHandle for Handle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = NonZeroIsize::new(self.0.0 as isize).ok_or(HandleError::Unavailable)?;
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(h))) })
    }
}

fn page() -> String {
    include_str!("../ui/index.html")
        .replace(r#"<link rel="stylesheet" href="style.css">"#, &format!("<style>{}</style>", include_str!("../ui/style.css")))
        .replace(r#"<script src="app.js"></script>"#, &format!("<script>{}</script>", include_str!("../ui/app.js")))
}

/// Light theme unless Windows apps are set to dark.
pub fn dark_mode() -> bool {
    let mut v = 1u32;
    let mut size = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        );
    }
    v == 0
}

/// Page background, so the window never flashes white (keep in sync with
/// `--bg` in style.css).
fn background(dark: bool) -> (u8, u8, u8) {
    if dark { (0x20, 0x20, 0x20) } else { (0xf3, 0xf3, 0xf3) }
}

fn colorref((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Dark title bar in dark mode, and on Windows 11 a caption the same colour
/// as the page.
fn style_frame(hwnd: HWND) {
    let dark = dark_mode();
    let on = BOOL::from(dark);
    let caption = colorref(background(dark));
    let text = if dark { COLORREF(0x00FF_FFFF) } else { COLORREF(0) };
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption as *const _ as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &text as *const _ as *const _, 4);
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CLOSE => {
                let _ = ShowWindow(hwnd, SW_HIDE);
                HIDDEN.with(|h| {
                    let _ = PostMessageW(Some(*h.borrow()), WM_ANNEX_GUI_CLOSED, WPARAM(0), LPARAM(0));
                });
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let dpi = GetDpiForSystem() as i32;
                let info = &mut *(lp.0 as *mut MINMAXINFO);
                info.ptMinTrackSize.x = MIN_W * dpi / 96;
                info.ptMinTrackSize.y = MIN_H * dpi / 96;
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                style_frame(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn instance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default() }
}

/// Registers the window class. `hidden` gets `WM_ANNEX_GUI_CLOSED`.
pub fn register(hidden: HWND) -> Result<()> {
    HIDDEN.with(|h| *h.borrow_mut() = hidden);
    unsafe {
        let hi = instance();
        let icon = LoadIconW(Some(hi), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hi,
            lpszClassName: w!("AnnexMain"),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(CreateSolidBrush(colorref(background(dark_mode()))).0),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            anyhow::bail!("RegisterClassExW failed");
        }
    }
    Ok(())
}

fn monitors() -> Vec<MONITORINFO> {
    unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<MONITORINFO>) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            list.push(info);
        }
        BOOL::from(true)
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add), LPARAM(&mut list as *mut Vec<MONITORINFO> as isize));
    }
    list
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Keeps the window clear of Annex's full-screen displays (`covered`), which
/// are topmost: moves it to a monitor without one, the primary if it can, or
/// when every monitor shows a Mac display, keeps it above them.
fn arrange(hwnd: HWND, covered: &[RECT]) {
    let mut r = RECT::default();
    let mut topmost = false;
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
        if covered.iter().any(|c| overlaps(c, &r)) {
            let mut free: Vec<_> = monitors().into_iter().filter(|m| !covered.iter().any(|c| overlaps(c, &m.rcMonitor))).collect();
            free.sort_by_key(|m| m.dwFlags & MONITORINFOF_PRIMARY == 0);
            match free.first() {
                Some(m) => {
                    let work = m.rcWork;
                    let w = (r.right - r.left).min(work.right - work.left);
                    let h = (r.bottom - r.top).min(work.bottom - work.top);
                    let x = work.left + ((work.right - work.left) - w) / 2;
                    let y = work.top + ((work.bottom - work.top) - h) / 2;
                    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                None => topmost = true,
            }
        }
        let after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

impl Gui {
    /// Creates and shows the window, then loads the page into it. `covered`
    /// is where Annex's displays are (see [`arrange`]).
    pub fn open(shared: &Arc<Shared>, covered: &[RECT]) -> Result<Gui> {
        load_webview2()?;
        let dpi = unsafe { GetDpiForSystem() } as i32;
        let (w, h) = (WIDTH * dpi / 96, HEIGHT * dpi / 96);
        let mut work = RECT::default();
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        let x = work.left + ((work.right - work.left) - w).max(0) / 2;
        let y = work.top + ((work.bottom - work.top) - h).max(0) / 2;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("AnnexMain"),
                w!("Annex"),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h.min(work.bottom - work.top),
                None,
                None,
                Some(instance()),
                None,
            )?
        };
        style_frame(hwnd);
        // Up and in front before the web view starts, which takes a moment.
        arrange(hwnd, covered);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }

        let dark = dark_mode();
        let (r, g, b) = background(dark);
        let mut context = WebContext::new(Some(misc::local_dir().join("WebView2")));
        let ipc = shared.clone();
        let built = WebViewBuilder::new_with_web_context(&mut context)
            .with_html(page())
            .with_background_color((r, g, b, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_hotkeys_zoom(false)
            .with_browser_accelerator_keys(false)
            .with_default_context_menus(false)
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .with_new_window_req_handler(|url, _| {
                open_external(&url);
                NewWindowResponse::Deny
            })
            .with_ipc_handler(move |req| ipc.ui(UiCmd::Gui(req.into_body())))
            .build(&Handle(hwnd));
        let webview = match built {
            Ok(v) => v,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(e).context("creating the web view");
            }
        };
        Ok(Gui { hwnd, webview: Some(webview), _context: context })
    }

    /// Shows the window and brings it to the front.
    pub fn show(&self, covered: &[RECT]) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
            // After restoring: a minimized window has no useful position.
            self.arrange(covered);
            let _ = SetForegroundWindow(self.hwnd);
        }
        if let Some(w) = &self.webview {
            let _ = w.focus();
        }
    }

    pub fn arrange(&self, covered: &[RECT]) {
        arrange(self.hwnd, covered);
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    /// Hands the page a new state (a JSON object).
    pub fn push(&self, state: &str) {
        if let Some(w) = &self.webview {
            let _ = w.evaluate_script(&format!("window.annex && annex.update({state});"));
        }
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        self.webview = None;
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Opens web links from the page in the browser; nothing else.
pub fn open_external(url: &str) {
    if url.starts_with("https://") || url.starts_with("ms-settings:") {
        misc::open_url(url);
    }
}

// --- WebView2 loader ------------------------------------------------------
//
// The GNU toolchain can't link Microsoft's static WebView2 loader, and a
// WebView2Loader.dll import would stop Annex.exe from starting when the DLL
// isn't next to it. So the DLL is embedded, written to the local data folder
// on first use, and loaded by hand; these two functions stand in for its
// exports, which satisfies the linker without an import entry.

static LOADER: &[u8] = include_bytes!(env!("ANNEX_WEBVIEW2_LOADER"));

fn loader() -> Option<HMODULE> {
    static MODULE: OnceLock<Option<usize>> = OnceLock::new();
    MODULE
        .get_or_init(|| {
            let path = misc::local_dir().join("WebView2Loader.dll");
            if std::fs::read(&path).map(|b| b != LOADER).unwrap_or(true) {
                let _ = std::fs::create_dir_all(misc::local_dir());
                if let Err(e) = std::fs::write(&path, LOADER) {
                    tracing::error!("writing {}: {e}", path.display());
                }
            }
            let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            match unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) } {
                Ok(m) => Some(m.0 as usize),
                Err(e) => {
                    tracing::error!("loading WebView2Loader.dll: {e}");
                    None
                }
            }
        })
        .map(|m| HMODULE(m as *mut _))
}

type FarProc = unsafe extern "system" fn() -> isize;

fn loader_fn(name: PCSTR) -> Option<FarProc> {
    unsafe { GetProcAddress(loader()?, name) }
}

/// Fails early, with a clear error, when WebView2 can't run.
fn load_webview2() -> Result<()> {
    let mut version = PWSTR::null();
    let hr = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) };
    if hr.is_err() || version.is_null() {
        anyhow::bail!("the Microsoft Edge WebView2 Runtime is not installed ({hr:?})");
    }
    unsafe { CoTaskMemFree(Some(version.0 as *const _)) };
    Ok(())
}

const E_MOD_NOT_FOUND: HRESULT = HRESULT(0x8007_007Eu32 as i32);

#[unsafe(no_mangle)]
pub unsafe extern "system" fn CreateCoreWebView2EnvironmentWithOptions(
    browser_folder: PCWSTR,
    user_data_folder: PCWSTR,
    options: *mut std::ffi::c_void,
    handler: *mut std::ffi::c_void,
) -> HRESULT {
    type F = unsafe extern "system" fn(PCWSTR, PCWSTR, *mut std::ffi::c_void, *mut std::ffi::c_void) -> HRESULT;
    match loader_fn(s!("CreateCoreWebView2EnvironmentWithOptions")) {
        Some(f) => unsafe { std::mem::transmute::<FarProc, F>(f)(browser_folder, user_data_folder, options, handler) },
        None => E_MOD_NOT_FOUND,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetAvailableCoreWebView2BrowserVersionString(browser_folder: PCWSTR, version: *mut PWSTR) -> HRESULT {
    type F = unsafe extern "system" fn(PCWSTR, *mut PWSTR) -> HRESULT;
    match loader_fn(s!("GetAvailableCoreWebView2BrowserVersionString")) {
        Some(f) => unsafe { std::mem::transmute::<FarProc, F>(f)(browser_folder, version) },
        None => E_MOD_NOT_FOUND,
    }
}
