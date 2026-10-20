//! BEP 42 secure node ids: the first 21 bits of a node id are derived from
//! its external IP, which makes it expensive to place many Sybil nodes
//! next to a target. The crawler uses this both to generate its own
//! (virtual) ids and to judge other nodes' ids.

use std::net::IpAddr;

use rand::RngExt;

use crate::compact::NodeId;

/// CRC-32C (Castagnoli), the checksum BEP 42 specifies.
fn crc32c(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    };
    let mut crc = !0u32;
    for &b in data {
        crc = TABLE[((crc ^ u32::from(b)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The 32-bit hash whose top 21 bits a secure id must start with, for an
/// IP and the 3-bit `r` stored in the id's last byte.
fn prefix_hash(ip: &IpAddr, r: u8) -> u32 {
    let r = u32::from(r & 7);
    match ip {
        IpAddr::V4(v4) => {
            let masked = u32::from_be_bytes(v4.octets()) & 0x030f_3fff | (r << 29);
            crc32c(&masked.to_be_bytes())
        }
        IpAddr::V6(v6) => {
            let high = u64::from_be_bytes(v6.octets()[..8].try_into().expect("8 bytes"));
            let masked = high & 0x0103_070f_1f3f_7fff | (u64::from(r) << 61);
            crc32c(&masked.to_be_bytes())
        }
    }
}

/// Addresses BEP 42 exempts: a node can't know a meaningful external IP
/// for them, so their ids are never judged.
pub fn is_exempt(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Generates a secure id for `ip`, with `r` (0–255; only the low 3 bits
/// affect the prefix) stored in the last byte and random filler bits.
pub fn generate(ip: &IpAddr, r: u8, rng: &mut impl RngExt) -> NodeId {
    let crc = prefix_hash(ip, r);
    let mut id: [u8; 20] = rng.random();
    id[0] = (crc >> 24) as u8;
    id[1] = (crc >> 16) as u8;
    id[2] = ((crc >> 8) as u8 & 0xf8) | (id[2] & 0x07);
    id[19] = r;
    NodeId(id)
}

/// Whether `id` is a valid BEP 42 id for a node at `ip`. Exempt
/// addresses always pass.
pub fn is_valid(id: &NodeId, ip: &IpAddr) -> bool {
    if is_exempt(ip) {
        return true;
    }
    let crc = prefix_hash(ip, id.0[19]);
    id.0[0] == (crc >> 24) as u8
        && id.0[1] == (crc >> 16) as u8
        && (id.0[2] & 0xf8) == ((crc >> 8) as u8 & 0xf8)
}

/// `count` secure ids for one external IP, spread over the distinct
/// prefixes BEP 42 allows (8 per address; beyond that ids share a prefix
/// and differ in their random bits).
pub fn virtual_ids(ip: &IpAddr, count: usize, rng: &mut impl RngExt) -> Vec<NodeId> {
    (0..count)
        .map(|i| {
            // The low 3 bits pick the prefix; bits 3–5 keep the last bytes
            // of ids that share a prefix distinct (up to 64 ids).
            let r = (i % 64) as u8 | (rng.random::<u8>() & 0xc0);
            generate(ip, r, rng)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn hex(s: &str) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn crc32c_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// The test vectors from BEP 42.
    #[test]
    fn bep42_vectors() {
        let vectors = [
            ("124.31.75.21", "5fbfbff10c5d6a4ec8a88e4c6ab4c28b95eee401"),
            ("21.75.31.124", "5a3ce9c14e7a08645677bbd1cfe7d8f956d53256"),
            ("65.23.51.170", "a5d43220bc8f112a3d426c84764f8c2a1150e616"),
            ("84.124.73.14", "1b0321dd1bb1fe518101ceef99462b947a01ff41"),
            ("43.213.53.83", "e56f6cbf5b7c4be0237986d5243b87aa6d51305a"),
        ];
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        for (ip, id) in vectors {
            let ip: IpAddr = ip.parse().unwrap();
            let id = NodeId(hex(id));
            assert!(is_valid(&id, &ip), "{ip}");
            let generated = generate(&ip, id.0[19], &mut rng);
            assert_eq!(generated.0[..2], id.0[..2], "{ip}");
            assert_eq!(generated.0[2] & 0xf8, id.0[2] & 0xf8, "{ip}");
            assert_eq!(generated.0[19], id.0[19]);
            // A different address does not validate.
            assert!(!is_valid(&id, &"8.8.8.8".parse().unwrap()));
        }
    }

    #[test]
    fn ipv6_ids_validate() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        let ip: IpAddr = "2001:4860:4860::8888".parse().unwrap();
        for r in 0..=255u8 {
            let id = generate(&ip, r, &mut rng);
            assert!(is_valid(&id, &ip));
        }
        let other: IpAddr = "2a00:1450::1".parse().unwrap();
        let id = generate(&ip, 3, &mut rng);
        assert!(!is_valid(&id, &other));
    }

    #[test]
    fn exempt_addresses() {
        for ip in [
            "10.1.2.3",
            "192.168.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_exempt(&ip), "{ip}");
            assert!(is_valid(&NodeId([0; 20]), &ip));
        }
        assert!(!is_exempt(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn virtual_ids_are_distinct_and_valid() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let ip: IpAddr = "93.184.216.34".parse().unwrap();
        let ids = virtual_ids(&ip, 16, &mut rng);
        assert_eq!(ids.len(), 16);
        let prefixes: std::collections::HashSet<_> = ids
            .iter()
            .map(|i| (i.0[0], i.0[1], i.0[2] & 0xf8))
            .collect();
        assert_eq!(
            prefixes.len(),
            8,
            "eight distinct BEP 42 prefixes per address"
        );
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 16);
        assert!(ids.iter().all(|id| is_valid(id, &ip)));
    }
}
