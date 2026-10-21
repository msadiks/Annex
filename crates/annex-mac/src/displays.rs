//! The Mac's own displays (for mirroring).

use std::ffi::c_void;

use annex_core::identity::VENDOR_ID;
use annex_proto::control::MacDisplay;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{CGDisplayBounds, CGDisplayIsBuiltin, CGDisplayIsMain, CGDisplayVendorNumber, CGGetActiveDisplayList};

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
        (id != 0 && active_ids().contains(&id)).then_some(id)
    }
}

/// Real displays that can be mirrored (our virtual displays excluded).
pub fn list() -> Vec<MacDisplay> {
    let mut out = Vec::new();
    let mut external = 0;
    for id in active_ids() {
        if CGDisplayVendorNumber(id) == VENDOR_ID {
            continue;
        }
        let Some(uuid) = uuid_of(id) else { continue };
        let b = CGDisplayBounds(id);
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
