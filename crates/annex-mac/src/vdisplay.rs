//! Virtual displays through the private `CGVirtualDisplay` API (spec 6.1).
//!
//! The API is undocumented, so [`probe`] checks every class and selector we
//! use before anything is created, and fails with a clear message instead of
//! an unrecognized-selector crash after a macOS update.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSArray, NSString};

const CLASSES: [(&std::ffi::CStr, &[&str]); 4] = [
    (
        c"CGVirtualDisplayDescriptor",
        &[
            "setName:",
            "setMaxPixelsWide:",
            "setMaxPixelsHigh:",
            "setSizeInMillimeters:",
            "setVendorID:",
            "setProductID:",
            "setSerialNum:",
            "setQueue:",
        ],
    ),
    (c"CGVirtualDisplayMode", &["initWithWidth:height:refreshRate:"]),
    (c"CGVirtualDisplaySettings", &["setModes:", "setHiDPI:"]),
    (c"CGVirtualDisplay", &["initWithDescriptor:", "applySettings:", "displayID"]),
];

fn class(name: &std::ffi::CStr) -> anyhow::Result<&'static AnyClass> {
    AnyClass::get(name).with_context(|| format!("{} is missing (macOS changed its virtual display API)", name.to_string_lossy()))
}

/// Checks that the private API looks like we expect.
pub fn probe() -> anyhow::Result<()> {
    for (name, selectors) in CLASSES {
        let cls = class(name)?;
        for s in selectors {
            let sel = Sel::register(&std::ffi::CString::new(*s).unwrap());
            if cls.instance_method(sel).is_none() {
                bail!("{}.{} is missing (macOS changed its virtual display API)", name.to_string_lossy(), s);
            }
        }
    }
    Ok(())
}

/// One virtual display. Dropping it unplugs the display.
pub struct VirtualDisplay {
    obj: Retained<AnyObject>,
    _queue: DispatchRetained<DispatchQueue>,
    pub id: u32,
}

// The object is only touched on creation and drop.
unsafe impl Send for VirtualDisplay {}

impl VirtualDisplay {
    /// Creates a `width × height` 60 Hz display with one mode and hiDPI off
    /// (spec 3.1). The identity decides what macOS remembers about it.
    pub fn create(name: &str, width: u32, height: u32, identity: (u32, u32, u32)) -> anyhow::Result<Self> {
        probe()?;
        let (vendor, product, serial) = identity;
        let queue = DispatchQueue::new("annex.vdisplay", None);
        unsafe {
            let desc: Retained<AnyObject> = msg_send![class(c"CGVirtualDisplayDescriptor")?, new];
            let _: () = msg_send![&desc, setName: &*NSString::from_str(name)];
            let _: () = msg_send![&desc, setMaxPixelsWide: width];
            let _: () = msg_send![&desc, setMaxPixelsHigh: height];
            // ~100 ppi, so macOS picks a normal (non-Retina) scale.
            let mm = CGSize { width: width as f64 * 25.4 / 100.0, height: height as f64 * 25.4 / 100.0 };
            let _: () = msg_send![&desc, setSizeInMillimeters: mm];
            let _: () = msg_send![&desc, setVendorID: vendor];
            let _: () = msg_send![&desc, setProductID: product];
            let _: () = msg_send![&desc, setSerialNum: serial];
            let q: *const AnyObject = (&*queue as *const DispatchQueue).cast();
            let _: () = msg_send![&desc, setQueue: q];

            let alloc: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplay")?, alloc];
            let obj: Option<Retained<AnyObject>> = msg_send![alloc, initWithDescriptor: &*desc];
            let obj = obj.context("macOS refused to create the virtual display")?;

            let mode_alloc: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplayMode")?, alloc];
            let mode: Retained<AnyObject> = msg_send![mode_alloc, initWithWidth: width, height: height, refreshRate: 60.0f64];
            let settings: Retained<AnyObject> = msg_send![class(c"CGVirtualDisplaySettings")?, new];
            let _: () = msg_send![&settings, setHiDPI: 0u32];
            let modes = NSArray::from_retained_slice(&[mode]);
            let _: () = msg_send![&settings, setModes: &*modes];
            let ok: bool = msg_send![&obj, applySettings: &*settings];
            if !ok {
                bail!("macOS rejected the virtual display mode {width}x{height}");
            }
            let id: u32 = msg_send![&obj, displayID];
            if id == 0 {
                bail!("the virtual display has no display id");
            }
            tracing::info!(id, name, width, height, "virtual display created");
            Ok(VirtualDisplay { obj, _queue: queue, id })
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        tracing::info!(id = self.id, "virtual display removed");
        // Releasing the object destroys the display (its mode never changes).
        let _ = &self.obj;
    }
}

/// Waits until `display_id` has left the active display list (destruction
/// is asynchronous, spec 6.1), so its identity can be reused.
pub fn wait_gone(display_id: u32, timeout: Duration) {
    let until = Instant::now() + timeout;
    while Instant::now() < until && crate::displays::active_ids().contains(&display_id) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[allow(dead_code)]
fn _assert_selectors() -> [Sel; 1] {
    [sel!(displayID)]
}
