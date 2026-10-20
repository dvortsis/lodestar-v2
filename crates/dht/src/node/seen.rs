//! The time-rotated "seen" filter (architecture §5, *Bounded memory*).
//!
//! BEP 51 sampling returns a flood of info hashes, most of them dead or
//! already seen. Two Bloom-filter generations remember recent sightings:
//! lookups check both, inserts go to the current one, and when it fills
//! up the old generation is dropped. Memory is fixed no matter how many
//! hashes stream past, at the price of forgetting old sightings, which
//! is fine: a hash seen again much later is worth another look.
//!
//! Positions come from a keyed hash, so a peer can't craft hashes that
//! collide in our filter.

use std::hash::BuildHasher;

use lodestar_metainfo::InfoHash;

#[derive(Debug, Clone)]
struct Bloom {
    bits: Vec<u64>,
    items: usize,
}

impl Bloom {
    fn new(bits: usize) -> Bloom {
        Bloom {
            bits: vec![0; bits.div_ceil(64)],
            items: 0,
        }
    }

    fn len_bits(&self) -> u64 {
        self.bits.len() as u64 * 64
    }

    fn contains(&self, h: (u64, u64), k: u32) -> bool {
        (0..k).all(|i| {
            let bit = h.0.wrapping_add(u64::from(i).wrapping_mul(h.1)) % self.len_bits();
            self.bits[(bit / 64) as usize] & (1 << (bit % 64)) != 0
        })
    }

    fn insert(&mut self, h: (u64, u64), k: u32) {
        for i in 0..k {
            let bit = h.0.wrapping_add(u64::from(i).wrapping_mul(h.1)) % self.len_bits();
            self.bits[(bit / 64) as usize] |= 1 << (bit % 64);
        }
        self.items += 1;
    }

    fn clear(&mut self) {
        self.bits.fill(0);
        self.items = 0;
    }
}

#[derive(Debug)]
pub(crate) struct SeenFilter {
    current: Bloom,
    previous: Bloom,
    capacity: usize,
    k: u32,
    hasher: std::collections::hash_map::RandomState,
}

impl SeenFilter {
    /// A filter remembering at least `capacity` and at most `2 * capacity`
    /// recent hashes, at roughly `fp_rate` false positives.
    pub fn new(capacity: usize, fp_rate: f64) -> SeenFilter {
        let capacity = capacity.max(1);
        let ln2 = std::f64::consts::LN_2;
        let bits = (-(capacity as f64) * fp_rate.ln() / (ln2 * ln2)).ceil() as usize;
        let k = ((bits as f64 / capacity as f64) * ln2)
            .round()
            .clamp(1.0, 16.0) as u32;
        SeenFilter {
            current: Bloom::new(bits.max(64)),
            previous: Bloom::new(bits.max(64)),
            capacity,
            k,
            hasher: std::collections::hash_map::RandomState::new(),
        }
    }

    fn positions(&self, hash: &InfoHash) -> (u64, u64) {
        let h1 = self.hasher.hash_one(hash.0);
        // An odd step visits distinct positions for any table size.
        let h2 = self.hasher.hash_one((hash.0, 0x9e37_79b9_7f4a_7c15u64)) | 1;
        (h1, h2)
    }

    /// Records a sighting; returns whether the hash was new.
    pub fn insert(&mut self, hash: &InfoHash) -> bool {
        let h = self.positions(hash);
        if self.current.contains(h, self.k) || self.previous.contains(h, self.k) {
            return false;
        }
        if self.current.items >= self.capacity {
            std::mem::swap(&mut self.current, &mut self.previous);
            self.current.clear();
        }
        self.current.insert(h, self.k);
        true
    }

    /// Bytes held by both generations.
    pub fn memory(&self) -> usize {
        (self.current.bits.len() + self.previous.bits.len()) * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(i: u32) -> InfoHash {
        let mut x = [0u8; 20];
        x[..4].copy_from_slice(&i.to_le_bytes());
        InfoHash(x)
    }

    #[test]
    fn remembers_recent_hashes() {
        let mut f = SeenFilter::new(1000, 0.01);
        let new = (0..1000).filter(|i| f.insert(&h(*i))).count();
        assert!(
            new >= 980,
            "only {new} of 1000 distinct hashes counted as new"
        );
        for i in 0..1000 {
            assert!(!f.insert(&h(i)), "{i} should be remembered");
        }
    }

    #[test]
    fn false_positive_rate_is_near_target() {
        let mut f = SeenFilter::new(10_000, 0.01);
        for i in 0..10_000 {
            f.insert(&h(i));
        }
        let fp = (1_000_000..1_020_000).filter(|i| !f.insert(&h(*i))).count();
        // 20k probes into a filter at capacity: expect ~1% plus rotation noise.
        assert!(fp < 600, "false positives: {fp}");
    }

    #[test]
    fn memory_is_bounded_and_old_hashes_are_forgotten() {
        let mut f = SeenFilter::new(100, 0.01);
        let mem = f.memory();
        for i in 0..100_000 {
            f.insert(&h(i));
        }
        assert_eq!(f.memory(), mem);
        // The first hashes rotated out long ago.
        let forgotten = (0..100).filter(|i| f.insert(&h(*i))).count();
        assert!(forgotten > 90, "only {forgotten} forgotten");
    }
}
