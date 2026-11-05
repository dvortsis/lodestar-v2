//! SP-3's rate state (§20.6 SP-3, plan 2026-11-04 Q10): how many
//! near-duplicate names one publisher, or one non-DHT source without a
//! publisher, has emitted within the last hour.
//!
//! - **Near-duplicate** is a name [`signature`]: the release parser's
//!   title, which already leaves out the episode, quality and group
//!   tokens, read through the filter's normalization (look-alike letters,
//!   invisible characters, case) and hashed. A name with no title has no
//!   signature and never counts.
//! - **The window is one hour,** in [`BUCKETS`] buckets of
//!   [`BUCKET_SECS`]: a count holds the names of the last 55 to 60
//!   minutes, never one older than an hour.
//! - **Bounded.** At most [`MAX_ORIGINS`] publishers and sources are
//!   tracked; when a new one arrives the one seen longest ago is evicted.
//!   Each keeps its [`SIGNATURES`] busiest signatures: a new signature
//!   takes the slot with the fewest names in the window (the one seen
//!   longest ago among them), so a flood's count grows while a
//!   publisher's many different titles take turns in the other slots.
//!   Eviction only ever forgets names, so a count is never more than the
//!   names really seen. The known limit: a flood that sends
//!   [`SIGNATURES`] or more different titles between each of its own
//!   names is forgotten each time, at the cost of sending that many
//!   times more.
//! - **No text is kept.** A publisher is keyed with the source that
//!   named it, so one source can't add to another's publisher, and the
//!   pair is hashed with keys drawn at process start ([`RandomState`]), so
//!   no string can be chosen to collide with another's. At most
//!   [`MAX_PUBLISHER_BYTES`] of it are hashed, and nothing the map holds
//!   grows with the length of what a source sends.
//! - **Memory only,** rebuilt from nothing at start: a restart forgets at
//!   most one window (plan §3).
//!
//! The writer is its only user: SP-3 depends on the order torrents are
//! committed in, and prepare runs in parallel (§19.4).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};

use crate::hash::{fmix64, fnv_byte, fnv_start};

/// The window, in seconds.
pub const WINDOW_SECS: i64 = 3600;
/// Buckets the window is counted in.
pub const BUCKETS: usize = 12;
/// One bucket's span.
pub const BUCKET_SECS: i64 = WINDOW_SECS / BUCKETS as i64;
/// Near-duplicate names in the window from which SP-3 flags (Q10).
pub const FLAG_AT: u32 = 20;
/// Near-duplicate names in the window from which SP-3 demotes (Q10).
pub const DEMOTE_AT: u32 = 100;
/// Publishers and sources tracked at once (Q10).
pub const MAX_ORIGINS: usize = 10_000;
/// Signatures kept per publisher or source.
pub const SIGNATURES: usize = 16;
/// Bytes of a publisher hashed: a longer one is the same publisher as
/// its first [`MAX_PUBLISHER_BYTES`]. Claims are clamped before this
/// (SRC-2); this keeps the map's cost fixed whatever a caller passes.
pub const MAX_PUBLISHER_BYTES: usize = 256;
/// Title tokens a signature reads: a title longer than this is the same
/// near-duplicate as its first [`MAX_TOKENS`].
pub const MAX_TOKENS: usize = 32;

/// How far a publisher's near-duplicates have gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// At least [`FLAG_AT`] in the window.
    Flag,
    /// At least [`DEMOTE_AT`] in the window.
    Demote,
}

impl Level {
    /// The level a count has reached, if any.
    pub fn of(count: u32) -> Option<Level> {
        if count >= DEMOTE_AT {
            Some(Level::Demote)
        } else if count >= FLAG_AT {
            Some(Level::Flag)
        } else {
            None
        }
    }
}

/// Who a name is counted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin<'a> {
    /// The publisher a source's claims name (SRC-2), as stated, with the
    /// source that named it: the same name from two sources is two
    /// publishers, so one source can't add to another's count (runner,
    /// #495).
    Publisher(u8, &'a str),
    /// A source without a publisher, by its id. The DHT is never one: it
    /// isn't a publisher (Q10), so the writer leaves its finds out.
    Source(u8),
}

/// A name's near-duplicate signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(u64);

/// The signature of a release title (the parser's `title`, without
/// episode, quality or group tokens): its normalized tokens in order.
/// `None` when it has none.
pub fn signature(title: &str) -> Option<Signature> {
    let mut tokens = Vec::new();
    crate::normalize::tokens(title, &mut tokens);
    let mut h = fnv_start();
    let mut last = None;
    let mut n = 0;
    for t in &tokens {
        // One word can give several tokens at one position (its readings);
        // they all count, so the signature reads them all.
        if last != Some(t.pos) {
            n += 1;
            if n > MAX_TOKENS {
                break;
            }
            last = Some(t.pos);
            h = fnv_byte(h, 0xff);
        }
        for b in t.hash.to_le_bytes() {
            h = fnv_byte(h, b);
        }
    }
    (n > 0).then(|| Signature(fmix64(h)))
}

