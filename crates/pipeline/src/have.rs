//! The "have" filter (architecture §5): is this hash already stored?
//!
//! A Bloom filter over every stored swarm hash, about 1.25 bytes per hash
//! at a ~1% false-positive rate. It never says "no" for a stored hash, so a
//! "no" goes straight to the fetch pool. A "yes" is usually a rediscovery,
//! and the writer confirms it against the store; the rare false positive
//! is sent on to the pool from there, so nothing new is lost to it.
//!
//! Reads and inserts are lock-free (atomic words); an observer, when one
//! is set (WK-3, only once workers are allowed), takes its own lock. When the filter fills
//! past its capacity it's rebuilt at twice the size from the store.
//!
//! **Filling at start (X-healthz-cold-start).** A large store takes
//! seconds to scan, so a start no longer waits for it: the filter is made
//! [`filling`](Have::filling), a blocking thread inserts every stored hash
//! ([`fill`](Have::fill)) and then calls
//! [`mark_filled`](Have::mark_filled). Until then a "no" can't be
//! trusted, so:
//! - the intake waits for [`filled`](Have::filled) before it admits
//!   anything;
//! - [`contains`](Have::contains) answers "yes" to every hash, so any other
//!   reader treats a key as stored, which only undercounts, as a false
//!   positive already does;
//! - [`is_full`](Have::is_full) answers "no", so the writer never swaps in
//!   a rebuilt filter under a running fill (the fill writes into the one it
//!   started with).
//!
//! **Sized late (X-healthz-count-async).** The start no longer counts the
//! store, so a filter made [`unsized_filling`](Have::unsized_filling)
//! starts as a one-word placeholder. While it's unsized, every insert is
//! also kept in a pending list, under the list's lock. Once the count is
//! in, the fill hands [`size`](Have::size) an empty filter of its size,
//! which replays the pending inserts into it and swaps it in under the
//! same lock, then scans the store into it in place. So an insert made
//! during the count is in the new filter whichever side of the swap it
//! fell on. Once sized, `insert` reads one flag and never takes the lock.

use std::fmt;
use std::hash::Hasher;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

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

/// Run on the fill's thread at a fixed point, so a test can hold the fill
/// there ([`PipelineConfig::fill_gate`](crate::PipelineConfig), after the
/// scan and before the filter is marked filled; `count_gate`, before the
/// count).
#[derive(Clone)]
pub struct FillGate(Arc<dyn Fn() + Send + Sync>);

impl FillGate {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> FillGate {
        FillGate(Arc::new(f))
    }

    pub fn wait(&self) {
        (self.0)()
    }
}

impl fmt::Debug for FillGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FillGate")
    }
}

/// The shared filter; [`Have::replace`] swaps in a rebuilt one.
pub struct Have {
    bloom: RwLock<Arc<Bloom>>,
    /// Set once, when workers are allowed; unset, an insert costs one
    /// atomic load more than before.
    observer: OnceLock<Arc<dyn HaveObserver>>,
    /// Whether every stored hash is in. The flag is the hot read; the
    /// channel is for waiting on it. Set once, after the fill's last
    /// insert: the flag's `Release` (and the channel's lock) publish the
    /// fill's inserts to whoever reads it `true`.
    filled: AtomicBool,
    filled_tx: tokio::sync::watch::Sender<bool>,
    /// Whether the bloom has its final size. `false` only for an
    /// [`unsized_filling`](Have::unsized_filling) filter until
    /// [`size`](Have::size); its `Release` publishes the swap.
    sized: AtomicBool,
    /// Every hash inserted while unsized, for [`size`](Have::size) to
    /// replay; `None` once sized. Its lock orders those inserts against
    /// the swap.
    pending: Mutex<Option<Vec<InfoHash>>>,
}

impl fmt::Debug for Have {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Have")
            .field("bloom", &self.current())
            .field("observed", &self.observer.get().is_some())
            .field("filled", &self.is_filled())
            .field("sized", &self.is_sized())
            .finish()
    }
}

impl Have {
    /// A complete filter: empty, or filled before anyone reads it.
    pub fn new(capacity: u64) -> Have {
        Have::with_state(capacity, true)
    }

