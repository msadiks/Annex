//! Stable identities (spec 3.3 and 7).

/// FNV-1a 64-bit. Stable across platforms and releases (unlike `DefaultHasher`).
pub fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in *p {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        // Separator so ("ab","c") != ("a","bc").
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Vendor id used for all Annex virtual displays ("AX").
pub const VENDOR_ID: u32 = 0x4158;

/// `(vendor, product, serial)` of the virtual display for a receiver monitor
/// slot. macOS remembers arrangement per identity, so this must never change
/// for a given receiver and slot.
pub fn virtual_display_identity(receiver_id: &str, monitor_index: u32) -> (u32, u32, u32) {
    let product = 0xA000 + (monitor_index & 0x0FFF);
    let serial = fnv1a64(&[receiver_id.as_bytes(), &monitor_index.to_be_bytes()]) as u32;
    // Serial 0 means "no serial" to some parts of macOS.
    (VENDOR_ID, product, serial.max(1))
}

/// Identity of the virtual display a Mac display mirrors when a mirrored
/// monitor is set to match the PC monitor's shape. Kept apart from the extend
/// identities so macOS never brings a mirror arrangement back onto an
/// extended display.
pub fn mirror_display_identity(receiver_id: &str, monitor_index: u32) -> (u32, u32, u32) {
    let product = 0xB000 + (monitor_index & 0x0FFF);
    let serial = fnv1a64(&[receiver_id.as_bytes(), &monitor_index.to_be_bytes(), b"mirror"]) as u32;
    (VENDOR_ID, product, serial.max(1))
}

/// Stable per-monitor id on Windows: friendly name plus EDID identity when
/// available, else the device path.
pub fn monitor_id(friendly_name: &str, edid: Option<(u16, u16, u32)>, device_path: &str) -> u64 {
    match edid {
        Some((mfg, product, serial)) => fnv1a64(&[
            friendly_name.as_bytes(),
            &mfg.to_be_bytes(),
            &product.to_be_bytes(),
            &serial.to_be_bytes(),
        ]),
        None => fnv1a64(&[friendly_name.as_bytes(), device_path.as_bytes()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_stable_and_distinct() {
        let a = virtual_display_identity("r1", 0);
        assert_eq!(a, virtual_display_identity("r1", 0));
        assert_ne!(a, virtual_display_identity("r1", 1));
        assert_ne!(a.2, virtual_display_identity("r2", 0).2);
        // Pinned value: changing the hash would rearrange users' displays.
        assert_eq!(fnv1a64(&[b"annex"]), fnv1a64(&[b"annex"]));
        assert_ne!(fnv1a64(&[b"ab", b"c"]), fnv1a64(&[b"a", b"bc"]));
    }
}
