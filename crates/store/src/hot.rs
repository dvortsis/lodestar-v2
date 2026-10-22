//! Hot columns (architecture §7): swarm stats, last-seen and flags per
//! torrent id, in memory, read and written without locks.
//!
//! Swarm stats change constantly; keeping them in the search index would
//! mean re-indexing whole documents. Instead the scraper writes these
//! arrays and search collectors read them for filters, sorts and ranking.
//! SQLite stays authoritative: the arrays are rebuilt from it whenever the
//! snapshot taken at a clean shutdown is missing or stale.
//!
//! They're plain atomics rather than mmap'd files (§16, M4 plan): the
//! workspace forbids `unsafe`, and mapping a file needs it.
//!
//! Layout: ids are split into chunks of 64k slots, allocated on first
//! write and never freed, so a reader never waits and memory follows the
//! highest id actually written. Within a chunk each column is contiguous,
//! so a scan of one column streams. Values are relaxed atomics: one
//! torrent's columns are written one after another, so a reader racing the
//! scraper may briefly see a new seeders count beside an old leechers
//! count, which is harmless for display and ranking.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// "Never scraped" (swarm columns) or "unknown" (last seen). The UI shows
/// "—", and no range filter matches it.
pub const NEVER: u32 = u32::MAX;

const CHUNK_BITS: u32 = 16;
const CHUNK: usize = 1 << CHUNK_BITS;
/// Ids up to 2³⁰ (about a billion) have slots; the directory costs
/// 16k pointers.
const MAX_CHUNKS: usize = 1 << 14;
/// Ids from this one on are ignored by writes and read as [`NEVER`].
pub const MAX_ID: u64 = (CHUNK * MAX_CHUNKS) as u64;

/// How often, at most, the [epoch](HotColumns::epoch) advances while
/// values keep changing: search caches that read hot columns are keyed by
/// it, so they stay valid for up to this long.
pub const EPOCH_EVERY: Duration = Duration::from_secs(60);

/// One hot column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Col {
    /// Seeders: the most any reporting tracker gave, or the DHT estimate
    /// when no tracker reports the swarm.
    Seeders,
    Leechers,
    /// Trackers reporting the hash (s+l+c > 0).
    Trackers,
    /// The DHT swarm estimate (BEP 33), seeds plus peers.
    Dht,
    /// Last DHT activity, unix seconds.
    LastSeen,
    /// Bits from [`flags`]; [`NEVER`] reads as none set.
    Flags,
}

/// Bits of the [`Col::Flags`] column. Values are persisted in snapshots:
/// append only. User flags (§7: favourite, hidden, …) take bits from 8 up.
pub mod flags {
    /// The swarm-consistency check flagged fake seeding (§8).
    pub const SUSPECT: u32 = 1 << 0;
}

impl Col {
    pub const ALL: [Col; 6] = [
        Col::Seeders,
        Col::Leechers,
        Col::Trackers,
        Col::Dht,
        Col::LastSeen,
        Col::Flags,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The snapshot file's name.
    pub fn file_name(self) -> &'static str {
        match self {
            Col::Seeders => "seeders.col",
            Col::Leechers => "leechers.col",
            Col::Trackers => "trackers.col",
            Col::Dht => "dht.col",
            Col::LastSeen => "last_seen.col",
            Col::Flags => "flags.col",
        }
    }
}

const COLS: usize = Col::ALL.len();

/// A torrent's swarm columns, written together after a scrape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwarmHot {
    pub seeders: u32,
    pub leechers: u32,
    pub trackers: u32,
    pub dht: u32,
}

/// The columns. Share one behind an `Arc`.
pub struct HotColumns {
    /// `chunks[id >> CHUNK_BITS]`: `COLS × CHUNK` values, column-major.
    chunks: Box<[OnceLock<Box<[AtomicU32]>>]>,
    /// One past the highest id written.
    len: AtomicU64,
    epoch: AtomicU64,
    dirty: AtomicBool,
    /// Milliseconds after `born` of the last epoch change.
    bumped_at: AtomicU64,
    born: Instant,
    epoch_every: Duration,
}

