//! BEP 33 scrape bloom filters: 256 bytes (m = 2048 bits), k = 2.
//!
//! A node storing peers for an info hash answers a `get_peers` with
//! `scrape=1` with two filters, `BFsd` (seeds) and `BFpe` (other peers),
//! each holding the IP addresses it knows. ORing the filters of the nodes
//! closest to the hash and estimating the number of distinct members
//! gives a swarm size without learning a single address.
//!
//! Each IP (its 4 or 16 raw bytes) is hashed with SHA-1; bytes 0–1 and
//! 2–3, read little-endian, give two bit indexes mod 2048, and bit
//! `i % 8` of byte `i / 8` is set.

use std::net::IpAddr;

use sha1::{Digest, Sha1};

/// Filter size in bytes.
pub const BYTES: usize = 256;
const M: f64 = (BYTES * 8) as f64;
const K: f64 = 2.0;

/// One BEP 33 filter.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bloom(pub [u8; BYTES]);

impl Default for Bloom {
    fn default() -> Self {
        Bloom([0; BYTES])
    }
}

impl std::fmt::Debug for Bloom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bloom(≈{:.1})", self.estimate())
    }
}

impl Bloom {
    pub fn insert(&mut self, ip: IpAddr) {
        let digest = match ip {
            IpAddr::V4(v4) => Sha1::digest(v4.octets()),
            IpAddr::V6(v6) => Sha1::digest(v6.octets()),
        };
        for pair in [[digest[0], digest[1]], [digest[2], digest[3]]] {
            let i = usize::from(u16::from_le_bytes(pair)) % (BYTES * 8);
            self.0[i / 8] |= 1 << (i % 8);
        }
    }

    pub fn union(&mut self, other: &Bloom) {
        for (a, b) in self.0.iter_mut().zip(other.0.iter()) {
            *a |= b;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|b| *b == 0)
    }

    /// The estimated number of distinct addresses:
    /// `ln(c/m) / (k·ln(1 − 1/m))` with `c` the zero bits. An empty filter
    /// is 0; a full one saturates at the estimate for a single zero bit
    /// (about 7,800), the most a filter can tell apart.
    pub fn estimate(&self) -> f64 {
        let zeros: u32 = self.0.iter().map(|b| b.count_zeros()).sum();
        if f64::from(zeros) >= M {
            return 0.0;
        }
        let c = f64::from(zeros.max(1));
        (c / M).ln() / (K * (-1.0 / M).ln_1p())
    }

    /// [`Bloom::estimate`], rounded.
    pub fn count(&self) -> u32 {
        self.estimate().round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// The BEP's own test vector: 192.0.2.0–192.0.2.255 and
    /// 2001:db8::–2001:db8::3e7 estimate to 1224.93. A different bit or
    /// byte order would land elsewhere.
    #[test]
    fn bep33_test_vector() {
        let mut b = Bloom::default();
        for i in 0..=255u8 {
            b.insert(IpAddr::V4(Ipv4Addr::new(192, 0, 2, i)));
        }
        for i in 0..1000u16 {
            b.insert(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, i)));
        }
        let e = b.estimate();
        assert!((e - 1224.93).abs() < 0.01, "estimate {e}");
    }

    #[test]
    fn empty_single_full_and_union() {
        let mut a = Bloom::default();
        assert!(a.is_empty());
        assert_eq!(a.count(), 0);
        a.insert("10.0.0.1".parse().unwrap());
        a.insert("10.0.0.1".parse().unwrap());
        assert_eq!(a.count(), 1, "duplicates count once");
        let mut b = Bloom::default();
        b.insert("10.0.0.2".parse().unwrap());
        b.union(&a);
        assert_eq!(b.count(), 2);
        let full = Bloom([0xff; BYTES]);
        assert!((7_000..8_000).contains(&full.count()), "{}", full.count());
    }

    #[test]
    fn estimates_track_the_true_count() {
        for n in [10u32, 100, 500, 2000] {
            let mut b = Bloom::default();
            for i in 0..n {
                b.insert(IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i)));
            }
            let e = b.estimate();
            let err = (e - f64::from(n)).abs() / f64::from(n);
            assert!(err < 0.1, "n={n} estimate={e}");
        }
    }
}