    /// A filter a fill is still to complete ([`mark_filled`]). Until then
    /// it reads as holding every hash and never as full.
    ///
    /// [`mark_filled`]: Have::mark_filled
    pub fn filling(capacity: u64) -> Have {
        Have::with_state(capacity, false)
    }

    /// A filling filter whose size isn't known yet: a placeholder until
    /// the fill hands [`size`](Self::size) the real one. Reads as
    /// [`filling`](Self::filling) does.
    pub fn unsized_filling() -> Have {
        let mut have = Have::with_state(1, false);
        have.sized.store(false, Ordering::Relaxed);
        *have.pending.get_mut().expect("pending lock") = Some(Vec::new());
        have
    }

    fn with_state(capacity: u64, filled: bool) -> Have {
        Have {
            bloom: RwLock::new(Arc::new(Bloom::new(capacity))),
            observer: OnceLock::new(),
            filled: AtomicBool::new(filled),
            filled_tx: tokio::sync::watch::Sender::new(filled),
            sized: AtomicBool::new(true),
            pending: Mutex::new(None),
        }
    }

    /// Whether the filter has its final size (always, unless made
    /// [`unsized_filling`](Self::unsized_filling) and not yet
    /// [`size`](Self::size)d).
    pub fn is_sized(&self) -> bool {
        self.sized.load(Ordering::Acquire)
    }

    /// Swaps in `bloom`, the fill's filter of the final size, as this one's: every hash
    /// inserted while unsized is replayed into it first, under the pending
    /// lock, so none is lost to the swap. Once only, before
    /// [`mark_filled`](Self::mark_filled); a second call is ignored.
    pub fn size(&self, bloom: Bloom) {
        let mut pending = self.pending.lock().expect("pending lock");
        let Some(inserted) = pending.take() else {
            debug_assert!(false, "a have-filter sized twice");
            return;
        };
        for h in &inserted {
            bloom.insert(h);
        }
        *self.bloom.write().expect("have lock") = Arc::new(bloom);
        // After the swap, still under the lock: an insert that reads
        // `true` finds the new filter.
        self.sized.store(true, Ordering::Release);
    }

    /// How many inserts are waiting for [`size`](Self::size). For tests.
    #[doc(hidden)]
    pub fn pending(&self) -> usize {
        self.pending
            .lock()
            .expect("pending lock")
            .as_ref()
            .map_or(0, Vec::len)
    }

    /// Whether the fill has finished: a "no" from the bloom is then true.
    pub fn is_filled(&self) -> bool {
        self.filled.load(Ordering::Acquire)
    }

    /// Returns once the fill has finished (at once for a complete filter).
    pub async fn filled(&self) {
        if self.is_filled() {
            return;
        }
        // The sender lives as long as `self`, so this only ends when the
        // value turns true.
        let mut rx = self.filled_tx.subscribe();
        let _ = rx.wait_for(|f| *f).await;
    }

    /// How many are waiting in [`filled`](Self::filled) now: the intake,
    /// while it waits for the fill. For tests.
    #[doc(hidden)]
    pub fn waiting(&self) -> usize {
        self.filled_tx.receiver_count()
    }

    /// Runs `scan` with an insert into the current filter, for the hashes
    /// the fill reads from the store: into the filter only, not to the
    /// observer. The workers' filter is built from the store itself (WK-3),
    /// as it was when the fill ran before an observer could be set. The
    /// filter is taken once: nothing replaces it while filling.
    pub fn fill<E>(
        &self,
        scan: impl FnOnce(&mut dyn FnMut(InfoHash)) -> Result<(), E>,
    ) -> Result<(), E> {
        let bloom = self.current();
        scan(&mut |h| bloom.insert(&h))
    }

    /// Ends the fill: every hash stored when it began is in, and every one
    /// stored since was inserted by the writer.
    pub fn mark_filled(&self) {
        self.filled.store(true, Ordering::Release);
        self.filled_tx.send_replace(true);
    }

    fn current(&self) -> Arc<Bloom> {
        self.bloom.read().expect("have lock").clone()
    }

