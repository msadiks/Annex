//! mDNS discovery (spec 8.1).
//!
//! The receiver advertises `_annex._tcp.local.` with TXT
//! `v=1 id=<receiver-id> name=<hostname> cp=<control port> vp=<video port> pair=<0|1>`.
//! The sender browses and ignores addresses outside the LAN ranges.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::SERVICE_TYPE;
use crate::addr::is_lan_addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverAd {
    pub id: String,
    pub name: String,
    pub control_port: u16,
    pub video_port: u16,
    /// The receiver is showing a PIN and accepts pairing.
    pub pairing: bool,
}

#[derive(Debug, Clone)]
pub struct DiscoveredReceiver {
    pub id: String,
    pub name: String,
    /// mDNS host name, e.g. `PC.local.`
    pub host: String,
    pub addrs: Vec<IpAddr>,
    pub control_port: u16,
    pub video_port: u16,
    pub pairing: bool,
    pub last_seen: Instant,
}

fn host_label(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "annex".into() } else { s.to_string() }
}

fn instance_name(ad: &ReceiverAd) -> String {
    let short: String = ad.id.chars().filter(|c| c.is_ascii_hexdigit()).take(8).collect();
    format!("Annex {} {}", host_label(&ad.name), short)
}

fn service_info(ad: &ReceiverAd) -> Result<ServiceInfo, mdns_sd::Error> {
    let props = [
        ("v", "1".to_string()),
        ("id", ad.id.clone()),
        ("name", ad.name.clone()),
        ("cp", ad.control_port.to_string()),
        ("vp", ad.video_port.to_string()),
        ("pair", if ad.pairing { "1" } else { "0" }.to_string()),
    ];
    let host = format!("{}.local.", host_label(&ad.name));
    Ok(ServiceInfo::new(SERVICE_TYPE, &instance_name(ad), &host, "", ad.control_port, &props[..])?
        .enable_addr_auto())
}

/// Receiver-side advertisement. Unregisters on drop.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    current: ReceiverAd,
}

impl Advertiser {
    pub fn start(ad: ReceiverAd) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let info = service_info(&ad)?;
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;
        Ok(Advertiser { daemon, fullname, current: ad })
    }

    /// Re-announces with new TXT values (e.g. pairing mode toggled).
    pub fn update(&mut self, ad: ReceiverAd) -> Result<(), mdns_sd::Error> {
        if ad == self.current {
            return Ok(());
        }
        let info = service_info(&ad)?;
        self.fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        self.current = ad;
        Ok(())
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(300));
        }
        let _ = self.daemon.shutdown();
    }
}

/// Sender-side browser.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    found: HashMap<String, DiscoveredReceiver>,
}

impl Browser {
    pub fn start() -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE_TYPE)?;
        Ok(Browser { daemon, events, found: HashMap::new() })
    }

    /// Drains pending events. Returns true if the set of receivers changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                ServiceEvent::ServiceResolved(svc) => {
                    let txt = &svc.txt_properties;
                    let get = |k: &str| txt.get_property_val_str(k).map(str::to_string);
                    let (Some(id), Some(v)) = (get("id"), get("v")) else { continue };
                    if v != "1" {
                        continue;
                    }
                    let addrs: Vec<IpAddr> = svc
                        .addresses
                        .iter()
                        .map(|a| a.to_ip_addr())
                        .filter(|a| is_lan_addr(*a))
                        .collect();
                    if addrs.is_empty() {
                        continue;
                    }
                    let rec = DiscoveredReceiver {
                        id,
                        name: get("name").unwrap_or_else(|| svc.host.trim_end_matches('.').to_string()),
                        host: svc.host.clone(),
                        addrs,
                        control_port: get("cp").and_then(|p| p.parse().ok()).unwrap_or(svc.port),
                        video_port: get("vp")
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(crate::DEFAULT_VIDEO_PORT),
                        pairing: get("pair").as_deref() == Some("1"),
                        last_seen: Instant::now(),
                    };
                    self.found.insert(svc.fullname.clone(), rec);
                    changed = true;
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    changed |= self.found.remove(&fullname).is_some();
                }
                _ => {}
            }
        }
        changed
    }

    pub fn receivers(&self) -> Vec<DiscoveredReceiver> {
        let mut v: Vec<_> = self.found.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn find(&self, id: &str) -> Option<DiscoveredReceiver> {
        self.found.values().find(|r| r.id == id).cloned()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