/// One signature's names in the window.
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    sig: u64,
    /// Bucket number (`now / BUCKET_SECS`) of the newest bucket.
    newest: i64,
    /// By bucket number modulo [`BUCKETS`].
    buckets: [u16; BUCKETS],
    /// Its origin's [`Slot::tick`] when it last counted a name.
    last: u64,
}

impl Counts {
    /// Moves the window on to bucket `at`, forgetting what's left it. A
    /// clock that went back within the window counts in the newest
    /// bucket; one that went back further starts the window again at
    /// `at`, so a clock set back days can't hold old names in it.
    fn advance(&mut self, at: i64) {
        if at < self.newest - BUCKETS as i64 {
            self.buckets = [0; BUCKETS];
            self.newest = at;
            return;
        }
        if at <= self.newest {
            return;
        }
        let gap = at - self.newest;
        if gap >= BUCKETS as i64 {
            self.buckets = [0; BUCKETS];
        } else {
            for b in self.newest + 1..=at {
                self.buckets[b.rem_euclid(BUCKETS as i64) as usize] = 0;
            }
        }
        self.newest = at;
    }

    fn total(&self) -> u32 {
        self.buckets.iter().map(|&c| u32::from(c)).sum()
    }

    /// The window's count as of bucket `at`, without changing anything.
    fn total_at(&self, at: i64) -> u32 {
        let mut c = *self;
        c.advance(at);
        c.total()
    }

    fn add(&mut self, at: i64) -> u32 {
        self.advance(at);
        let b = &mut self.buckets[self.newest.rem_euclid(BUCKETS as i64) as usize];
        *b = b.saturating_add(1);
        self.total()
    }
}

const NONE: u32 = u32::MAX;

/// One publisher or source.
#[derive(Debug, Clone)]
struct Slot {
    key: u64,
    /// Toward the slot seen longer ago, and toward the one seen more
    /// recently ([`NONE`] at either end).
    older: u32,
    newer: u32,
    sigs: [Counts; SIGNATURES],
    used: u8,
    /// Names this origin has counted: orders its signatures by when each
    /// was last seen.
    tick: u64,
}

/// The rate map. [`Flood::record`] counts a name and says how far its
/// publisher's near-duplicates have gone.
#[derive(Debug)]
pub struct Flood {
    keys: RandomState,
    cap: usize,
    index: HashMap<u64, u32>,
    slots: Vec<Slot>,
    /// The slot seen longest ago, and the one seen last.
    oldest: u32,
    newest: u32,
}

impl Default for Flood {
    fn default() -> Flood {
        Flood::with_cap(MAX_ORIGINS)
    }
}

impl Flood {
    /// A map tracking at most `cap` publishers and sources (at least one).
    pub fn with_cap(cap: usize) -> Flood {
        Flood {
            keys: RandomState::new(),
            cap: cap.max(1),
            index: HashMap::new(),
            slots: Vec::new(),
            oldest: NONE,
            newest: NONE,
        }
    }

    /// Publishers and sources tracked now.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The most this map tracks.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// `origin`'s key in this map: a publisher hashed under this map's
    /// keys. [`Flood::record_key`] takes it, so a caller can keep it in
    /// place of the text.
    pub fn key(&self, origin: Origin<'_>) -> u64 {
        let mut h = self.keys.build_hasher();
        match origin {
            Origin::Publisher(source, p) => {
                h.write_u8(0);
                h.write_u8(source);
                let b = p.as_bytes();
                h.write(&b[..b.len().min(MAX_PUBLISHER_BYTES)]);
            }
            Origin::Source(s) => {
                h.write_u8(1);
                h.write_u8(s);
            }
        }
        h.finish()
    }

    /// Counts one name with signature `sig` from `origin` at `now` (Unix
    /// seconds) and returns the level the near-duplicates it belongs to
    /// have reached, this one included.
    pub fn record(&mut self, origin: Origin<'_>, sig: Signature, now: i64) -> Option<Level> {
        self.record_key(self.key(origin), sig, now)
    }