    /// Whether `hash` may be stored. "Yes" to everything until the fill
    /// has finished, so a "no" is never given that the store contradicts.
    pub fn contains(&self, hash: &InfoHash) -> bool {
        !self.is_filled() || self.current().contains(hash)
    }

    pub fn insert(&self, hash: &InfoHash) {
        // The steady state: one flag read, no lock.
        if self.is_sized() {
            self.current().insert(hash);
        } else {
            self.insert_unsized(hash);
        }
        if let Some(o) = self.observer.get() {
            o.inserted(hash);
        }
    }

    /// An insert while the filter may still be unsized, under the pending
    /// lock so [`size`](Self::size) can't swap between the two steps:
    /// before the swap it's kept for the replay, after it (the list gone)
    /// it goes straight into the new filter.
    #[cold]
    fn insert_unsized(&self, hash: &InfoHash) {
        let mut pending = self.pending.lock().expect("pending lock");
        if let Some(list) = pending.as_mut() {
            list.push(*hash);
        }
        self.current().insert(hash);
    }

    /// Tells `observer` of every insert from now on. `false` if one is
    /// already set (there is one per filter: the coordinator's).
    pub fn observe(&self, observer: Arc<dyn HaveObserver>) -> bool {
        self.observer.set(observer).is_ok()
    }

