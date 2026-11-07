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

use std::fmt;
use std::hash::Hasher;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use lodestar_cluster::have::{HaveHeader, HaveKey, chunks_for};
use lodestar_metainfo::InfoHash;
use siphasher::sip128::{Hasher128, SipHasher13};

const BITS_PER_ITEM: u64 = 10;
const HASHES: u8 = 7;
const MIN_CAPACITY: u64 = 1 << 20;

/// Told of every insert into a [`Have`] (WK-3): the coordinator keeps the
/// workers' filter from it. Called on the inserting thread, so it must
/// only record the key and return.
pub trait HaveObserver: Send + Sync + 'static {
    fn inserted(&self, hash: &InfoHash);
}

/// The shared filter; [`Have::replace`] swaps in a rebuilt one.
pub struct Have {
    bloom: RwLock<Arc<Bloom>>,
    /// Set once, when workers are allowed; unset, an insert costs one
    /// atomic load more than before.
    observer: OnceLock<Arc<dyn HaveObserver>>,
}

impl fmt::Debug for Have {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Have")
            .field("bloom", &self.current())
            .field("observed", &self.observer.get().is_some())
            .finish()
    }
}

impl Have {
    pub fn new(capacity: u64) -> Have {
        Have {
            bloom: RwLock::new(Arc::new(Bloom::new(capacity))),
            observer: OnceLock::new(),
        }
    }

    fn current(&self) -> Arc<Bloom> {
        self.bloom.read().expect("have lock").clone()
    }

    pub fn contains(&self, hash: &InfoHash) -> bool {
        self.current().contains(hash)
    }

    pub fn insert(&self, hash: &InfoHash) {
        self.current().insert(hash);
        if let Some(o) = self.observer.get() {
            o.inserted(hash);
        }
    }

    /// Tells `observer` of every insert from now on. `false` if one is
    /// already set (there is one per filter: the coordinator's).
    pub fn observe(&self, observer: Arc<dyn HaveObserver>) -> bool {
        self.observer.set(observer).is_ok()
    }

    /// Past capacity, the false-positive rate climbs; time to rebuild.
    pub fn is_full(&self) -> bool {
        self.current().is_full()
    }

    pub fn len(&self) -> u64 {
        self.current().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> u64 {
        self.current().capacity
    }

    pub fn memory_bytes(&self) -> u64 {
        self.current().memory_bytes()
    }

    pub fn replace(&self, bloom: Bloom) {
        *self.bloom.write().expect("have lock") = Arc::new(bloom);
    }
}

/// A capacity for `stored` hashes with room to grow.
pub fn capacity_for(stored: u64) -> u64 {
    (stored.saturating_mul(2)).max(MIN_CAPACITY)
}

/// A Bloom filter keyed with SipHash-1-3 under a random 128-bit key.
///
/// The key keeps peers from picking hashes that collide in it, and since
/// it travels with a copy (WK-3), a copy answers the same on any machine.
/// The positions are wire-stable: SipHash-1-3-128 of the 20 hash bytes
/// gives `h1` (low half) and `h2` (high half, made odd), and hash `i` sets
/// bit `(h1 + i·h2) mod bits`, bit `b` being bit `b % 64` of little-endian
/// word `b / 64`.
pub struct Bloom {
    bits: Vec<AtomicU64>,
    capacity: u64,
    hashes: u8,
    count: AtomicU64,
    key: HaveKey,
}

impl fmt::Debug for Bloom {
    // Neither the key nor the bits.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bloom")
            .field("bits", &self.bit_len())
            .field("hashes", &self.hashes)
            .field("capacity", &self.capacity)
            .field("count", &self.len())
            .finish_non_exhaustive()
    }
}

impl Bloom {
    /// 10 bits and 7 hashes per key (~1% false positives at capacity),
    /// under a new random key.
    pub fn new(capacity: u64) -> Bloom {
        Bloom::with_params(capacity, BITS_PER_ITEM, HASHES, new_key())
    }

    /// A filter of `bits_per_item` bits per key for `capacity` keys.
    pub fn with_params(capacity: u64, bits_per_item: u64, hashes: u8, key: HaveKey) -> Bloom {
        let capacity = capacity.max(1);
        let words = capacity
            .saturating_mul(bits_per_item.max(1))
            .div_ceil(64)
            .max(1) as usize;
        Bloom {
            bits: (0..words).map(|_| AtomicU64::new(0)).collect(),
            capacity,
            hashes: hashes.max(1),
            count: AtomicU64::new(0),
            key,
        }
    }

    /// An empty filter of a snapshot's shape, its count the header's, for
    /// the chunks to fill ([`write_bytes`](Self::write_bytes)). The header
    /// must have passed [`HaveHeader::check`], which bounds its size.
    pub fn for_header(h: &HaveHeader) -> Bloom {
        Bloom {
            bits: (0..h.bits / 64).map(|_| AtomicU64::new(0)).collect(),
            capacity: h.capacity,
            hashes: h.hashes,
            count: AtomicU64::new(h.count),
            key: h.key,
        }
    }

    /// The header a snapshot of this filter starts with.
    pub fn header(&self, generation: u64, serial: u64, next_delta: u64) -> HaveHeader {
        HaveHeader {
            generation,
            serial,
            next_delta,
            key: self.key,
            bits: self.bit_len(),
            hashes: self.hashes,
            capacity: self.capacity,
            count: self.len(),
            chunks: chunks_for(self.bit_len()),
        }
    }

