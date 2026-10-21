//! Monitor enumeration (spec 7): DXGI outputs for geometry and adapter,
//! `EnumDisplaySettingsEx` for the current mode, `QueryDisplayConfig` for
//! friendly names and EDID ids.

use std::collections::HashMap;

use annex_core::identity::monitor_id;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{LUID, RECT};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsExW, EDS_RAWMODE};
use windows::core::PCWSTR;

#[derive(Debug, Clone)]
pub struct MonitorDesc {
    pub id: u64,
    pub name: String,
    pub rect: RECT,
    pub refresh_mhz: u32,
    pub primary: bool,
    pub adapter_luid: LUID,
}

impl MonitorDesc {
    pub fn width(&self) -> u32 {
        (self.rect.right - self.rect.left) as u32
    }

    pub fn height(&self) -> u32 {
        (self.rect.bottom - self.rect.top) as u32
    }
}

fn wstr(s: &[u16]) -> String {
    let n = s.iter().position(|c| *c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}

struct TargetInfo {
    friendly: String,
    edid: Option<(u16, u16)>,
    path: String,
}

fn display_config_names() -> HashMap<String, TargetInfo> {
    let mut out = HashMap::new();
    unsafe {
        let mut np = 0u32;
        let mut nm = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).is_err() {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            // Bit 1 of the flags union: edidIdsValid.
            let edid_valid = tgt.flags.Anonymous.value & 0x2 != 0;
            out.insert(
                wstr(&src.viewGdiDeviceName),
                TargetInfo {
                    friendly: wstr(&tgt.monitorFriendlyDeviceName),
                    edid: edid_valid.then_some((tgt.edidManufactureId, tgt.edidProductCodeId)),
                    path: wstr(&tgt.monitorDevicePath),
                },
            );
        }
    }
    out
}

fn current_refresh_mhz(gdi_name: &str) -> u32 {
    let wide: Vec<u16> = gdi_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        if EnumDisplaySettingsExW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm, EDS_RAWMODE).as_bool() {
            return dm.dmDisplayFrequency.max(1) * 1000;
        }
    }
    60_000
}

/// All monitors attached to the desktop, primary first.
pub fn enumerate() -> Vec<MonitorDesc> {
    let names = display_config_names();
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            ai += 1;
            let Ok(ad) = adapter.GetDesc1() else { continue };
            if ad.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                oi += 1;
                let Ok(od) = output.GetDesc() else { continue };
                if !od.AttachedToDesktop.as_bool() {
                    continue;
                }
                let gdi = wstr(&od.DeviceName);
                let info = names.get(&gdi);
                let friendly = info
                    .map(|i| i.friendly.clone())
                    .filter(|f| !f.is_empty())
                    .unwrap_or_else(|| gdi.trim_start_matches(r"\\.\").to_string());
                let id = monitor_id(
                    &friendly,
                    info.and_then(|i| i.edid).map(|(m, p)| (m, p, 0)),
                    info.map(|i| i.path.as_str()).unwrap_or(&gdi),
                );
                let r = od.DesktopCoordinates;
                out.push(MonitorDesc {
                    id,
                    name: friendly,
                    refresh_mhz: current_refresh_mhz(&gdi),
                    primary: r.left == 0 && r.top == 0,
                    rect: r,
                    adapter_luid: ad.AdapterLuid,
                });
            }
        }
    }
    // Disambiguate identical names (two of the same model).
    let mut seen: HashMap<String, u32> = HashMap::new();
    for m in out.iter_mut() {
        let n = seen.entry(m.name.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            m.name = format!("{} ({})", m.name, n);
        }
    }
    out.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    out
}
