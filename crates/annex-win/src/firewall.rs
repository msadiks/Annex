//! Windows Firewall: the Mac connects in to this PC (TCP control port, UDP
//! video port, mDNS), so Annex needs an inbound allow rule. The installer adds
//! one; this module checks whether traffic would get through on the networks
//! the PC is on now, and fixes the rules from an elevated copy of Annex.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, VARIANT_BOOL, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::WindowsFirewall::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{Interface, PCWSTR, w};

/// What the firewall does to a Mac trying to reach this PC right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Checking,
    /// Allowed on at least one network the PC is on (or the firewall is off).
    Allowed,
    /// No rule for Annex: Windows drops the Mac's connections.
    NoRule,
    /// A rule blocks Annex (for example after *Cancel* on Windows' prompt).
    Blocked,
    /// Allowed on private networks, but this network is public.
    PublicNetwork,
    /// The firewall can't be queried (another firewall product, policy).
    Unknown,
}

impl Status {
    pub fn key(self) -> &'static str {
        match self {
            Status::Checking => "checking",
            Status::Allowed => "ok",
            Status::NoRule => "norule",
            Status::Blocked => "blocked",
            Status::PublicNetwork => "public",
            Status::Unknown => "unknown",
        }
    }

    pub fn needs_action(self) -> bool {
        matches!(self, Status::NoRule | Status::Blocked | Status::PublicNetwork)
    }
}

const PROFILES: [i32; 3] = [NET_FW_PROFILE2_DOMAIN.0, NET_FW_PROFILE2_PRIVATE.0, NET_FW_PROFILE2_PUBLIC.0];
const TCP: i32 = 6;
const UDP: i32 = 17;
const ANY: i32 = 256;

struct Rule {
    allow: bool,
    profiles: i32,
    protocol: i32,
    ports: String,
}

fn ports_match(spec: &str, port: u16) -> bool {
    let spec = spec.trim();
    if spec.is_empty() || spec == "*" {
        return true;
    }
    spec.split(',').any(|p| {
        let p = p.trim();
        match p.split_once('-') {
            Some((a, b)) => matches!((a.parse::<u16>(), b.parse::<u16>()), (Ok(a), Ok(b)) if a <= port && port <= b),
            None => p.parse::<u16>() == Ok(port),
        }
    })
}

impl Rule {
    fn covers(&self, profile: i32, protocol: i32, port: u16) -> bool {
        self.profiles & profile != 0 && (self.protocol == ANY || self.protocol == protocol) && ports_match(&self.ports, port)
    }
}

fn expand(s: &str) -> String {
    let src: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 1024];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

fn same_path(rule_app: &str, exe: &str) -> bool {
    expand(rule_app).eq_ignore_ascii_case(exe)
}

/// Inbound rules that apply to this executable (or to every program).
fn rules_for(policy: &INetFwPolicy2, exe: &str) -> windows::core::Result<Vec<Rule>> {
    let mut out = Vec::new();
    unsafe {
        let rules = policy.Rules()?;
        let en: IEnumVARIANT = rules._NewEnum()?.cast()?;
        loop {
            let mut v = [VARIANT::default()];
            let mut got = 0u32;
            if en.Next(&mut v, &mut got).is_err() || got == 0 {
                break;
            }
            let Ok(rule) = IDispatch::try_from(&v[0]).and_then(|d| d.cast::<INetFwRule>()) else { continue };
            if rule.Direction().ok() != Some(NET_FW_RULE_DIR_IN) || rule.Enabled().ok() != Some(VARIANT_BOOL::from(true)) {
                continue;
            }
            let app = rule.ApplicationName().map(|b| b.to_string()).unwrap_or_default();
            let service = rule.ServiceName().map(|b| b.to_string()).unwrap_or_default();
            let applies = if app.is_empty() { service.is_empty() || service == "*" } else { same_path(&app, exe) };
            if !applies {
                continue;
            }
            out.push(Rule {
                allow: rule.Action().ok() == Some(NET_FW_ACTION_ALLOW),
                profiles: rule.Profiles().unwrap_or(0),
                protocol: rule.Protocol().unwrap_or(ANY),
                ports: rule.LocalPorts().map(|b| b.to_string()).unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

fn evaluate(exe: &str, tcp: u16, udp: u16) -> windows::core::Result<Status> {
    let policy: INetFwPolicy2 = unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)? };
    let active = unsafe { policy.CurrentProfileTypes()? };
    let rules = rules_for(&policy, exe)?;
    let open = |profile: i32, proto: i32, port: u16| {
        let blocked = rules.iter().any(|r| !r.allow && r.covers(profile, proto, port));
        let allowed = rules.iter().any(|r| r.allow && r.covers(profile, proto, port));
        (allowed && !blocked, blocked)
    };
    let mut any_block = false;
    for p in PROFILES.into_iter().filter(|p| active & p != 0) {
        let prof = NET_FW_PROFILE_TYPE2(p);
        unsafe {
            if !policy.get_FirewallEnabled(prof)?.as_bool() {
                return Ok(Status::Allowed);
            }
            if policy.get_BlockAllInboundTraffic(prof)?.as_bool() {
                any_block = true;
                continue;
            }
            let default_allow = policy.get_DefaultInboundAction(prof)? == NET_FW_ACTION_ALLOW;
            let (t_ok, t_block) = open(p, TCP, tcp);
            let (u_ok, u_block) = open(p, UDP, udp);
            any_block |= t_block || u_block;
            if (t_ok || (default_allow && !t_block)) && (u_ok || (default_allow && !u_block)) {
                return Ok(Status::Allowed);
            }
        }
    }
    let private_ok = open(NET_FW_PROFILE2_PRIVATE.0, TCP, tcp).0 && open(NET_FW_PROFILE2_PRIVATE.0, UDP, udp).0;
    Ok(if active & NET_FW_PROFILE2_PUBLIC.0 != 0 && private_ok {
        Status::PublicNetwork
    } else if any_block {
        Status::Blocked
    } else {
        Status::NoRule
    })
}

/// Checks the rules for this executable. Takes a moment (it walks every
/// firewall rule), so call it off the UI thread.
pub fn check(tcp: u16, udp: u16) -> Status {
    let exe = exe_path();
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let r = evaluate(&exe, tcp, udp);
        if init {
            CoUninitialize();
        }
        match r {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("firewall check failed: {e}");
                Status::Unknown
            }
        }
    }
}

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
}

