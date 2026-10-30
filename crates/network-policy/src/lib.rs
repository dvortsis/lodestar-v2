//! Conservative public-unicast destinations for untrusted discovery hints.
//! Special-use ranges follow the IANA IPv4/IPv6 special-purpose registries
//! (2025-10-09). Transition, documentation and protocol-assignment ranges
//! are excluded even when a special-purpose subrange is globally reachable.
//! This policy is for outbound discovery, not inbound protocol replies.

use std::net::{IpAddr, SocketAddr};

/// Canonicalize mapped IPv4 before deduplication and subscribed-IP checks.
pub fn normalize(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => v6
            .ip()
            .to_ipv4_mapped()
            .map_or(addr, |ip| SocketAddr::new(IpAddr::V4(ip), v6.port())),
        _ => addr,
    }
}

/// Local overrides still reject an unusable zero port.
pub fn allows(addr: SocketAddr, allow_private: bool) -> bool {
    if !allow_private && matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0) {
        return false;
    }
    let addr = normalize(addr);
    addr.port() != 0 && (allow_private || public_ip(addr.ip()))
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192
                    && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
                && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Native global unicast only; reject scoped destinations and
            // special-purpose IETF, 6to4 and documentation allocations.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_private_reserved_and_mapped() {
        for ip in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.1.2.3",
            "169.254.1.1",
            "172.31.1.1",
            "192.168.1.1",
            "192.0.0.9",
            "192.0.2.1",
            "192.88.99.1",
            "198.19.1.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001:2::1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
            "3fff::1",
        ] {
            let addr = SocketAddr::new(ip.parse().unwrap(), 80);
            assert!(!allows(addr, false), "{ip}");
            assert!(allows(addr, true), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(
                allows(SocketAddr::new(ip.parse().unwrap(), 1), false),
                "{ip}"
            );
            assert!(!allows(SocketAddr::new(ip.parse().unwrap(), 0), true));
        }
        assert_eq!(
            normalize("[::ffff:8.8.8.8]:80".parse().unwrap()),
            "8.8.8.8:80".parse().unwrap()
        );
    }
}
