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

/// For the operator's resolved bootstrap seeds only: fake-ip DNS
/// (198.18.0.0/15) and DNS64/NAT64 (64:ff9b::/96) answer the configured
/// router names inside special ranges, so the special-range check is
/// skipped. A zero port, an unspecified, broadcast or multicast address and
/// a scoped IPv6 destination are still refused. Never apply this to an
/// address learned from the network.
pub fn allows_seed(addr: SocketAddr, allow_private: bool) -> bool {
    if !allow_private && matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0) {
        return false;
    }
    let addr = normalize(addr);
    let ip = addr.ip();
    let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
    addr.port() != 0 && !ip.is_unspecified() && !ip.is_multicast() && !broadcast
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
                    && (b == 168
                        || (b == 0 && matches!(c, 0 | 2))
                        || (b == 88 && c == 99)
                        // AS112-v4, AMT, AS112 direct delegation.
                        || (b == 31 && c == 196)
                        || (b == 52 && c == 193)
                        || (b == 175 && c == 48)))
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
                // AS112 direct delegation, 2620:4f:8000::/48.
                && !(s[0] == 0x2620 && s[1] == 0x4f && s[2] == 0x8000)
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
            "192.31.196.1",
            "192.52.193.1",
            "192.175.48.1",
            "2620:4f:8000::1",
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
            "192.31.197.1",
            "192.52.192.255",
            "192.175.49.1",
            "2620:4f:8001::1",
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

    #[test]
    fn range_edges() {
        let ok = |ip: &str| allows(SocketAddr::new(ip.parse().unwrap(), 6881), false);
        // 100.64.0.0/10 (shared address space) is exactly that wide.
        assert!(ok("100.63.255.255"));
        assert!(!ok("100.64.0.0"));
        assert!(!ok("100.127.255.255"));
        assert!(ok("100.128.0.0"));
        // 192.0.0.0/24 and 192.0.2.0/24 are special; 192.0.1.0/24 isn't.
        assert!(!ok("192.0.0.255"));
        assert!(ok("192.0.1.0"));
        assert!(ok("192.0.1.255"));
        assert!(!ok("192.0.2.0"));
        // 198.18.0.0/15 (benchmarking, fake-ip DNS).
        assert!(ok("198.17.255.255"));
        assert!(!ok("198.18.0.0"));
        assert!(!ok("198.19.255.255"));
        assert!(ok("198.20.0.0"));
        assert!(ok("2620:4f:7fff:ffff::1"));
        assert!(!ok("2620:4f:8000:ffff::1"));
    }

    #[test]
    fn scoped_ipv6_is_rejected_unless_local() {
        let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(
            "2606:4700::1111".parse().unwrap(),
            6881,
            0,
            3,
        ));
        assert!(!allows(scoped, false));
        assert!(allows(scoped, true));
        assert!(!allows_seed(scoped, false));
        assert!(allows_seed(scoped, true));
    }

    #[test]
    fn seeds_skip_only_the_special_range_check() {
        let seed = |a: &str| allows_seed(a.parse().unwrap(), false);
        // Fake-ip, NAT64 and private answers for a configured router name.
        for a in [
            "198.18.0.7:6881",
            "[64:ff9b::5db8:d822]:6881",
            "10.0.0.1:6881",
            "[::ffff:198.18.0.7]:6881",
            "67.215.246.10:6881",
        ] {
            assert!(seed(a), "{a}");
        }
        for a in [
            "198.18.0.7:0",
            "0.0.0.0:6881",
            "[::]:6881",
            "255.255.255.255:6881",
            "224.0.0.1:6881",
            "[ff02::1]:6881",
            "[::ffff:224.0.0.1]:6881",
        ] {
            assert!(!seed(a), "{a}");
        }
    }
}