fn netsh(args: &[&str]) -> bool {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// mDNS, which the Mac uses to find this PC.
const MDNS_PORT: u16 = 5353;

/// The ports [`allow`] opens: `--ports=<tcp>,<udp>` when given (the window's
/// "Allow" button passes them), else the ones in the settings, else the
/// defaults.
pub fn ports(args: &[String]) -> (u16, u16) {
    let given = args.iter().find_map(|a| a.strip_prefix("--ports=")).and_then(|p| {
        let (tcp, udp) = p.split_once(',')?;
        Some((tcp.trim().parse().ok()?, udp.trim().parse().ok()?))
    });
    given
        .or_else(|| {
            let path = crate::misc::data_dir().join("config.toml");
            let cfg = annex_core::config::load::<annex_core::config::ReceiverConfig>(&path).ok()??;
            Some((cfg.control_port, cfg.video_port))
        })
        .unwrap_or((annex_proto::DEFAULT_CONTROL_PORT, annex_proto::DEFAULT_VIDEO_PORT))
}

/// `annex.exe --allow-firewall[=private] [--ports=<tcp>,<udp>]`, run elevated
/// (by the installer or via [`fix`]): replaces every rule for this executable
/// with inbound allow rules for Annex's own ports only (TCP control, UDP video
/// and mDNS) on private and domain networks and, unless `public` is false, on
/// public networks limited to the local subnet. Windows 11 marks new networks
/// Public, and Annex only talks to paired Macs on the LAN anyway.
pub fn allow(exe: &Path, public: bool, tcp: u16, udp: u16) -> bool {
    remove(exe);
    let program = format!("program={}", exe.display());
    let tcp_ports = format!("localport={tcp}");
    let udp_ports = if udp == MDNS_PORT { format!("localport={udp}") } else { format!("localport={udp},{MDNS_PORT}") };
    let mut ok = true;
    for (protocol, ports) in [("protocol=TCP", tcp_ports.as_str()), ("protocol=UDP", udp_ports.as_str())] {
        ok &= netsh(&[
            "advfirewall", "firewall", "add", "rule", "name=Annex", "dir=in", "action=allow", &program, protocol, ports,
            "enable=yes", "profile=private,domain", "description=Lets your Mac connect to Annex on this PC.",
        ]);
        if public {
            ok &= netsh(&[
                "advfirewall", "firewall", "add", "rule", "name=Annex (public networks)", "dir=in", "action=allow", &program,
                protocol, ports, "enable=yes", "profile=public", "remoteip=localsubnet",
                "description=Lets your Mac connect to Annex on public networks, from this PC's own subnet only.",
            ]);
        }
    }
    ok
}

/// `annex.exe --remove-firewall`: deletes every rule for this executable.
pub fn remove(exe: &Path) -> bool {
    let program = format!("program={}", exe.display());
    netsh(&["advfirewall", "firewall", "delete", "rule", "name=all", &program])
}

/// Re-runs Annex elevated (one UAC prompt) to open `tcp` and `udp` and waits
/// for it. Returns false if the user declined.
pub fn fix(tcp: u16, udp: u16) -> bool {
    let exe: Vec<u16> = exe_path().encode_utf16().chain(std::iter::once(0)).collect();
    let args: Vec<u16> = format!("--allow-firewall --ports={tcp},{udp}").encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            if e.code() != ERROR_CANCELLED.to_hresult() {
                tracing::warn!("firewall fix: {e}");
            }
            return false;
        }
        if info.hProcess.is_invalid() {
            return true;
        }
        let mut code = 1u32;
        if WaitForSingleObject(info.hProcess, INFINITE) == WAIT_OBJECT_0 {
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
        let _ = CloseHandle(info.hProcess);
        code == 0
    }
}

#[cfg(test)]
mod tests {
    use super::ports_match;

    #[test]
    fn ports() {
        assert!(ports_match("*", 48800));
        assert!(ports_match("", 48800));
        assert!(ports_match("80,48800", 48800));
        assert!(ports_match("48000-49000", 48801));
        assert!(!ports_match("80,443", 48800));
        assert!(!ports_match("RPC", 48800));
    }
}
