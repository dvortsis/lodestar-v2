//! Conservative public-unicast destinations for untrusted discovery hints.
//! Special-use ranges follow the IANA IPv4/IPv6 special-purpose registries
//! (2025-10-09). Transition, documentation and protocol-assignment ranges
//! are excluded even when a special-purpose subrange is globally reachable.
//! This policy is for outbound discovery, not inbound protocol replies.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

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

/// For the operator's resolved bootstrap seeds only: [`allows`], plus the
/// two ranges a resolver rewrites public router names into. Fake-ip DNS
/// (Clash, OpenClash, sing-box) answers from 198.18.0.0/15, and DNS64
/// synthesizes NAT64 addresses in 64:ff9b::/96 (RFC 6052) or the
/// local-use 64:ff9b:1::/96 (RFC 8215); a NAT64 address counts only when
/// the IPv4 address it embeds is public, and other local-use prefix lengths
/// are refused because they can't be decoded safely. Every other special range, a zero
/// port and a scoped IPv6 destination stay refused. Never apply this to an
/// address learned from the network.
pub fn allows_seed(addr: SocketAddr, allow_private: bool) -> bool {
    if allows(addr, allow_private) {
        return true;
    }
    if matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0) {
        return false;
    }
    let addr = normalize(addr);
    addr.port() != 0 && rewritten_public_name(addr.ip())
}

fn rewritten_public_name(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            a == 198 && matches!(b, 18 | 19)
        }
        // Only a NAT64 address whose embedded IPv4 (RFC 6052) is itself
        // public: 64:ff9b::7f00:1 is loopback through the translator.
        IpAddr::V6(ip) => nat64_embedded(ip).is_some_and(|v4| public_ip(IpAddr::V4(v4))),
    }
}

/// The IPv4 address inside a NAT64 address: the last 32 bits of the
/// well-known 64:ff9b::/96, or of 64:ff9b:1::/96. RFC 8215 lets an operator
/// embed at any RFC 6052 length inside the local-use 64:ff9b:1::/48, and a
/// /48, /56 or /64 layout can't be told apart from the address alone (one
/// can decode as public while the translator reaches 10/8), so only the
/// /96 form is decoded. Any other layout whose bits 48–95 are zero embeds
/// an address in 0.0.0.0/8, which is refused anyway.
fn nat64_embedded(ip: std::net::Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let well_known = s[2..6] == [0; 4];
    let local_use_96 = s[2] == 1 && s[3..6] == [0; 3];
    (s[0] == 0x64 && s[1] == 0xff9b && (well_known || local_use_96))
        .then(|| Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7])))
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
    fn seeds_add_only_fake_ip_and_nat64() {
        let seed = |a: &str| allows_seed(a.parse().unwrap(), false);
        // Fake-ip and NAT64 answers for a configured router name, and a
        // public one.
        for a in [
            "198.18.0.7:6881",
            "198.19.255.255:6881",
            "[::ffff:198.18.0.7]:6881",
            // 93.184.216.34 embedded in each accepted NAT64 form.
            "[64:ff9b::5db8:d822]:6881",
            "[64:ff9b:1::5db8:d822]:6881",
            "67.215.246.10:6881",
        ] {
            assert!(seed(a), "{a}");
        }
        // Every other special range stays refused, as do unusable ports
        // and scoped destinations.
        for a in [
            "198.17.255.255:0",
            "198.18.0.7:0",
            "10.0.0.1:6881",
            "127.0.0.1:6881",
            "169.254.169.254:80",
            "100.64.0.1:6881",
            "192.168.1.1:6881",
            "0.0.0.0:6881",
            "255.255.255.255:6881",
            "224.0.0.1:6881",
            "[::]:6881",
            "[::1]:6881",
            "[fe80::1]:6881",
            "[fd00::1]:6881",
            "[ff02::1]:6881",
            "[::ffff:127.0.0.1]:6881",
            "[64:ff9b::5db8:d822]:0",
            "[64:ff9b:0:1::1]:6881",
            "[64:ff9b:2::1]:6881",
        ] {
            assert!(!seed(a), "{a}");
        }
        // NAT64 addresses embedding loopback, 10/8 and the metadata address
        // in each accepted /96, and local-use layouts other than /96: a /48
        // with 93.184.216.34, a /64 with 10.0.0.1 that a /48 reading would
        // decode as the public 171.205.10.0, and a /56 with 10.0.0.1. The
        // public policy and the seed check refuse each.
        for a in [
            "[64:ff9b::7f00:1]:6881",
            "[64:ff9b::a00:1]:6881",
            "[64:ff9b::a9fe:a9fe]:6881",
            "[64:ff9b:1::7f00:1]:6881",
            "[64:ff9b:1::a00:1]:6881",
            "[64:ff9b:1::a9fe:a9fe]:6881",
            "[64:ff9b:1:5db8:d8:2200::]:6881",
            "[64:ff9b:1:abcd:a:0:100::]:6881",
            "[64:ff9b:1:ab0a:0:1::]:6881",
        ] {
            let addr: SocketAddr = a.parse().unwrap();
            assert!(!allows(addr, false), "{a}");
            assert!(!allows_seed(addr, false), "{a}");
            assert!(allows_seed(addr, true), "a local override: {a}");
        }
        let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(
            "64:ff9b::5db8:d822".parse().unwrap(),
            6881,
            0,
            3,
        ));
        assert!(!seed(&scoped.to_string()) && !allows_seed(scoped, false));
    }
}