impl std::fmt::Debug for HotColumns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotColumns")
            .field("len", &self.len())
            .field("epoch", &self.epoch.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Default for HotColumns {
    fn default() -> Self {
        HotColumns::new()
    }
}

impl HotColumns {
    pub fn new() -> HotColumns {
        HotColumns::with_epoch_every(EPOCH_EVERY)
    }

    /// For tests: a shorter (or zero) epoch period.
    pub fn with_epoch_every(epoch_every: Duration) -> HotColumns {
        HotColumns {
            chunks: (0..MAX_CHUNKS).map(|_| OnceLock::new()).collect(),
            len: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            bumped_at: AtomicU64::new(0),
            born: Instant::now(),
            epoch_every,
        }
    }

    fn slot(&self, col: Col, id: u64) -> Option<&AtomicU32> {
        if id >= MAX_ID {
            return None;
        }
        let chunk = self.chunks[(id >> CHUNK_BITS) as usize].get()?;
        Some(&chunk[col.index() * CHUNK + (id as usize & (CHUNK - 1))])
    }

    fn slot_mut(&self, col: Col, id: u64) -> Option<&AtomicU32> {
        if id >= MAX_ID {
            return None;
        }
        let chunk = self.chunks[(id >> CHUNK_BITS) as usize]
            .get_or_init(|| (0..COLS * CHUNK).map(|_| AtomicU32::new(NEVER)).collect());
        self.len.fetch_max(id + 1, Ordering::Relaxed);
        Some(&chunk[col.index() * CHUNK + (id as usize & (CHUNK - 1))])
    }

    /// The value, or [`NEVER`].
    #[inline]
    pub fn get(&self, col: Col, id: u64) -> u32 {
        self.slot(col, id)
            .map_or(NEVER, |v| v.load(Ordering::Relaxed))
    }

    /// Sets one value without advancing the epoch: for bulk loads, which
    /// call [`HotColumns::changed`] once at the end.
    pub fn set(&self, col: Col, id: u64, value: u32) {
        if let Some(v) = self.slot_mut(col, id) {
            v.store(value, Ordering::Relaxed);
        }
    }

    /// Writes a torrent's swarm columns. Values are capped one below
    /// [`NEVER`], so a real count is never mistaken for "never scraped".
    pub fn set_swarm(&self, id: u64, s: SwarmHot) {
        let cap = |v: u32| v.min(NEVER - 1);
        self.set(Col::Seeders, id, cap(s.seeders));
        self.set(Col::Leechers, id, cap(s.leechers));
        self.set(Col::Trackers, id, cap(s.trackers));
        self.set(Col::Dht, id, cap(s.dht));
        self.changed();
    }

    /// The torrent's [`flags`], none when unknown.
    pub fn flags(&self, id: u64) -> u32 {
        match self.get(Col::Flags, id) {
            NEVER => 0,
            f => f,
        }
    }

    /// Sets or clears flag bits, writing only when they change.
    pub fn set_flags(&self, id: u64, bits: u32, on: bool) {
        let now = self.flags(id);
        let next = if on { now | bits } else { now & !bits };
        if next != now {
            self.set(Col::Flags, id, next);
            self.changed();
        }
    }

    /// Records DHT activity at `at` (unix seconds); last-seen only moves
    /// forward.
    pub fn touch(&self, id: u64, at: u32) {
        let Some(v) = self.slot_mut(Col::LastSeen, id) else {
            return;
        };
        let at = at.min(NEVER - 1);
        // A compare-exchange loop: `fetch_update` is deprecated and its
        // replacement is newer than the MSRV.
        let mut cur = v.load(Ordering::Relaxed);
        while cur == NEVER || at > cur {
            match v.compare_exchange_weak(cur, at, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    self.changed();
                    return;
                }
                Err(now) => cur = now,
            }
        }
    }

    /// Marks the columns changed, so the epoch advances when its period
    /// is up.
    pub fn changed(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// A number that changes at most once per period while values change,
    /// and never otherwise. Results computed from the columns stay valid
    /// for as long as it's the same.
    pub fn epoch(&self) -> u64 {
        if self.dirty.load(Ordering::Relaxed) {
            let now = self.born.elapsed().as_millis() as u64;
            let last = self.bumped_at.load(Ordering::Relaxed);
            let due = now.saturating_sub(last) >= self.epoch_every.as_millis() as u64;
            // One caller wins the bump; the others see the new epoch.
            if due
                && self
                    .bumped_at
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                self.dirty.store(false, Ordering::Relaxed);
                self.epoch.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.epoch.load(Ordering::Relaxed)
    }

    /// One past the highest id written.
    pub fn len(&self) -> u64 {
        self.len.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes held by allocated chunks.
    pub fn memory_bytes(&self) -> usize {
        self.chunks.iter().filter(|c| c.get().is_some()).count() * COLS * CHUNK * 4
    }

    /// Calls `f(id, value)` for every id below [`HotColumns::len`] whose
    /// value isn't [`NEVER`], in id order.
    pub fn for_each(&self, col: Col, mut f: impl FnMut(u64, u32)) {
        let len = self.len();
        for (c, chunk) in self.chunks.iter().enumerate() {
            let base = (c as u64) << CHUNK_BITS;
            if base >= len {
                break;
            }
            let Some(chunk) = chunk.get() else {
                continue;
            };
            let column = &chunk[col.index() * CHUNK..(col.index() + 1) * CHUNK];
            for (i, v) in column.iter().enumerate() {
                let v = v.load(Ordering::Relaxed);
                if v != NEVER {
                    f(base + i as u64, v);
                }
            }
        }
    }

    /// Writes one snapshot file per column into `dir`, each tagged with
    /// `stamp`. Files are written beside their final names and renamed, so
    /// a crash mid-save leaves the old snapshot or none, never half of one.
    /// The caller records `stamp` (in SQLite) only after this returns.
    pub fn save(&self, dir: &Path, stamp: u64) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let len = self.len();
        for col in Col::ALL {
            let path = dir.join(col.file_name());
            let tmp = tmp_path(&path);
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
            let mut sum = Fnv::new();
            w.write_all(&header(col, stamp, len))?;
            for id in 0..len {
                let b = self.get(col, id).to_le_bytes();
                sum.update(&b);
                w.write_all(&b)?;
            }
            w.write_all(&sum.0.to_le_bytes())?;
            let file = w.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
        }
        Ok(())
    }

    /// Loads a snapshot written by [`HotColumns::save`] with this `stamp`.
    /// `None` when any column is missing, from another stamp, truncated or
    /// corrupt: the caller then rebuilds from SQLite.
    pub fn load(dir: &Path, stamp: u64) -> io::Result<Option<HotColumns>> {
        let hot = HotColumns::new();
        for col in Col::ALL {
            let file = match File::open(dir.join(col.file_name())) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            let size = file.metadata()?.len();
            let mut r = BufReader::with_capacity(1 << 20, file);
            let mut head = [0u8; HEADER];
            if r.read_exact(&mut head).is_err() {
                return Ok(None);
            }
            let len = u64::from_le_bytes(head[24..32].try_into().expect("8 bytes"));
            if head != header(col, stamp, len)
                || len > MAX_ID
                || size != HEADER as u64 + len * 4 + 8
            {
                return Ok(None);
            }
            let mut sum = Fnv::new();
            let mut b = [0u8; 4];
            for id in 0..len {
                r.read_exact(&mut b)?;
                sum.update(&b);
                let v = u32::from_le_bytes(b);
                if v != NEVER {
                    hot.set(col, id, v);
                }
            }
            let mut tail = [0u8; 8];
            r.read_exact(&mut tail)?;
            if u64::from_le_bytes(tail) != sum.0 {
                return Ok(None);
            }
            hot.len.fetch_max(len, Ordering::Relaxed);
        }
        Ok(Some(hot))
    }
}

const MAGIC: &[u8; 8] = b"LSHOTCOL";
const FORMAT: u32 = 1;
/// Magic, format, column, stamp, length.
const HEADER: usize = 8 + 4 + 4 + 8 + 8;

fn header(col: Col, stamp: u64, len: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[12..16].copy_from_slice(&(col.index() as u32).to_le_bytes());
    h[16..24].copy_from_slice(&stamp.to_le_bytes());
    h[24..32].copy_from_slice(&len.to_le_bytes());
    h
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".tmp");
    PathBuf::from(p)
}

/// FNV-1a, 64-bit: catches torn and corrupted snapshots; not a defence
/// against tampering (the data directory is trusted).
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn swarm(seeders: u32) -> SwarmHot {
        SwarmHot {
            seeders,
            leechers: seeders / 2,
            trackers: 3,
            dht: 40,
        }
    }

    #[test]
    fn unset_reads_never_and_writes_grow() {
        let hot = HotColumns::new();
        assert_eq!(hot.get(Col::Seeders, 5), NEVER);
        assert!(hot.is_empty());
        assert_eq!(hot.memory_bytes(), 0);
        hot.set_swarm(5, swarm(10));
        hot.set_swarm(200_000, swarm(0));
        assert_eq!(hot.get(Col::Seeders, 5), 10);
        assert_eq!(hot.get(Col::Leechers, 5), 5);
        assert_eq!(hot.get(Col::Seeders, 200_000), 0, "a real zero");
        assert_eq!(hot.get(Col::Seeders, 6), NEVER);
        assert_eq!(hot.get(Col::LastSeen, 5), NEVER);
        assert_eq!(hot.len(), 200_001);
        assert_eq!(hot.memory_bytes(), 2 * COLS * CHUNK * 4);
        // Past the last chunk: ignored.
        hot.set_swarm(MAX_ID, swarm(1));
        assert_eq!(hot.get(Col::Seeders, MAX_ID), NEVER);
        assert_eq!(hot.len(), 200_001);
    }

    #[test]
    fn counts_never_collide_with_the_sentinel() {
        let hot = HotColumns::new();
        hot.set_swarm(1, swarm(NEVER));
        assert_eq!(hot.get(Col::Seeders, 1), NEVER - 1);
    }

    #[test]
    fn last_seen_only_moves_forward() {
        let hot = HotColumns::with_epoch_every(Duration::ZERO);
        hot.touch(9, 1_000);
        hot.touch(9, 900);
        assert_eq!(hot.get(Col::LastSeen, 9), 1_000);
        hot.touch(9, 2_000);
        assert_eq!(hot.get(Col::LastSeen, 9), 2_000);
    }

    #[test]
    fn epoch_moves_only_after_changes_and_at_most_once_per_period() {
        let hot = HotColumns::with_epoch_every(Duration::ZERO);
        let e0 = hot.epoch();
        assert_eq!(hot.epoch(), e0, "no change, no new epoch");
        hot.set_swarm(1, swarm(1));
        let e1 = hot.epoch();
        assert_eq!(e1, e0 + 1);
        assert_eq!(hot.epoch(), e1);
        hot.touch(1, 5);
        assert_eq!(hot.epoch(), e1 + 1);
        // A stale touch changes nothing.
        hot.touch(1, 4);
        assert_eq!(hot.epoch(), e1 + 1);

        let slow = HotColumns::with_epoch_every(Duration::from_secs(3600));
        slow.set_swarm(1, swarm(1));
        assert_eq!(slow.epoch(), 0, "period not up yet");
    }

    #[test]
    fn flags_default_to_none_and_toggle() {
        let hot = HotColumns::with_epoch_every(Duration::ZERO);
        assert_eq!(hot.flags(3), 0);
        hot.set_flags(3, flags::SUSPECT, true);
        assert_eq!(hot.flags(3), flags::SUSPECT);
        let e = hot.epoch();
        hot.set_flags(3, flags::SUSPECT, true);
        assert_eq!(hot.epoch(), e, "no change, no new epoch");
        hot.set_flags(3, flags::SUSPECT, false);
        assert_eq!(hot.flags(3), 0);
    }

    #[test]
    fn for_each_visits_set_values_in_order() {
        let hot = HotColumns::new();
        for id in [70_000u64, 3, 65_535] {
            hot.set_swarm(id, swarm(id as u32));
        }
        let mut seen = Vec::new();
        hot.for_each(Col::Seeders, |id, v| seen.push((id, v)));
        assert_eq!(seen, [(3, 3), (65_535, 65_535), (70_000, 70_000)]);
    }

    #[test]
    fn concurrent_writers_and_readers() {
        let hot = Arc::new(HotColumns::new());
        let writers: Vec<_> = (0..4u64)
            .map(|t| {
                let hot = hot.clone();
                std::thread::spawn(move || {
                    for i in 0..50_000u64 {
                        let id = i * 4 + t;
                        hot.set_swarm(id, swarm(id as u32 % 1000));
                        hot.touch(id, id as u32);
                    }
                })
            })
            .collect();
        let reader = {
            let hot = hot.clone();
            std::thread::spawn(move || {
                let mut seen = 0u64;
                for _ in 0..20 {
                    for id in 0..200_000u64 {
                        let v = hot.get(Col::Seeders, id);
                        assert!(v == NEVER || v == id as u32 % 1000, "id {id}: {v}");
                        seen += u64::from(v != NEVER);
                    }
                }
                seen
            })
        };
        for w in writers {
            w.join().unwrap();
        }
        reader.join().unwrap();
        for id in 0..200_000u64 {
            assert_eq!(hot.get(Col::Seeders, id), id as u32 % 1000);
            assert_eq!(hot.get(Col::LastSeen, id), id as u32);
        }
        assert_eq!(hot.len(), 200_000);
    }

    #[test]
    fn snapshot_round_trip_and_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let hot = HotColumns::new();
        hot.set_swarm(2, swarm(7));
        hot.set_swarm(140_000, swarm(0));
        hot.touch(2, 1_700_000_000);
        hot.save(dir.path(), 42).unwrap();

        let back = HotColumns::load(dir.path(), 42).unwrap().unwrap();
        for col in Col::ALL {
            for id in [0, 2, 3, 140_000, 140_001] {
                assert_eq!(back.get(col, id), hot.get(col, id), "{col:?} {id}");
            }
        }
        assert_eq!(back.len(), hot.len());

        // Another stamp: stale.
        assert!(HotColumns::load(dir.path(), 43).unwrap().is_none());
        // A missing column.
        let empty = tempfile::tempdir().unwrap();
        assert!(HotColumns::load(empty.path(), 42).unwrap().is_none());
        // A flipped bit.
        let path = dir.path().join(Col::Leechers.file_name());
        let mut bytes = fs::read(&path).unwrap();
        bytes[HEADER + 9] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(HotColumns::load(dir.path(), 42).unwrap().is_none());
        // Truncated.
        bytes.truncate(bytes.len() - 3);
        fs::write(&path, &bytes).unwrap();
        assert!(HotColumns::load(dir.path(), 42).unwrap().is_none());
    }
}