    /// [`Flood::record`] by the origin's [`Flood::key`].
    pub fn record_key(&mut self, key: u64, sig: Signature, now: i64) -> Option<Level> {
        let at = now.div_euclid(BUCKET_SECS);
        let i = match self.index.get(&key) {
            Some(&i) => {
                self.touch(i);
                i
            }
            None => self.insert(key),
        };
        let slot = &mut self.slots[i as usize];
        slot.tick += 1;
        let tick = slot.tick;
        let used = usize::from(slot.used);
        let j = match slot.sigs[..used].iter().position(|c| c.sig == sig.0) {
            Some(j) => j,
            None => {
                let j = if used < SIGNATURES {
                    slot.used += 1;
                    used
                } else {
                    // The slot with the fewest names in the window; the
                    // one seen longest ago among them.
                    (0..SIGNATURES)
                        .min_by_key(|&j| (slot.sigs[j].total_at(at), slot.sigs[j].last))
                        .expect("SIGNATURES > 0")
                };
                slot.sigs[j] = Counts {
                    sig: sig.0,
                    newest: at,
                    ..Counts::default()
                };
                j
            }
        };
        let c = &mut slot.sigs[j];
        c.last = tick;
        Level::of(c.add(at))
    }

    /// What `origin`'s names with `sig` count now, without counting one.
    pub fn count(&self, origin: Origin<'_>, sig: Signature, now: i64) -> u32 {
        let at = now.div_euclid(BUCKET_SECS);
        self.index.get(&self.key(origin)).map_or(0, |&i| {
            let s = &self.slots[i as usize];
            s.sigs[..usize::from(s.used)]
                .iter()
                .find(|c| c.sig == sig.0)
                .map_or(0, |c| c.total_at(at))
        })
    }

    /// A new slot for `key`, the newest, evicting the oldest when full.
    fn insert(&mut self, key: u64) -> u32 {
        let fresh = Slot {
            key,
            older: NONE,
            newer: NONE,
            sigs: [Counts::default(); SIGNATURES],
            used: 0,
            tick: 0,
        };
        let i = if self.slots.len() < self.cap {
            // Grown by doubling, but never past the cap.
            if self.slots.len() == self.slots.capacity() {
                let more = self.slots.len().max(64).min(self.cap - self.slots.len());
                self.slots.reserve_exact(more);
            }
            self.slots.push(fresh);
            (self.slots.len() - 1) as u32
        } else {
            let i = self.oldest;
            self.unlink(i);
            let old = std::mem::replace(&mut self.slots[i as usize], fresh);
            self.index.remove(&old.key);
            i
        };
        self.index.insert(key, i);
        self.link_newest(i);
        i
    }

    fn touch(&mut self, i: u32) {
        if self.newest != i {
            self.unlink(i);
            self.link_newest(i);
        }
    }

    fn unlink(&mut self, i: u32) {
        let (older, newer) = {
            let s = &self.slots[i as usize];
            (s.older, s.newer)
        };
        match older {
            NONE => self.oldest = newer,
            o => self.slots[o as usize].newer = newer,
        }
        match newer {
            NONE => self.newest = older,
            n => self.slots[n as usize].older = older,
        }
        let s = &mut self.slots[i as usize];
        s.older = NONE;
        s.newer = NONE;
    }

    fn link_newest(&mut self, i: u32) {
        let prev = self.newest;
        {
            let s = &mut self.slots[i as usize];
            s.older = prev;
            s.newer = NONE;
        }
        match prev {
            NONE => self.oldest = i,
            p => self.slots[p as usize].newer = i,
        }
        self.newest = i;
    }

