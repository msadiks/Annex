//! The popover under the menu bar icon: a borderless web view that drops down
//! on a click and hides when it loses focus, like a menu.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::tray;

pub const LABEL: &str = "popover";
const WIDTH: f64 = 340.0;
/// Space between the menu bar and the popover.
const GAP: f64 = 6.0;
/// Space kept to the screen edges.
const MARGIN: f64 = 8.0;

#[derive(Default)]
pub struct Popover {
    /// Height of the popover's content, as the page last reported it.
    height: Mutex<f64>,
    /// When a lost focus hid the popover. The click on the icon that took the
    /// focus away should not open it again.
    hidden_at: Mutex<Option<Instant>>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    app.manage(Popover { height: Mutex::new(320.0), hidden_at: Mutex::new(None) });
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("popover.html".into()))
        .title("Annex")
        .inner_size(WIDTH, 320.0)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .visible(false)
        .focused(false)
        .build()?;
    Ok(())
}

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// A click on the menu bar icon.
pub fn toggle(app: &AppHandle) {
    let Some(w) = window(app) else { return };
    if w.is_visible().unwrap_or(false) {
        hide(app, false);
        return;
    }
    let state = app.state::<Popover>();
    if state.hidden_at.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
        return;
    }
    place(app);
    let Some(mtm) = MainThreadMarker::new() else { return };
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.unhide(None);
    #[allow(deprecated)] // `activate` alone is too polite to take focus from the active app.
    ns_app.activateIgnoringOtherApps(true);
    let _ = w.show();
    let _ = w.set_focus();
    if let Some(ns) = ns_window(&w) {
        ns.makeKeyAndOrderFront(None);
    }
    highlight(app, true);
    let _ = w.eval("window.annexShown && annexShown()");
}

/// Hides the popover. `blurred`: it lost the focus to something else.
pub fn hide(app: &AppHandle, blurred: bool) {
    let Some(w) = window(app) else { return };
    if !w.is_visible().unwrap_or(false) {
        return;
    }
    let _ = w.hide();
    let _ = w.eval("window.annexHidden && annexHidden()");
    highlight(app, false);
    if blurred {
        *app.state::<Popover>().hidden_at.lock().unwrap() = Some(Instant::now());
        return;
    }
    // Hand the focus back to the app that had it, unless the window is open.
    let settings_open = app.get_webview_window(crate::app::SETTINGS).is_some_and(|s| s.is_visible().unwrap_or(false));
    if !settings_open && let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).hide(None);
    }
}

/// The page reports its height whenever it changes.
pub fn set_height(app: &AppHandle, height: f64) {
    *app.state::<Popover>().height.lock().unwrap() = height.clamp(80.0, 2000.0);
    if window(app).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        place(app);
    }
}

fn ns_window(w: &WebviewWindow) -> Option<&NSWindow> {
    let ptr = w.ns_window().ok()?;
    Some(unsafe { &*(ptr as *const NSWindow) })
}

/// The frame of the menu bar icon and the visible frame of its screen, in
/// AppKit coordinates (origin at the bottom left, in points).
fn icon_frame(app: &AppHandle) -> Option<(NSRect, NSRect)> {
    let tray = app.tray_by_id(tray::ID)?;
    tray.with_inner_tray_icon(|t| {
        let mtm = MainThreadMarker::new()?;
        let window = t.ns_status_item()?.button(mtm)?.window()?;
        let screen = window.screen()?;
        Some((window.frame(), screen.visibleFrame()))
    })
    .ok()
    .flatten()
}

fn highlight(app: &AppHandle, on: bool) {
    if let Some(tray) = app.tray_by_id(tray::ID) {
        let _ = tray.with_inner_tray_icon(move |t| {
            if let (Some(mtm), Some(item)) = (MainThreadMarker::new(), t.ns_status_item())
                && let Some(button) = item.button(mtm)
            {
                button.highlight(on);
            }
        });
    }
}

/// Puts the popover under the icon, centred on it and kept on the screen.
fn place(app: &AppHandle) {
    let (Some(w), Some((icon, screen))) = (window(app), icon_frame(app)) else { return };
    let Some(ns) = ns_window(&w) else { return };
    let top = icon.origin.y - GAP;
    let room = top - screen.origin.y - MARGIN;
    let height = app.state::<Popover>().height.lock().unwrap().min(room);
    let centre = icon.origin.x + icon.size.width / 2.0;
    let left_most = screen.origin.x + MARGIN;
    let right_most = screen.origin.x + screen.size.width - WIDTH - MARGIN;
    let x = (centre - WIDTH / 2.0).min(right_most).max(left_most);
    ns.setFrame_display(NSRect::new(NSPoint::new(x, top - height), NSSize::new(WIDTH, height)), true);
    ns.invalidateShadow();
}
