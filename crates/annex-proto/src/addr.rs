//! Address policy: Annex only talks to machines on the local network.
//!
//! Accepted: RFC 1918 private IPv4, IPv4 link-local, IPv6 link-local and IPv6
//! unique-local addresses. Loopback is accepted too, so both ends can run on one
//! machine for testing. Everything else (public addresses, multicast, CGNAT...)
//! is rejected.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Returns true if `ip` is an address Annex is willing to talk to.
pub fn is_lan_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_lan_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan_v4(v4);
            }
            is_lan_v6(v6)
        }
    }
}

/// Same as [`is_lan_addr`] for a socket address.
pub fn is_lan_socket(addr: SocketAddr) -> bool {
    is_lan_addr(addr.ip())
}

fn is_lan_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || ip.is_loopback()
}

fn is_lan_v6(ip: Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    let link_local = (seg0 & 0xffc0) == 0xfe80;
    let unique_local = (seg0 & 0xfe00) == 0xfc00;
    link_local || unique_local || ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn accepts_private_ranges() {
        for a in [
            "10.0.0.1",
            "10.255.255.254",
            "172.16.0.1",
            "172.31.255.1",
            "192.168.1.20",
            "169.254.3.4",
            "127.0.0.1",
            "fe80::1",
            "fd12:3456::1",
            "::1",
            "::ffff:192.168.0.2",
        ] {
            assert!(is_lan_addr(ip(a)), "{a} should be accepted");
        }
    }

    #[test]
    fn rejects_public_and_odd_ranges() {
        for a in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "224.0.0.251",
            "0.0.0.0",
            "2001:db8::1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "ff02::fb",
        ] {
            assert!(!is_lan_addr(ip(a)), "{a} should be rejected");
        }
    }
}