    pub fn bit_len(&self) -> u64 {
        self.bits.len() as u64 * 64
    }

    pub fn memory_bytes(&self) -> u64 {
        self.bits.len() as u64 * 8
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn len(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.len() > self.capacity
    }

    /// The filter's bytes in `range` (little-endian words), as a snapshot
    /// chunk carries them. `range` is 8-byte aligned.
    pub fn read_bytes(&self, range: Range<usize>) -> Vec<u8> {
        debug_assert!(range.start.is_multiple_of(8) && range.end.is_multiple_of(8));
        self.bits[range.start / 8..range.end / 8]
            .iter()
            .flat_map(|w| w.load(Ordering::Relaxed).to_le_bytes())
            .collect()
    }

    /// ORs a chunk's bytes in at byte `offset`, so inserts that arrived
    /// while the snapshot was assembling are kept. Both are 8-byte aligned
    /// and inside the filter (an `Assembly` placed them).
    pub fn write_bytes(&self, offset: usize, bytes: &[u8]) {
        debug_assert!(offset.is_multiple_of(8) && bytes.len().is_multiple_of(8));
        let words = &self.bits[offset / 8..(offset + bytes.len()) / 8];
        for (w, b) in words.iter().zip(bytes.chunks_exact(8)) {
            w.fetch_or(
                u64::from_le_bytes(b.try_into().expect("8")),
                Ordering::Relaxed,
            );
        }
    }

    fn positions(&self, hash: &InfoHash) -> impl Iterator<Item = u64> {
        let m = self.bit_len();
        let mut s = SipHasher13::new_with_key(self.key.secret_bytes());
        s.write(&hash.0);
        let h = s.finish128();
        let (h1, h2) = (h.h1, h.h2 | 1);
        (0..u64::from(self.hashes)).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % m)
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

/// A new filter key. The system random source failing is as fatal here as
/// it was for `RandomState`, which panics the same way.
#[must_use]
pub fn new_key() -> HaveKey {
    HaveKey::random().expect("the system random source")
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
        // The count skips a hash whose bits are all set already, which a
        // filter this small hits often (the seed differs per run), so
        // insert until it passes capacity rather than exactly 11 times.
        let filled = (0..1_000).any(|i| {
            have.insert(&hash(i));
            have.is_full()
        });
        assert!(filled, "never full: {}", have.len());
        let bigger = Bloom::new(capacity_for(11));
        bigger.insert(&hash(3));
        have.replace(bigger);
        assert!(!have.is_full());
        assert!(have.contains(&hash(3)));
        assert_eq!(have.capacity(), MIN_CAPACITY);
    }

    #[test]
    fn a_copy_with_the_key_answers_the_same() {
        let a = Bloom::new(10_000);
        for i in 0..10_000 {
            a.insert(&hash(i));
        }
        let h = a.header(1, 1, 0);
        let b = Bloom::for_header(&h);
        let all = 0..h.bits as usize / 8;
        b.write_bytes(0, &a.read_bytes(all.clone()));
        assert_eq!(a.read_bytes(all.clone()), b.read_bytes(all));
        assert!((0..50_000).all(|i| a.contains(&hash(i)) == b.contains(&hash(i))));
        assert_eq!(b.len(), a.len());
    }

    #[test]
    fn debug_shows_neither_key_nor_bits() {
        let key = HaveKey::from_bytes([0x5A; 16]);
        let b = Bloom::with_params(1000, 10, 7, key);
        b.insert(&hash(1));
        let shown = format!("{b:?} {:?}", Have::new(10));
        assert!(shown.contains("Bloom"));
        for needle in ["5a", "5A", "90,", "AtomicU64", "key"] {
            assert!(!shown.contains(needle), "{needle} in {shown}");
        }
    }

    #[test]
    fn the_observer_hears_every_insert() {
        #[derive(Default)]
        struct Seen(std::sync::Mutex<Vec<InfoHash>>);
        impl HaveObserver for Seen {
            fn inserted(&self, hash: &InfoHash) {
                self.0.lock().unwrap().push(*hash);
            }
        }
        let have = Have::new(100);
        have.insert(&hash(0));
        let seen = Arc::new(Seen::default());
        assert!(have.observe(seen.clone()));
        assert!(!have.observe(seen.clone()));
        have.insert(&hash(1));
        have.insert(&hash(2));
        assert_eq!(*seen.0.lock().unwrap(), vec![hash(1), hash(2)]);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(12))]

        /// The keyed hash keeps today's rate: at capacity, 10 bits and 7
        /// hashes give ~0.8% (1% in §5), and Q6's 8 bits and 6 hashes ~2.2%.
        /// Never a false negative.
        #[test]
        fn false_positive_rate_is_unchanged(
            key in proptest::prelude::any::<[u8; 16]>(),
            base in 0u64..1 << 40,
            narrow in proptest::prelude::any::<bool>(),
        ) {
            let n = 20_000;
            let (bits, k, most) = if narrow { (8, 6, 0.030) } else { (10, 7, 0.0125) };
            let b = Bloom::with_params(n, bits, k, HaveKey::from_bytes(key));
            for i in base..base + n {
                b.insert(&hash(i));
            }
            proptest::prop_assert!((base..base + n).all(|i| b.contains(&hash(i))));
            let fp = (base + n..base + 3 * n).filter(|i| b.contains(&hash(*i))).count();
            let rate = fp as f64 / (2 * n) as f64;
            proptest::prop_assert!(rate < most, "{rate} at {bits} bits");
        }
    }
}
