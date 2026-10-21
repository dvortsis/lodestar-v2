//! The "have" filter (architecture §5): is this hash already stored?
//!
//! A Bloom filter over every stored swarm hash, about 1.25 bytes per hash
//! at a ~1% false-positive rate. It never says "no" for a stored hash, so a
//! "no" goes straight to the fetch pool. A "yes" is usually a rediscovery,
//! and the writer confirms it against the store; the rare false positive
//! is sent on to the pool from there, so nothing new is lost to it.
//!
//! Reads and inserts are lock-free (atomic words). When the filter fills
//! past its capacity it's rebuilt at twice the size from the store.

use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lodestar_metainfo::InfoHash;

const BITS_PER_ITEM: u64 = 10;
const HASHES: u64 = 7;
const MIN_CAPACITY: u64 = 1 << 20;

/// The shared filter; [`Have::replace`] swaps in a rebuilt one.
#[derive(Debug)]
pub struct Have(RwLock<Arc<Bloom>>);

impl Have {
    pub fn new(capacity: u64) -> Have {
        Have(RwLock::new(Arc::new(Bloom::new(capacity))))
    }

    fn current(&self) -> Arc<Bloom> {
        self.0.read().expect("have lock").clone()
    }

    pub fn contains(&self, hash: &InfoHash) -> bool {
        self.current().contains(hash)
    }

    pub fn insert(&self, hash: &InfoHash) {
        self.current().insert(hash);
    }

    /// Past capacity, the false-positive rate climbs; time to rebuild.
    pub fn is_full(&self) -> bool {
        let b = self.current();
        b.count.load(Ordering::Relaxed) > b.capacity
    }

    pub fn len(&self) -> u64 {
        self.current().count.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> u64 {
        self.current().capacity
    }

    pub fn memory_bytes(&self) -> u64 {
        self.current().bits.len() as u64 * 8
    }

    pub fn replace(&self, bloom: Bloom) {
        *self.0.write().expect("have lock") = Arc::new(bloom);
    }
}

/// A capacity for `stored` hashes with room to grow.
pub fn capacity_for(stored: u64) -> u64 {
    (stored.saturating_mul(2)).max(MIN_CAPACITY)
}

#[derive(Debug)]
pub struct Bloom {
    bits: Vec<AtomicU64>,
    capacity: u64,
    count: AtomicU64,
    /// Keyed so peers can't pick hashes that collide in our filter.
    keys: (RandomState, RandomState),
}

impl Bloom {
    pub fn new(capacity: u64) -> Bloom {
        let capacity = capacity.max(1);
        let words = (capacity * BITS_PER_ITEM).div_ceil(64) as usize;
        Bloom {
            bits: (0..words).map(|_| AtomicU64::new(0)).collect(),
            capacity,
            count: AtomicU64::new(0),
            keys: (RandomState::new(), RandomState::new()),
        }
    }

    fn positions(&self, hash: &InfoHash) -> impl Iterator<Item = u64> {
        let m = self.bits.len() as u64 * 64;
        let h1 = self.keys.0.hash_one(hash.0);
        let h2 = self.keys.1.hash_one(hash.0) | 1;
        (0..HASHES).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % m)
    }

    pub fn contains(&self, hash: &InfoHash) -> bool {
        self.positions(hash)
            .all(|p| self.bits[(p / 64) as usize].load(Ordering::Relaxed) & (1 << (p % 64)) != 0)
    }

    pub fn insert(&self, hash: &InfoHash) {
        let mut new = false;
        for p in self.positions(hash) {
            let bit = 1 << (p % 64);
            let old = self.bits[(p / 64) as usize].fetch_or(bit, Ordering::Relaxed);
            new |= old & bit == 0;
        }
        if new {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(i: u64) -> InfoHash {
        let mut h = [0u8; 20];
        h[..8].copy_from_slice(&i.to_le_bytes());
        h[12..].copy_from_slice(&(i * 31).to_be_bytes());
        InfoHash(h)
    }

    #[test]
    fn no_false_negatives_and_about_one_percent_false_positives() {
        let have = Have::new(100_000);
        for i in 0..100_000 {
            have.insert(&hash(i));
        }
        assert!((0..100_000).all(|i| have.contains(&hash(i))));
        let fp = (1_000_000..1_100_000)
            .filter(|i| have.contains(&hash(*i)))
            .count();
        assert!(fp < 1_500, "false positives: {fp} per 100k");
        assert!(!have.is_full());
        assert_eq!(have.memory_bytes(), 125_000);
        // Counting is approximate (a new hash whose bits are all set
        // already isn't counted), but close.
        assert!(have.len() > 99_000);
    }

    #[test]
    fn reports_full_and_can_be_replaced() {
        let have = Have::new(10);
        for i in 0..11 {
            have.insert(&hash(i));
        }
        assert!(have.is_full());
        let bigger = Bloom::new(capacity_for(11));
        bigger.insert(&hash(3));
        have.replace(bigger);
        assert!(!have.is_full());
        assert!(have.contains(&hash(3)));
        assert_eq!(have.capacity(), MIN_CAPACITY);
    }
}