    /// Past capacity, the false-positive rate climbs; time to rebuild.
    /// Never while filling: the fill writes into the current filter, so a
    /// rebuild waits until it's done.
    pub fn is_full(&self) -> bool {
        self.is_filled() && self.current().is_full()
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

    /// Swaps in a rebuilt filter. Not while filling: the fill's inserts
    /// would go into the old one ([`is_full`](Self::is_full) never asks
    /// for a rebuild then).
    pub fn replace(&self, bloom: Bloom) {
        debug_assert!(self.is_filled(), "a have-filter replaced while filling");
        debug_assert!(self.is_sized(), "a have-filter replaced unsized");
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

    /// Whether a snapshot with header `h` is of this filter: the same key
    /// and shape, so its chunks can be ORed straight into it.
    pub fn matches(&self, h: &HaveHeader) -> bool {
        self.key == h.key
            && self.bit_len() == h.bits
            && self.hashes == h.hashes
            && self.capacity == h.capacity
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

    #[test]
    fn while_filling_it_holds_everything_and_is_never_full() {
        let have = Have::filling(10);
        assert!(!have.is_filled());
        // The bloom is empty, but no "no" may be given yet.
        assert!(have.contains(&hash(1)));
        have.fill(|insert| {
            (0..1_000).for_each(|i| insert(hash(i)));
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(have.current().is_full());
        assert!(!have.is_full(), "a rebuild was asked for while filling");
        have.mark_filled();
        assert!(have.is_filled());
        assert!(have.is_full());
        assert!((0..1_000).all(|i| have.contains(&hash(i))));
        assert!(!Have::new(10).contains(&hash(1)));
    }

    #[test]
    fn fill_inserts_skip_the_observer() {
        #[derive(Default)]
        struct Count(AtomicU64);
        impl HaveObserver for Count {
            fn inserted(&self, _: &InfoHash) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let have = Have::filling(100);
        let seen = Arc::new(Count::default());
        assert!(have.observe(seen.clone()));
        have.fill(|insert| {
            insert(hash(1));
            Ok::<_, ()>(())
        })
        .unwrap();
        have.insert(&hash(2));
        assert_eq!(seen.0.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn filled_returns_only_once_marked() {
        let have = Arc::new(Have::filling(100));
        let waiter = tokio::spawn({
            let have = have.clone();
            async move { have.filled().await }
        });
        // Not finished while unmarked: give it every chance to run.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(!waiter.is_finished());
        have.mark_filled();
        tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
            .await
            .expect("filled within 10 s")
            .unwrap();
        // A complete filter is filled at once.
        Have::new(10).filled().await;
    }

    #[test]
    fn an_unsized_filter_reads_as_filling_and_keeps_its_inserts_for_the_swap() {
        let have = Have::unsized_filling();
        assert!(!have.is_sized() && !have.is_filled() && !have.is_full());
        assert!(
            have.contains(&hash(1)),
            "filling: every hash reads as stored"
        );
        have.insert(&hash(1));
        have.insert(&hash(2));
        assert_eq!(have.pending(), 2);
        // The fill's own filter, with what it scanned.
        let bloom = Bloom::new(capacity_for(10));
        bloom.insert(&hash(3));
        have.size(bloom);
        assert!(have.is_sized() && !have.is_filled());
        assert_eq!(have.pending(), 0);
        assert_eq!(have.capacity(), capacity_for(10));
        // After the swap an insert goes straight into the new filter.
        have.insert(&hash(4));
        assert_eq!(have.pending(), 0);
        have.mark_filled();
        assert!((1..=4).all(|i| have.contains(&hash(i))));
        assert!(!have.contains(&hash(1_000_000)));
    }

    /// Many threads insert while the fill swaps the filter in: every hash
    /// inserted, before, during or after the swap, is in the filter that
    /// ends up there. Several rounds, so the swap falls at different
    /// points of the inserts.
    #[test]
    fn no_insert_is_lost_to_the_swap_under_concurrent_inserts() {
        const THREADS: u64 = 8;
        const EACH: u64 = 50_000;
        let mut mid_insert = 0;
        for round in 0..6u64 {
            let have = Arc::new(Have::unsized_filling());
            let started = Arc::new(AtomicU64::new(0));
            let finished = Arc::new(AtomicU64::new(0));
            let (done_tx, done) = std::sync::mpsc::channel();
            for t in 0..THREADS {
                let (have, started, finished, done_tx) = (
                    have.clone(),
                    started.clone(),
                    finished.clone(),
                    done_tx.clone(),
                );
                std::thread::spawn(move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    for i in 0..EACH {
                        have.insert(&hash((round * THREADS + t) * EACH + i));
                    }
                    finished.fetch_add(1, Ordering::SeqCst);
                    done_tx.send(()).unwrap();
                });
            }
            drop(done_tx);
            // Swap once the inserters are running and a share of the
            // inserts is in, a share that moves with the round.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let share = THREADS * EACH * (round + 1) / 10;
            while started.load(Ordering::SeqCst) < THREADS || have.pending() < share as usize {
                assert!(
                    std::time::Instant::now() < deadline,
                    "inserters started and {share} inserts in"
                );
                std::thread::yield_now();
            }
            let bloom = Bloom::new(capacity_for(THREADS * EACH));
            bloom.insert(&hash(1 << 50));
            have.size(bloom);
            // Read after the swap: inserters still running then raced it.
            if finished.load(Ordering::SeqCst) < THREADS {
                mid_insert += 1;
            }
            for _ in 0..THREADS {
                done.recv_timeout(std::time::Duration::from_secs(10))
                    .expect("each inserter done within 10 s");
            }
            have.mark_filled();
            assert!(have.is_sized());
            assert_eq!(have.pending(), 0);
            assert_eq!(have.capacity(), capacity_for(THREADS * EACH));
            assert!(have.contains(&hash(1 << 50)), "the fill's own hash");
            for t in 0..THREADS {
                for i in 0..EACH {
                    assert!(
                        have.contains(&hash((round * THREADS + t) * EACH + i)),
                        "round {round}: thread {t}'s insert {i} lost to the swap"
                    );
                }
            }
        }
        // The swap fell while inserts were still running in some round,
        // or this proved nothing about racing them.
        assert!(mid_insert > 0, "no round swapped with inserters running");
    }

    /// Once sized, `insert` never takes the pending lock: one held here
    /// doesn't stop it.
    #[test]
    fn a_sized_filter_inserts_without_the_pending_lock() {
        for have in [Have::new(100), {
            let h = Have::unsized_filling();
            h.size(Bloom::new(100));
            h
        }] {
            let have = Arc::new(have);
            let _held = have.pending.lock().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn({
                let have = have.clone();
                move || {
                    have.insert(&hash(7));
                    tx.send(()).unwrap();
                }
            });
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .expect("the insert didn't wait for the pending lock");
            assert!(have.current().contains(&hash(7)));
        }
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