    /// Checks the links and the index agree (tests).
    #[doc(hidden)]
    pub fn check(&self) -> Result<(), String> {
        if self.slots.len() > self.cap || self.index.len() != self.slots.len() {
            return Err(format!(
                "{} slots, {} indexed, cap {}",
                self.slots.len(),
                self.index.len(),
                self.cap
            ));
        }
        let mut seen = 0;
        let mut at = self.oldest;
        let mut prev = NONE;
        while at != NONE {
            let s = &self.slots[at as usize];
            if s.older != prev || self.index.get(&s.key) != Some(&at) {
                return Err(format!("slot {at} is linked wrong"));
            }
            seen += 1;
            if seen > self.slots.len() {
                return Err("the list loops".into());
            }
            prev = at;
            at = s.newer;
        }
        if seen != self.slots.len() || prev != self.newest {
            return Err(format!("{seen} of {} slots linked", self.slots.len()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(t: &str) -> Signature {
        signature(t).unwrap()
    }

    #[test]
    fn signatures_read_normalized_titles() {
        assert_eq!(sig("Free Movie"), sig("FREE  movie"));
        assert_eq!(sig("Free Movie"), sig("Ｆｒｅｅ Movie"));
        assert_ne!(sig("Free Movie"), sig("Free Movies Now"));
        assert_ne!(sig("ab c"), sig("a bc"));
        assert_eq!(signature(""), None);
        assert_eq!(signature(" - . "), None);
        let long: String = (0..200).map(|i| format!("w{i} ")).collect();
        let longer = format!("{long} more");
        assert_eq!(signature(&long), signature(&longer), "capped at MAX_TOKENS");
    }

    #[test]
    fn counts_flag_then_demote_within_the_hour() {
        let mut f = Flood::default();
        let p = Origin::Publisher(1, "uploader");
        let s = sig("Some Title");
        for n in 1..FLAG_AT {
            assert_eq!(f.record(p, s, 1000 + i64::from(n)), None, "{n}");
        }
        assert_eq!(f.record(p, s, 1100), Some(Level::Flag));
        for _ in FLAG_AT + 1..DEMOTE_AT {
            assert_eq!(f.record(p, s, 1200), Some(Level::Flag));
        }
        assert_eq!(f.record(p, s, 1300), Some(Level::Demote));
        // Another publisher, another source, another title: their own.
        assert_eq!(f.record(Origin::Publisher(1, "other"), s, 1300), None);
        assert_eq!(f.record(Origin::Source(3), s, 1300), None);
        assert_eq!(f.record(p, sig("Another Title"), 1300), None);
        // An hour on, the window is empty again.
        assert_eq!(f.count(p, s, 1300 + WINDOW_SECS), 0);
        assert_eq!(f.record(p, s, 1300 + WINDOW_SECS), None);
    }

    #[test]
    fn the_window_slides_by_buckets() {
        let mut f = Flood::default();
        let p = Origin::Source(7);
        let s = sig("Title");
        let t0 = 10 * BUCKET_SECS;
        for _ in 0..10 {
            f.record(p, s, t0);
        }
        for _ in 0..10 {
            f.record(p, s, t0 + 30 * 60);
        }
        assert_eq!(f.count(p, s, t0 + 30 * 60), 20);
        // The first ten leave once their bucket is an hour old.
        assert_eq!(f.count(p, s, t0 + WINDOW_SECS - 1), 20);
        assert_eq!(f.count(p, s, t0 + WINDOW_SECS), 10);
        // A clock that goes back counts in the newest bucket.
        assert_eq!(f.record(p, s, t0), Some(Level::Flag));
        // One set back two days starts the window again.
        let back = t0 + 30 * 60 - 2 * 86_400;
        assert_eq!(f.record(p, s, back), None);
        assert_eq!(f.count(p, s, back), 1);
    }

    #[test]
    fn one_publisher_name_on_two_sources_is_two_publishers() {
        let mut f = Flood::default();
        let s = sig("Title");
        for _ in 0..FLAG_AT - 1 {
            f.record(Origin::Publisher(1, "p"), s, 0);
        }
        assert_eq!(f.record(Origin::Publisher(2, "p"), s, 0), None);
        assert_eq!(f.count(Origin::Publisher(1, "p"), s, 0), FLAG_AT - 1);
        assert_eq!(f.record(Origin::Publisher(1, "p"), s, 0), Some(Level::Flag));
        // Past the hashed length, one publisher.
        let long = "x".repeat(MAX_PUBLISHER_BYTES);
        let longer = format!("{long}tail");
        assert_eq!(
            f.key(Origin::Publisher(1, &long)),
            f.key(Origin::Publisher(1, &longer))
        );
    }

    #[test]
    fn eviction_drops_the_one_seen_longest_ago() {
        let mut f = Flood::with_cap(3);
        let s = sig("Title");
        for p in ["a", "b", "c"] {
            f.record(Origin::Publisher(1, p), s, 0);
        }
        f.record(Origin::Publisher(1, "a"), s, 1);
        f.record(Origin::Publisher(1, "d"), s, 2);
        f.check().unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f.count(Origin::Publisher(1, "b"), s, 2), 0, "evicted");
        assert_eq!(f.count(Origin::Publisher(1, "a"), s, 2), 2);
        assert_eq!(f.count(Origin::Publisher(1, "c"), s, 2), 1);
    }

    #[test]
    fn a_flood_keeps_its_slot_among_many_titles() {
        let mut f = Flood::default();
        let p = Origin::Publisher(1, "p");
        let flood = sig("Same Spam");
        let mut level = None;
        for i in 0..FLAG_AT {
            level = f.record(p, flood, 0);
            // One-off titles in between, fewer than the slots, never push
            // the flood out: each takes the oldest one-off's slot.
            for j in 0..SIGNATURES as u32 - 1 {
                assert_eq!(f.record(p, sig(&format!("title {i} {j}")), 0), None);
            }
        }
        assert_eq!(level, Some(Level::Flag));
    }
}
