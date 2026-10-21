//! Small system helpers: clocks, machine name, permissions.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::time::{Duration, Instant};

use objc2_core_foundation::CFString;
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

#[repr(C)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebase) -> c_int;
    fn sysctlbyname(name: *const c_char, oldp: *mut c_void, oldlenp: *mut usize, newp: *mut c_void, newlen: usize) -> c_int;
}

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyComputerName(store: *const c_void, encoding: *mut u32) -> *const CFString;
}

/// Converts a mach absolute time (e.g. SCK's `displayTime`) to an `Instant`.
pub fn instant_from_mach(t: u64) -> Instant {
    let mut tb = MachTimebase { numer: 0, denom: 0 };
    let now_mach = unsafe {
        mach_timebase_info(&mut tb);
        mach_absolute_time()
    };
    let now = Instant::now();
    if tb.denom == 0 || t > now_mach {
        return now;
    }
    let ns = (now_mach - t) as u128 * tb.numer as u128 / tb.denom as u128;
    now.checked_sub(Duration::from_nanos(ns as u64)).unwrap_or(now)
}

fn sysctl_string(name: &CStr) -> Option<String> {
    let mut len = 0usize;
    unsafe {
        if sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        CStr::from_bytes_until_nul(&buf).ok().map(|s| s.to_string_lossy().into_owned())
    }
}

/// Chip name, e.g. "Apple M1 Pro". Used for the encoder budget.
pub fn machine_name() -> String {
    sysctl_string(c"machdep.cpu.brand_string").unwrap_or_else(|| "Apple silicon".into())
}

/// The user-visible computer name ("Sadik's MacBook Air" style).
pub fn computer_name() -> Option<String> {
    unsafe {
        let s = SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut());
        if s.is_null() {
            return None;
        }
        let owned = objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(s as *mut CFString));
        Some(owned.to_string())
    }
}

pub fn has_screen_recording() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Shows the system prompt (once); returns whether access is granted now.
pub fn request_screen_recording() -> bool {
    CGRequestScreenCaptureAccess()
}

/// Wraps a value so it can cross into Apple callbacks that run on other
/// threads. Only used for CoreFoundation/ObjC objects documented as safe to
/// hand between threads (pixel buffers, sample buffers, sessions).
pub struct SendBox<T>(pub T);
unsafe impl<T> Send for SendBox<T> {}
unsafe impl<T> Sync for SendBox<T> {}
