//! The Mac's own displays (for mirroring).

use std::ffi::c_void;

use annex_core::identity::VENDOR_ID;
use annex_proto::control::MacDisplay;
use anyhow::bail;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayMirrorOfDisplay, CGConfigureOption, CGDisplayBounds, CGDisplayConfigRef, CGDisplayIsActive,
    CGDisplayIsBuiltin, CGDisplayIsMain, CGDisplayMirrorsDisplay, CGDisplayVendorNumber, CGError, CGGetActiveDisplayList,
    CGGetOnlineDisplayList,
};

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const c_void;
    fn CGDisplayGetDisplayIDFromUUID(uuid: *const c_void) -> u32;
}

unsafe extern "C" {
    fn CFUUIDCreateString(alloc: *const c_void, uuid: *const c_void) -> *const CFString;
    fn CFUUIDCreateFromString(alloc: *const c_void, s: *const CFString) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

pub fn active_ids() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    unsafe {
        CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n);
    }
    ids[..n as usize].to_vec()
}

/// Like [`active_ids`], but also displays that only mirror another one.
fn online_ids() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    unsafe {
        CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n);
    }
    ids[..n as usize].to_vec()
}

/// The Annex virtual display that Mac display `id` mirrors, while it is
/// matched to a PC monitor's shape.
pub fn annex_master(id: u32) -> Option<u32> {
    let m = CGDisplayMirrorsDisplay(id);
    (m != 0 && CGDisplayVendorNumber(m) == VENDOR_ID).then_some(m)
}

/// The display that shows Mac display `id`'s picture: itself, or the Annex
/// virtual display it mirrors.
pub fn content_of(id: u32) -> u32 {
    annex_master(id).unwrap_or(id)
}

/// Makes Mac display `id` mirror `master`, or stops it mirroring with `None`.
/// macOS undoes the change when Annex quits.
pub fn set_mirror(id: u32, master: Option<u32>) -> anyhow::Result<()> {
    unsafe {
        let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
        let e = CGBeginDisplayConfiguration(&mut cfg);
        if e != CGError::Success {
            bail!("macOS refused to change the display setup (error {})", e.0);
        }
        let e = CGConfigureDisplayMirrorOfDisplay(cfg, id, master.unwrap_or(0));
        if e != CGError::Success {
            CGCancelDisplayConfiguration(cfg);
            bail!("macOS refused to mirror the display (error {})", e.0);
        }
        let e = CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForAppOnly);
        if e != CGError::Success {
            bail!("macOS could not apply the display mirroring (error {})", e.0);
        }
    }
    tracing::info!(display = id, master = master.unwrap_or(0), "display mirroring changed");
    Ok(())
}

pub fn uuid_of(id: u32) -> Option<String> {
    unsafe {
        let u = CGDisplayCreateUUIDFromDisplayID(id);
        if u.is_null() {
            return None;
        }
        let s = CFUUIDCreateString(std::ptr::null(), u);
        CFRelease(u);
        if s.is_null() {
            return None;
        }
        let s = CFRetained::from_raw(std::ptr::NonNull::new_unchecked(s as *mut CFString));
        Some(s.to_string())
    }
}

pub fn id_of(uuid: &str) -> Option<u32> {
    let s = CFString::from_str(uuid);
    unsafe {
        let u = CFUUIDCreateFromString(std::ptr::null(), &*s);
        if u.is_null() {
            return None;
        }
        let id = CGDisplayGetDisplayIDFromUUID(u);
        CFRelease(u);
        (id != 0 && online_ids().contains(&id)).then_some(id)
    }
}

/// Real displays that can be mirrored (our virtual displays excluded).
pub fn list() -> Vec<MacDisplay> {
    let mut out = Vec::new();
    let mut external = 0;
    for id in online_ids() {
        if CGDisplayVendorNumber(id) == VENDOR_ID {
            continue;
        }
        // A display matched to a PC monitor mirrors one of ours and so is no
        // longer active, but it is still the one being mirrored.
        if !CGDisplayIsActive(id) && annex_master(id).is_none() {
            continue;
        }
        let Some(uuid) = uuid_of(id) else { continue };
        let b = CGDisplayBounds(content_of(id));
        let builtin = CGDisplayIsBuiltin(id);
        let name = if builtin {
            "Built-in display".to_string()
        } else {
            external += 1;
            if CGDisplayIsMain(id) { format!("Main display {external}") } else { format!("Display {external}") }
        };
        out.push(MacDisplay { uuid, name, width: b.size.width as u32, height: b.size.height as u32, builtin });
    }
    out
}
