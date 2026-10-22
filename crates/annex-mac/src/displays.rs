//! The Mac's own displays (for mirroring).

use std::ffi::c_void;
use std::time::{Duration, Instant};

use annex_core::identity::{VENDOR_ID, is_mirror_product};
use annex_proto::control::MacDisplay;
use anyhow::bail;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayMirrorOfDisplay, CGConfigureDisplayOrigin, CGConfigureOption, CGDisplayBounds, CGDisplayConfigRef,
    CGDisplayIsActive, CGDisplayIsBuiltin, CGDisplayIsMain, CGDisplayMirrorsDisplay, CGDisplayModelNumber,
    CGDisplayVendorNumber, CGError, CGGetActiveDisplayList, CGGetOnlineDisplayList, CGMainDisplayID,
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

/// The Annex mirror display that Mac display `id` mirrors, while it is
/// matched to a PC monitor's shape. Annex's extended displays never count:
/// their picture is not the Mac display's.
pub fn annex_master(id: u32) -> Option<u32> {
    let m = CGDisplayMirrorsDisplay(id);
    (m != 0 && CGDisplayVendorNumber(m) == VENDOR_ID && is_mirror_product(CGDisplayModelNumber(m))).then_some(m)
}

/// The display that shows Mac display `id`'s picture: itself, or the Annex
/// virtual display it mirrors.
pub fn content_of(id: u32) -> u32 {
    annex_master(id).unwrap_or(id)
}

/// A Mac display made to mirror one of our virtual displays (mirror matched
/// to a PC monitor's shape), and where it was before. macOS undoes the
/// mirroring when Annex quits.
#[derive(Clone, Copy, Debug)]
pub struct Mirrored {
    pub display: u32,
    pub master: u32,
    origin: (i32, i32),
    width: i32,
}

impl Mirrored {
    /// Makes Mac display `display` mirror `master`. The mirror takes the Mac
    /// display's place, so it stays the main display if the Mac display was,
    /// and the other displays stay where they are.
    pub fn start(display: u32, master: u32) -> anyhow::Result<Self> {
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && !active_ids().contains(&master) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let b = CGDisplayBounds(display);
        let m = Mirrored {
            display,
            master,
            origin: (b.origin.x as i32, b.origin.y as i32),
            width: b.size.width as i32,
        };
        m.apply()?;
        m.settle()?;
        Ok(m)
    }

    /// Whether the displays are still arranged the way [`Mirrored::apply`]
    /// left them, or one of them is gone (then there is nothing to restore).
    pub fn holds(&self) -> bool {
        let online = online_ids();
        if !online.contains(&self.display) || !online.contains(&self.master) {
            return true;
        }
        self.mirrors()
    }

    fn mirrors(&self) -> bool {
        CGDisplayMirrorsDisplay(self.display) == self.master && CGDisplayMirrorsDisplay(self.master) == 0
    }

    /// Arranges the displays for the mirroring (again). macOS brings back the
    /// arrangement it remembers for a set of displays whenever one comes or
    /// goes, which can turn the mirroring around (the Mac display becomes
    /// the main one and the virtual display mirrors it) or undo it.
    pub fn apply(&self) -> anyhow::Result<()> {
        retry(|| {
            configure(|cfg| unsafe {
                check(CGConfigureDisplayMirrorOfDisplay(cfg, self.master, 0))?;
                check(CGConfigureDisplayMirrorOfDisplay(cfg, self.display, self.master))?;
                check(CGConfigureDisplayOrigin(cfg, self.master, self.origin.0, self.origin.1))
            })?;
            if !self.mirrors() {
                bail!("macOS did not keep the display mirroring");
            }
            Ok(())
        })?;
        tracing::info!(display = self.display, master = self.master, main = CGMainDisplayID(), "display mirroring changed");
        Ok(())
    }

    /// Waits until the mirroring has held for a moment, restoring it while
    /// macOS is still rearranging the displays. A capture of the virtual
    /// display started before then can keep showing it as an empty extended
    /// desktop instead of the Mac display's picture.
    pub fn settle(&self) -> anyhow::Result<()> {
        let until = Instant::now() + Duration::from_secs(4);
        let mut held_since = Instant::now();
        while Instant::now() < until {
            if !self.holds() {
                tracing::info!(display = self.display, master = self.master, "macOS changed the display mirroring; restoring it");
                self.apply()?;
                held_since = Instant::now();
            } else if held_since.elapsed() >= Duration::from_millis(800) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Stops the mirroring and puts the Mac display back where it was, the
    /// virtual display next to it until it goes away.
    pub fn stop(&self) -> anyhow::Result<()> {
        let (x, y) = self.origin;
        retry(|| {
            configure(|cfg| unsafe {
                check(CGConfigureDisplayMirrorOfDisplay(cfg, self.display, 0))?;
                check(CGConfigureDisplayOrigin(cfg, self.display, x, y))?;
                check(CGConfigureDisplayOrigin(cfg, self.master, x + self.width, y))
            })
        })?;
        tracing::info!(display = self.display, main = CGMainDisplayID(), "display mirroring stopped");
        Ok(())
    }
}

/// Runs `f` until it succeeds, for up to two seconds: macOS refuses display
/// changes (error 1001) while it is still applying another one, such as a
/// virtual display that has just come or gone.
fn retry(mut f: impl FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
    let mut tries = 0;
    loop {
        match f() {
            Err(e) if tries < 20 => {
                tries += 1;
                tracing::debug!("display change not applied yet, retrying: {e:#}");
                std::thread::sleep(Duration::from_millis(100));
            }
            r => return r,
        }
    }
}

/// Applies the display changes `f` makes, as one change, until Annex quits.
fn configure(f: impl FnOnce(CGDisplayConfigRef) -> anyhow::Result<()>) -> anyhow::Result<()> {
    unsafe {
        let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
        let e = CGBeginDisplayConfiguration(&mut cfg);
        if e != CGError::Success {
            bail!("macOS refused to change the display setup (error {})", e.0);
        }
        if let Err(e) = f(cfg) {
            CGCancelDisplayConfiguration(cfg);
            return Err(e);
        }
        let e = CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForAppOnly);
        if e != CGError::Success {
            bail!("macOS could not apply the display change (error {})", e.0);
        }
    }
    Ok(())
}

fn check(e: CGError) -> anyhow::Result<()> {
    if e != CGError::Success {
        bail!("macOS refused the display change (error {})", e.0);
    }
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
