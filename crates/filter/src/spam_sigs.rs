//! SP-4's signature set (§20.6, plan 2026-11-04 RP-5 and Q12): the
//! payload signatures ([`crate::payload_sig`]) of torrents a spam rule
//! hid, each with the rule and the action a clone inherits.
//!
//! - **Who adds:** the writer, when SP-2 hides a stored torrent or SP-3
//!   demotes or hides one ([`Original::of`]), and only for a payload
//!   specific enough that an unrelated torrent can't share it by chance
//!   ([`crate::PayloadSig::specific`], CS-8a's test).
//! - **Who removes:** the filters API's release hook, when the original
//!   is excepted from the rule that hid it or released
//!   (`crates/api/src/filters.rs`), so its clones follow it.
//! - **Capped** at [`CAP`] signatures; the oldest is dropped first.
//!
//! # Storage
//! Append-only `settings` rows of at most [`ROW_BYTES`] (4 KB; Q12 allows
//! up to [`MAX_ROW_BYTES`], 64 KB), `filter.sp4.sigs.<n>` ([`PREFIX`]),
//! numbered in the order they're started. A row is a blob: the format
//! byte [`FORMAT`], then 8 bytes per entry, little-endian: the signature
//! with its low three bits replaced by flags ([`REMOVED`], [`HIDE`],
//! [`BY_SP3`]). Entries are appended to the newest row only; when it's
//! full the next one starts. So a batch of hides rewrites one row of at
//! most 4 KB, never the set.
//!
//! - **A release appends a removal** (an entry with [`REMOVED`]) rather
//!   than rewriting the row that holds the signature. On load the last
//!   entry for a signature wins.
//! - **Compaction:** once a quarter of a row's entries are dead (removed,
//!   added again later, or dropped by the cap), that row alone is
//!   rewritten without them, or deleted when nothing in it is left. A
//!   removal is kept while an older row still holds an add it cancels.
//!   Once that add goes, the removal's own row is compacted too, so a row
//!   of removals nothing else touches still goes (X-sp4-followups; RP-5
//!   left such rows until something else in them changed).
//! - **The cap isn't written:** it's applied again on load, oldest first.
//!   Dropped entries go when their row is compacted. Until then a restart
//!   after releases took the set below its cap can bring a few of them
//!   back: at most a quarter of a row, the newest of those dropped. That
//!   errs strict.
//!
//! # Fail closed
//! A row that doesn't read (not a blob of this format: a newer version's
//! row after a rollback, or a corrupted one; or a key that isn't a row
//! number below [`ROW_BAND`]) loses only its own
//! signatures. It's left as it is, never written over, and listed
//! ([`SpamSigs::labels`]) in `filter.stand_ins` and the Filters & Safety
//! notice, as RP-0 lists lost version state. New rows are numbered past
//! it. It's deleted once the cap has dropped something newer than
//! anything it can hold: everything in it would have gone by then.
//!
//! # Memory
//! About 17 MB at the cap (X-sp4-followups; RP-5's `HashMap` index took
//! 45 MB): 8 bytes an entry as it's held, and an index of 4-byte
//! locations (`Index`), at most half full, so 2M buckets (8 MB) at 1M
//! signatures. A location names a row by its place in memory and an entry
//! by its index there; the key is read back from the entry, so the index
//! doesn't hold it again. Buckets come from SipHash under keys drawn at
//! start, because file sizes, and so signatures, are the sender's to
//! choose. A load sizes the index once.
//!
//! **Rows held at once:** a location has room for 524,288 rows
//! (`MAX_HELD_ROWS`): 2,000 or so hold the cap. Past that, which no set
//! reaches in practice, the oldest row goes to make room, at run time or
//! on a load: its signatures go as the cap drops the oldest, it's deleted
//! at the next flush, and it's logged (`rows_full`). That errs loose, as
//! the cap does, and the set keeps changing.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::BuildHasher;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lodestar_store::{RawSetting, Store};

use crate::core::Action;
use crate::packs::PackRule;
use crate::payload::PayloadSig;

/// At most this many signatures (Q12: 1M, 8 MB stored).
pub const CAP: usize = 1_000_000;
/// The `settings` key prefix of the rows: `filter.sp4.sigs.<n>`.
pub const PREFIX: &str = "filter.sp4.sigs.";
/// A row is at most this long (Q12), as read: a row this build didn't
/// write, up to this, still reads.
pub const MAX_ROW_BYTES: usize = 64 << 10;
/// A row this build writes is at most this long. A row is rewritten whole
/// with each batch that adds to it, and a 64 KB row costs about 200 µs a
/// write where 4 KB costs the transaction's own 17 µs or so
/// (docs/decisions/2026-11-05-RP-5.md).
pub const ROW_BYTES: usize = 4 << 10;
/// The format byte a row starts with.
pub const FORMAT: u8 = 1;
/// Entries in a full row: 511.
pub const ROW_ENTRIES: usize = (ROW_BYTES - 1) / 8;
/// A bound on row numbers, kept from RP-5: [`ROW_BAND`] is half of it.
pub const MAX_ROW: u64 = 1 << 40;
/// Row numbers are below this, read or written: a key at or past it is
/// listed as a lost key and never sets where numbering starts, and no
/// row is started there. Reaching it from row 0 takes 2^39 rows of 511
/// hides; only a stray key near it can bring it close. Then, once the
/// numbers run out, the set stops changing and says so
/// ([`SpamSigs::labels`]: `packs.sp4.row.numbers`), rather than write a
/// row a restart wouldn't read: no signature is added, and no release
/// takes effect either (that errs strict).
pub const ROW_BAND: u64 = MAX_ROW / 2;
/// Why a row was dropped, in the note a load returns and the log: memory
/// held as many rows as it can (`MAX_HELD_ROWS`).
pub const ROWS_FULL: &str = "more rows than memory holds";
/// The label suffix when the row numbers ran out ([`ROW_BAND`]).
const OUT_OF_NUMBERS: &str = "numbers";
/// The `filter.stand_ins` label prefix of a row that doesn't read.
pub const LABEL_PREFIX: &str = "packs.sp4.row.";

/// An entry removes its signature.
pub const REMOVED: u64 = 1;
/// The original was hidden (otherwise demoted).
pub const HIDE: u64 = 1 << 1;
/// The original's rule was SP-3 (otherwise SP-2).
pub const BY_SP3: u64 = 1 << 2;
const FLAGS: u64 = REMOVED | HIDE | BY_SP3;

/// A signature as the set keys it: its low three bits are the flags'.
fn key(sig: u64) -> u64 {
    sig & !FLAGS
}

/// What a clone inherits: the rule that hid the original and its action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Original {
    /// SP-2 or SP-3.
    pub rule: PackRule,
    /// Demote or hide.
    pub action: Action,
}

impl Original {
    /// The original a decision makes, if it feeds the set: SP-2 hiding,
    /// or SP-3 demoting or hiding. A torrent quarantined or dropped isn't
    /// stored, and a flag or SP-2's demote (through an override) isn't a
    /// hide.
    pub fn of(rule: PackRule, action: Action) -> Option<Original> {
        let feeds = match rule {
            PackRule::Sp2 => action == Action::Hide,
            PackRule::Sp3 => matches!(action, Action::Demote | Action::Hide),
            _ => false,
        };
        feeds.then_some(Original { rule, action })
    }

    fn flags(self) -> u64 {
        let hide = if self.action == Action::Hide { HIDE } else { 0 };
        let rule = if self.rule == PackRule::Sp3 {
            BY_SP3
        } else {
            0
        };
        hide | rule
    }

    fn from_flags(e: u64) -> Original {
        Original {
            rule: if e & BY_SP3 != 0 {
                PackRule::Sp3
            } else {
                PackRule::Sp2
            },
            action: if e & HIDE != 0 {
                Action::Hide
            } else {
                Action::Demote
            },
        }
    }
}

/// Whether a payload can join or match the set: specific payloads only.
pub fn usable(sig: Option<PayloadSig>) -> Option<u64> {
    sig.filter(|s| s.specific).map(|s| s.sig)
}

/// Where a live add is: its row, its index there, and its flags (the
/// entry's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Loc {
    row: u64,
    index: usize,
    flags: u64,
}

impl Loc {
    fn row(self) -> u64 {
        self.row
    }
    fn index(self) -> usize {
        self.index
    }
    fn flags(self) -> u64 {
        self.flags
    }
}

#[derive(Debug, Clone, Default)]
struct Row {
    /// Its number, `filter.sp4.sigs.<n>`.
    n: u64,
    entries: Vec<u64>,
    /// Entries that no longer count: adds no longer live, and removals
    /// (each one dead from the start, but kept while it's needed).
    dead: usize,
}

/// Bits of a [`Slot`] that give an entry's index in its row: a row as
/// read holds at most 8,191 entries ([`MAX_ROW_BYTES`]).
const INDEX_BITS: u32 = 13;
/// Rows held at once: the rest of a [`Slot`]'s bits.
const MAX_HELD_ROWS: usize = 1 << (32 - INDEX_BITS);
const _: () = assert!((MAX_ROW_BYTES - 1) / 8 < (1 << INDEX_BITS));

/// Where an entry is, as the index holds it in 4 bytes: the place of its
/// row in [`Rows::held`], then its index in that row. The largest
/// (`u32::MAX`, an index of 8,191) can't be an entry, so it marks an
/// empty bucket.
type Slot = u32;
const EMPTY: Slot = Slot::MAX;

/// The rows that read, held by place, and their numbers in order.
#[derive(Debug, Clone)]
struct Rows {
    /// At most this many held at once: [`MAX_HELD_ROWS`], lower in tests.
    limit: usize,
    /// Row number to its place in `held`.
    order: BTreeMap<u64, u32>,
    held: Vec<Row>,
    /// Places a deleted row left, for the next row.
    free: Vec<u32>,
}

impl Rows {
    fn new(limit: usize) -> Rows {
        Rows {
            limit: limit.min(MAX_HELD_ROWS),
            order: BTreeMap::new(),
            held: Vec::new(),
            free: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn get(&self, n: u64) -> Option<&Row> {
        self.order.get(&n).map(|p| &self.held[*p as usize])
    }

    fn get_mut(&mut self, n: u64) -> Option<&mut Row> {
        self.order.get(&n).map(|p| &mut self.held[*p as usize])
    }

    /// Holds a new row `n`. Returns its place, or `None` when no more
    /// rows can be held ([`MAX_HELD_ROWS`]).
    fn insert(&mut self, n: u64, entries: Vec<u64>) -> Option<u32> {
        debug_assert!(!self.order.contains_key(&n), "row {n} held twice");
        let row = Row {
            n,
            entries,
            dead: 0,
        };
        let place = match self.free.pop() {
            Some(p) => {
                self.held[p as usize] = row;
                p
            }
            None if self.held.len() < self.limit => {
                self.held.push(row);
                (self.held.len() - 1) as u32
            }
            None => return None,
        };
        self.order.insert(n, place);
        Some(place)
    }

    fn remove(&mut self, n: u64) {
        if let Some(p) = self.order.remove(&n) {
            self.held[p as usize] = Row::default();
            self.free.push(p);
        }
    }

    fn place(&self, n: u64) -> Option<u32> {
        self.order.get(&n).copied()
    }

    fn range(
        &self,
        r: impl std::ops::RangeBounds<u64>,
    ) -> impl DoubleEndedIterator<Item = (u64, &Row)> {
        self.order
            .range(r)
            .map(|(n, p)| (*n, &self.held[*p as usize]))
    }

    fn iter(&self) -> impl DoubleEndedIterator<Item = (u64, &Row)> {
        self.range(..)
    }

    fn slot(place: u32, index: usize) -> Slot {
        place << INDEX_BITS | index as u32
    }

    /// The entry a slot points at.
    fn entry(&self, s: Slot) -> u64 {
        self.held[(s >> INDEX_BITS) as usize].entries[(s & ((1 << INDEX_BITS) - 1)) as usize]
    }

    fn loc(&self, s: Slot) -> Loc {
        let row = &self.held[(s >> INDEX_BITS) as usize];
        let index = (s & ((1 << INDEX_BITS) - 1)) as usize;
        Loc {
            row: row.n,
            index,
            flags: row.entries[index] & FLAGS,
        }
    }
}

/// The live signatures: an open-addressed table of [`Slot`]s, 4 bytes a
/// bucket, the key read back from the entry a slot points at. Probed
/// linearly; a removal shifts the run after it back, so there are no
/// tombstones. At most half full, so a probe reads about two buckets.
///
/// **Hash flooding:** a signature is a hash of file sizes the sender
/// chooses (and `fmix64` can be inverted), so buckets come from SipHash
/// under keys drawn at start ([`RandomState`]), never from the
/// signature's own bits.
#[derive(Debug, Clone)]
struct Index {
    buckets: Vec<Slot>,
    len: usize,
    keys: RandomState,
}

impl Index {
    fn new() -> Index {
        Index {
            buckets: Vec::new(),
            len: 0,
            keys: RandomState::new(),
        }
    }

    /// Room for `n` live signatures without growing: a load sizes it
    /// once, so the tables it would outgrow on the way never stay
    /// resident.
    fn reserve(&mut self, n: usize, rows: &Rows) {
        let size = n.saturating_mul(2).next_power_of_two().max(16);
        if size > self.buckets.len() {
            self.resize(size, rows);
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn home(&self, k: u64) -> usize {
        self.keys.hash_one(k) as usize & (self.buckets.len() - 1)
    }

    /// The bucket that holds `k`.
    fn find(&self, k: u64, rows: &Rows) -> Option<usize> {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut b = self.home(k);
        loop {
            let s = self.buckets[b];
            if s == EMPTY {
                return None;
            }
            if key(rows.entry(s)) == k {
                return Some(b);
            }
            b = (b + 1) & mask;
        }
    }

    fn get(&self, k: u64, rows: &Rows) -> Option<Loc> {
        self.find(k, rows).map(|b| rows.loc(self.buckets[b]))
    }

    /// Whether `k` is live at `s`.
    fn at(&self, k: u64, s: Slot, rows: &Rows) -> bool {
        self.find(k, rows).is_some_and(|b| self.buckets[b] == s)
    }

    /// Points `k` at `s`, which must hold an add of `k`. Returns where it
    /// was.
    fn put(&mut self, k: u64, s: Slot, rows: &Rows) -> Option<Loc> {
        if let Some(b) = self.find(k, rows) {
            let old = rows.loc(self.buckets[b]);
            self.buckets[b] = s;
            return Some(old);
        }
        if (self.len + 1) * 2 > self.buckets.len() {
            self.grow(rows);
        }
        let mask = self.buckets.len() - 1;
        let mut b = self.home(k);
        while self.buckets[b] != EMPTY {
            b = (b + 1) & mask;
        }
        self.buckets[b] = s;
        self.len += 1;
        None
    }

    /// Shrinks the table to the size its signatures need, if it's larger.
    fn fit(&mut self, rows: &Rows) {
        let size = self.len.saturating_mul(2).next_power_of_two().max(16);
        if size < self.buckets.len() {
            self.resize(size, rows);
        }
    }

    fn grow(&mut self, rows: &Rows) {
        self.resize((self.buckets.len() * 2).max(16), rows);
    }

    fn resize(&mut self, size: usize, rows: &Rows) {
        debug_assert!(size.is_power_of_two() && size >= self.len * 2);
        let old = std::mem::replace(&mut self.buckets, vec![EMPTY; size]);
        let mask = size - 1;
        for s in old.into_iter().filter(|s| *s != EMPTY) {
            let mut b = self.home(key(rows.entry(s)));
            while self.buckets[b] != EMPTY {
                b = (b + 1) & mask;
            }
            self.buckets[b] = s;
        }
    }

    /// Takes `k` out. Returns where it was.
    fn remove(&mut self, k: u64, rows: &Rows) -> Option<Loc> {
        let at = self.find(k, rows)?;
        let old = rows.loc(self.buckets[at]);
        let mask = self.buckets.len() - 1;
        let mut hole = at;
        let mut b = at;
        loop {
            b = (b + 1) & mask;
            let s = self.buckets[b];
            if s == EMPTY {
                break;
            }
            // It moves into the hole unless its home lies after the hole,
            // up to it: it would be found no more.
            let home = self.home(key(rows.entry(s)));
            if b.wrapping_sub(home) & mask >= b.wrapping_sub(hole) & mask {
                self.buckets[hole] = s;
                hole = b;
            }
        }
        self.buckets[hole] = EMPTY;
        self.len -= 1;
        Some(old)
    }

    /// Every live signature and where it is: for tests.
    #[cfg(test)]
    fn iter<'a>(&'a self, rows: &'a Rows) -> impl Iterator<Item = (u64, Loc)> + 'a {
        self.buckets
            .iter()
            .filter(|s| **s != EMPTY)
            .map(|s| (key(rows.entry(*s)), rows.loc(*s)))
    }
}

#[derive(Debug, Clone)]
struct Inner {
    cap: usize,
    /// The rows that read, by number.
    rows: Rows,
    /// Live signatures, by [`key`].
    live: Index,
    /// How many adds no longer live each signature still has stored: a
    /// removal of it is needed while there's one ([`Inner::compact`]).
    stale: HashMap<u64, u32>,
    /// The rows that don't read, by number, and the keys under [`PREFIX`]
    /// that aren't row numbers, as labels can carry them.
    lost_rows: BTreeSet<u64>,
    lost_other: BTreeSet<String>,
    /// The number of the row entries are appended to, if it's still open.
    tail: Option<u64>,
    /// The number a new row takes: past every row seen, lost ones too.
    next: u64,
    /// Where the oldest live add may be: everything before it is dead.
    oldest: (u64, usize),
    /// Rows to write and rows to delete at the next flush.
    dirty: BTreeSet<u64>,
    gone: BTreeSet<u64>,
    /// A row couldn't be started: the numbers ran out ([`ROW_BAND`]).
    out_of_numbers: bool,
    /// The removals stored, by signature and row: a removal is needed only
    /// while a stale add of its signature is, and these find its row once
    /// none is ([`Inner::dropped_stale`]).
    removals: BTreeSet<(u64, u64)>,
    /// Signatures whose last stale add just went: their removals' rows
    /// are compacted next ([`Inner::compact`]).
    freed: Vec<u64>,
    /// Rows dropped because memory held `MAX_HELD_ROWS` ([`Inner::append`]),
    /// to be logged ([`SpamSigs::take_dropped`]).
    dropped: Vec<u64>,
}

impl Inner {
    fn new(cap: usize, row_limit: usize) -> Inner {
        Inner {
            cap,
            rows: Rows::new(row_limit),
            live: Index::new(),
            stale: HashMap::new(),
            lost_rows: BTreeSet::new(),
            lost_other: BTreeSet::new(),
            tail: None,
            next: 0,
            oldest: (0, 0),
            dirty: BTreeSet::new(),
            gone: BTreeSet::new(),
            out_of_numbers: false,
            removals: BTreeSet::new(),
            freed: Vec::new(),
            dropped: Vec::new(),
        }
    }

    /// Appends `entry` to the open row, starting one if needed. Returns
    /// where it went, or `None` when no row can be started (at
    /// [`ROW_BAND`]).
    fn append(&mut self, entry: u64) -> Option<(u64, usize)> {
        let open = self.tail.filter(|t| {
            self.rows
                .get(*t)
                .is_some_and(|r| r.entries.len() < ROW_ENTRIES)
        });
        let n = match open {
            Some(n) => n,
            None => {
                let n = self.next;
                if n >= ROW_BAND {
                    self.out_of_numbers = true;
                    return None;
                }
                // More rows than memory can address (never, in practice:
                // 524,288 rows): the oldest row goes, its signatures with
                // it, as the cap drops the oldest. That errs loose, and
                // it's logged.
                if self.rows.insert(n, Vec::new()).is_none()
                    && !(self.drop_oldest_row() && self.rows.insert(n, Vec::new()).is_some())
                {
                    self.out_of_numbers = true;
                    return None;
                }
                self.next = n + 1;
                self.tail = Some(n);
                n
            }
        };
        let row = self.rows.get_mut(n).expect("open row");
        row.entries.push(entry);
        let i = row.entries.len() - 1;
        self.dirty.insert(n);
        Some((n, i))
    }

    /// Counts a stored add of `k` that no longer lives.
    fn went_stale(&mut self, k: u64) {
        *self.stale.entry(k).or_default() += 1;
    }

    /// Counts a stale add of `k` dropped from the rows.
    fn dropped_stale(&mut self, k: u64) {
        if let Some(c) = self.stale.get_mut(&k) {
            *c -= 1;
            if *c == 0 {
                self.stale.remove(&k);
                self.freed.push(k);
            }
        }
    }

    /// Marks one entry of row `n` dead.
    fn kill(&mut self, n: u64, touched: &mut BTreeSet<u64>) {
        if let Some(r) = self.rows.get_mut(n) {
            r.dead += 1;
            touched.insert(n);
        }
    }

    fn add(&mut self, sig: u64, o: Original) {
        let k = key(sig);
        let flags = o.flags();
        // Out of row numbers (never, in practice): the set stays as it is.
        let Some((n, i)) = self.append(k | flags) else {
            return;
        };
        let mut touched = BTreeSet::new();
        let place = self.rows.place(n).expect("the open row");
        if let Some(old) = self.live.put(k, Rows::slot(place, i), &self.rows) {
            self.kill(old.row(), &mut touched);
            self.went_stale(k);
        }
        while self.live.len() > self.cap && self.evict(&mut touched) {}
        self.compact(touched);
    }

    fn remove(&mut self, sig: u64, rule: Option<PackRule>) -> bool {
        let k = key(sig);
        let Some(loc) = self.live.get(k, &self.rows) else {
            // Dropped by the cap but still stored, it would come back on
            // load (the module docs): a removal cancels it. Only entries
            // before the eviction point can be such adds; that's at most
            // a quarter of a row and rows that hold only removals.
            if self.dropped_add(k, rule)
                && let Some((n, _)) = self.append(k | REMOVED)
            {
                self.removals.insert((k, n));
                let mut touched = BTreeSet::new();
                self.kill(n, &mut touched);
                self.compact(touched);
            }
            return false;
        };
        if rule.is_some_and(|r| Original::from_flags(loc.flags()).rule != r) {
            return false;
        }
        // Out of row numbers (never, in practice): kept, as it's stored.
        let Some((n, _)) = self.append(k | REMOVED) else {
            return false;
        };
        self.removals.insert((k, n));
        let mut touched = BTreeSet::new();
        // Found again: starting a row for the removal may have dropped the
        // oldest row, and the add with it ([`Inner::drop_oldest_row`]).
        if let Some(loc) = self.live.remove(k, &self.rows) {
            self.went_stale(k);
            self.kill(loc.row(), &mut touched);
        }
        self.kill(n, &mut touched);
        self.compact(touched);
        true
    }

    /// Whether an add of `k` (by `rule`, if given) the cap dropped is still
    /// stored: one before the eviction point that no later entry cancels.
    fn dropped_add(&self, k: u64, rule: Option<PackRule>) -> bool {
        if !self.stale.contains_key(&k) {
            return false;
        }
        let (on, oi) = self.oldest;
        let mut last = None;
        for (n, r) in self.rows.range(..=on) {
            let upto = if n == on {
                oi.min(r.entries.len())
            } else {
                r.entries.len()
            };
            for e in &r.entries[..upto] {
                if key(*e) == k {
                    last = Some(*e);
                }
            }
        }
        let Some(e) = last.filter(|e| e & REMOVED == 0) else {
            return false;
        };
        // Nothing after the eviction point cancels it already.
        let cancelled = self
            .rows
            .range(on..)
            .flat_map(|(n, r)| {
                let from = if n == on { oi.min(r.entries.len()) } else { 0 };
                r.entries[from..].iter()
            })
            .any(|x| key(*x) == k);
        !cancelled && rule.is_none_or(|r| Original::from_flags(e).rule == r)
    }

    /// Drops the oldest live signature. Returns whether there was one to
    /// drop.
    fn evict(&mut self, touched: &mut BTreeSet<u64>) -> bool {
        let (mut n, mut i) = self.oldest;
        let (n, i, k) = 'search: loop {
            let Some((rn, row)) = self.rows.range(n..).next() else {
                debug_assert!(self.live.is_empty(), "a live entry the rows don't hold");
                return false;
            };
            if rn != n {
                (n, i) = (rn, 0);
            }
            while i < row.entries.len() {
                let e = row.entries[i];
                let k = key(e);
                if e & REMOVED == 0
                    && self
                        .live
                        .get(k, &self.rows)
                        .is_some_and(|l| l.row() == n && l.index() == i)
                {
                    break 'search (n, i, k);
                }
                i += 1;
            }
            let Some(after) = n.checked_add(1) else {
                return false;
            };
            (n, i) = (after, 0);
        };
        self.live.remove(k, &self.rows);
        self.went_stale(k);
        self.oldest = (n, i + 1);
        self.kill(n, touched);
        // Everything a lost row older than this one holds would have gone
        // first. Only rows by their own number: another key is never
        // deleted.
        let older: Vec<u64> = self.lost_rows.range(..n).copied().collect();
        for l in older {
            self.lost_rows.remove(&l);
            self.gone.insert(l);
        }
        true
    }

    /// Drops the oldest row and everything in it, when memory holds
    /// `MAX_HELD_ROWS` and a row must start. Its live adds go as the cap
    /// drops the oldest; its removals cancel nothing older, since no held
    /// row is older and the lost rows older than it go too, as an
    /// eviction past them does. Returns whether there was a row.
    fn drop_oldest_row(&mut self) -> bool {
        let Some((n, _)) = self.rows.iter().next() else {
            return false;
        };
        let place = self.rows.place(n).expect("held");
        let entries = self.rows.held[place as usize].entries.clone();
        for (i, e) in entries.iter().enumerate() {
            let k = key(*e);
            if e & REMOVED != 0 {
                self.removals.remove(&(k, n));
            } else if self.live.at(k, Rows::slot(place, i), &self.rows) {
                self.live.remove(k, &self.rows);
            } else {
                self.dropped_stale(k);
            }
        }
        self.rows.remove(n);
        self.dirty.remove(&n);
        self.gone.insert(n);
        if self.tail == Some(n) {
            self.tail = None;
        }
        if self.oldest.0 <= n {
            self.oldest = (n.saturating_add(1), 0);
        }
        let older: Vec<u64> = self.lost_rows.range(..n).copied().collect();
        for l in older {
            self.lost_rows.remove(&l);
            self.gone.insert(l);
        }
        self.dropped.push(n);
        true
    }

    /// Rewrites each touched row once a quarter of it is dead. A removal
    /// whose last stale add goes on the way touches its own row too, so a
    /// row of removals no longer needed goes, though nothing else would
    /// touch it again.
    fn compact(&mut self, mut touched: BTreeSet<u64>) {
        loop {
            self.compact_rows(touched);
            if self.freed.is_empty() {
                return;
            }
            touched = BTreeSet::new();
            for k in std::mem::take(&mut self.freed) {
                touched.extend(self.removals.range((k, 0)..=(k, u64::MAX)).map(|(_, n)| *n));
            }
        }
    }

    fn compact_rows(&mut self, touched: BTreeSet<u64>) {
        for n in touched {
            let Some(place) = self.rows.place(n) else {
                continue;
            };
            let row = &self.rows.held[place as usize];
            if row.dead == 0 || row.dead * 4 < row.entries.len() {
                continue;
            }
            // A removal is kept while a stale add of its signature is
            // still stored: dropping it could bring that add back on load.
            // (Conservative: the stale add may be one a later entry
            // cancels anyway.) The stale adds this row drops are counted
            // first, so a removal beside its own add goes with it.
            let mut kept = Vec::with_capacity(row.entries.len() - row.dead);
            // The index's bucket of each live add kept, in order: they're
            // pointed at the add's new place once the row is rewritten.
            // Buckets don't move until the index changes, which it
            // doesn't here.
            let mut buckets = Vec::with_capacity(row.entries.len() - row.dead);
            let mut dropped_adds = Vec::new();
            let mut removals = Vec::new();
            for (i, e) in row.entries.iter().enumerate() {
                let k = key(*e);
                if *e & REMOVED != 0 {
                    removals.push(kept.len());
                    kept.push(*e);
                } else if let Some(b) = self
                    .live
                    .find(k, &self.rows)
                    .filter(|b| self.live.buckets[*b] == Rows::slot(place, i))
                {
                    buckets.push(b);
                    kept.push(*e);
                } else {
                    dropped_adds.push(k);
                }
            }
            for k in dropped_adds {
                self.dropped_stale(k);
            }
            let stale = &self.stale;
            let held = &mut self.removals;
            let mut at = 0;
            let mut r = removals.into_iter().peekable();
            kept.retain(|e| {
                let i = at;
                at += 1;
                if r.peek() == Some(&i) {
                    r.next();
                    let needed = stale.contains_key(&key(*e));
                    if !needed {
                        held.remove(&(key(*e), n));
                    }
                    needed
                } else {
                    true
                }
            });
            if self.oldest.0 == n {
                self.oldest.1 = 0;
            }
            if kept.is_empty() {
                debug_assert!(buckets.is_empty(), "a live add dropped");
                self.rows.remove(n);
                self.dirty.remove(&n);
                self.gone.insert(n);
                if self.tail == Some(n) {
                    self.tail = None;
                }
                continue;
            }
            let mut buckets = buckets.into_iter();
            for (i, e) in kept.iter().enumerate() {
                if e & REMOVED == 0 {
                    let b = buckets.next().expect("a bucket for each live add");
                    self.live.buckets[b] = Rows::slot(place, i);
                }
            }
            // A kept removal still counts as dead: it's no live add.
            let dead = kept.iter().filter(|e| *e & REMOVED != 0).count();
            let row = &mut self.rows.held[place as usize];
            row.entries = kept;
            row.dead = dead;
            self.dirty.insert(n);
        }
    }
}

/// The set, shared by every version of the engine ([`crate::Engine`]
/// holds it in an `Arc`), the writer that adds to it and the API that
/// releases from it. Every write to the store is made under its lock, so
/// two writers' rows can't land out of order.
pub struct SpamSigs {
    inner: Mutex<Inner>,
    /// Live signatures, read without the lock.
    len: AtomicUsize,
    /// Something to write.
    pending: AtomicBool,
    /// Rows dropped to the held limit, not yet taken
    /// ([`SpamSigs::take_dropped`]): the writer reads this, not the lock,
    /// after each batch.
    dropped: AtomicBool,
}

impl std::fmt::Debug for SpamSigs {
    // Never the set itself: tens of MB at the cap.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpamSigs")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for SpamSigs {
    fn default() -> Self {
        SpamSigs::with_cap(CAP)
    }
}

/// What a load found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Live signatures.
    pub len: usize,
    /// The rows that didn't read, by their keys' suffixes.
    pub lost: Vec<String>,
    /// Rows dropped, oldest first, because memory can't hold that many
    /// (never, in practice): their signatures go, as the cap drops the
    /// oldest, and the rows are deleted at the next flush.
    pub dropped: Vec<u64>,
}

impl SpamSigs {
    /// An empty set holding at most `cap` signatures. The build's cap is
    /// [`CAP`]; tests use a smaller one through the same code.
    pub fn with_cap(cap: usize) -> SpamSigs {
        SpamSigs {
            inner: Mutex::new(Inner::new(cap.max(1), MAX_HELD_ROWS)),
            len: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn settle(&self, inner: &Inner) {
        self.len.store(inner.live.len(), Ordering::Release);
        self.pending.store(
            !inner.dirty.is_empty() || !inner.gone.is_empty(),
            Ordering::Release,
        );
        self.dropped
            .store(!inner.dropped.is_empty(), Ordering::Release);
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cap(&self) -> usize {
        self.lock().cap
    }

    /// The original a signature was added for, if it's in the set.
    pub fn get(&self, sig: u64) -> Option<Original> {
        if self.is_empty() {
            return None;
        }
        let inner = self.lock();
        inner
            .live
            .get(key(sig), &inner.rows)
            .map(|l| Original::from_flags(l.flags()))
    }

    /// Adds a signature in memory, in commit order; [`SpamSigs::flush`]
    /// writes it. Added again, it takes the newer original and counts
    /// from now for the cap.
    pub fn add(&self, sig: u64, original: Original) {
        let mut inner = self.lock();
        inner.add(sig, original);
        self.settle(&inner);
    }

    /// Writes what changed since the last flush: one row for a batch of
    /// adds, and a row compacted or deleted now and then. Nothing to
    /// write costs an atomic load. On an error the changes stay pending
    /// and the next flush writes them.
    pub fn flush(&self, store: &mut Store) -> Result<usize, lodestar_store::Error> {
        if !self.pending.load(Ordering::Acquire) {
            return Ok(0);
        }
        let mut inner = self.lock();
        let n = Self::write(&mut inner, store)?;
        self.settle(&inner);
        Ok(n)
    }

    fn write(inner: &mut Inner, store: &mut Store) -> Result<usize, lodestar_store::Error> {
        if inner.dirty.is_empty() && inner.gone.is_empty() {
            return Ok(0);
        }
        let set: Vec<(String, Vec<u8>)> = inner
            .dirty
            .iter()
            .filter_map(|n| {
                inner
                    .rows
                    .get(*n)
                    .map(|r| (format!("{PREFIX}{n}"), encode(&r.entries)))
            })
            .collect();
        let gone: Vec<String> = inner.gone.iter().map(|n| format!("{PREFIX}{n}")).collect();
        let set_refs: Vec<(&str, &[u8])> = set
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        let gone_refs: Vec<&str> = gone.iter().map(String::as_str).collect();
        store.update_settings_raw(&set_refs, &gone_refs)?;
        inner.dirty.clear();
        inner.gone.clear();
        Ok(set.len())
    }

    /// Removes signatures from the set and writes the removals, under one
    /// lock: the release hook's path. With `rule`, only a signature that
    /// rule added is removed (an exception to SP-2 leaves what SP-3 hid).
    /// Returns how many were in the set.
    pub fn release(
        &self,
        store: &mut Store,
        sigs: &[u64],
        rule: Option<PackRule>,
    ) -> Result<usize, lodestar_store::Error> {
        let mut inner = self.lock();
        let n = sigs.iter().filter(|s| inner.remove(**s, rule)).count();
        let written = Self::write(&mut inner, store);
        self.settle(&inner);
        written.map(|_| n)
    }

    /// Replaces the set with the stored rows (`settings` rows under
    /// [`PREFIX`], [`Store::settings_with_prefix_raw`]). A row that
    /// doesn't read is skipped and reported; the rest load. Nothing is
    /// written.
    pub fn load(&self, rows: &[RawSetting]) -> Loaded {
        let mut inner = {
            let held = self.lock();
            Inner::new(held.cap, held.rows.limit)
        };
        let mut readable: Vec<(u64, Vec<u64>)> = Vec::new();
        for (k, v) in rows {
            let Some(suffix) = k.strip_prefix(PREFIX) else {
                continue;
            };
            let Some(n) = canonical(suffix) else {
                inner.lost_other.insert(sanitize(suffix));
                continue;
            };
            inner.next = inner.next.max(n + 1);
            // A row is a blob; text under the prefix isn't this format. A
            // number is never both lost and read: `settings.key` is the
            // primary key, so each number comes once (and two rows under
            // one number, which a store can't hold, keep the last below).
            match v.as_ref().err().and_then(|b| decode(b)) {
                Some(entries) => readable.push((n, entries)),
                None => {
                    inner.lost_rows.insert(n);
                }
            }
        }
        // Keys sort as text; rows go by number. A store holds each key
        // once, but a load never trusts that: of two rows under one number
        // the last read stands, and the other is never held.
        readable.sort_by_key(|(n, _)| *n);
        readable.reverse();
        readable.dedup_by_key(|(n, _)| *n);
        readable.reverse();
        let adds = readable
            .iter()
            .map(|(_, e)| e.iter().filter(|e| *e & REMOVED == 0).count())
            .sum::<usize>();
        inner.live.reserve(adds.min(inner.cap), &inner.rows);
        let mut touched = BTreeSet::new();
        for (n, entries) in readable {
            let len = entries.len();
            // More rows than memory can address (never, in practice:
            // 524,288 rows): the oldest goes, as at run time.
            let place = match inner.rows.insert(n, Vec::new()) {
                Some(p) => p,
                None if inner.drop_oldest_row() => {
                    inner.rows.insert(n, Vec::new()).expect("a place freed")
                }
                None => {
                    inner.lost_rows.insert(n);
                    continue;
                }
            };
            inner.rows.held[place as usize].entries = entries;
            let mut dead = 0;
            for i in 0..len {
                let e = inner.rows.held[place as usize].entries[i];
                let k = key(e);
                let old = if e & REMOVED != 0 {
                    inner.live.remove(k, &inner.rows)
                } else {
                    inner.live.put(k, Rows::slot(place, i), &inner.rows)
                };
                if let Some(old) = old {
                    if old.row() == n {
                        dead += 1;
                    } else {
                        inner.kill(old.row(), &mut touched);
                    }
                }
                if e & REMOVED != 0 {
                    dead += 1;
                }
            }
            inner.rows.held[place as usize].dead = dead;
        }
        let mut stale: HashMap<u64, u32> = HashMap::new();
        for (n, row) in inner.rows.iter() {
            let place = inner.rows.place(n).expect("held");
            for (i, e) in row.entries.iter().enumerate() {
                let k = key(*e);
                if *e & REMOVED == 0 && !inner.live.at(k, Rows::slot(place, i), &inner.rows) {
                    *stale.entry(k).or_default() += 1;
                }
            }
        }
        inner.stale = stale;
        inner.freed.clear();
        inner.removals = inner
            .rows
            .iter()
            .flat_map(|(n, r)| {
                r.entries
                    .iter()
                    .filter(|e| *e & REMOVED != 0)
                    .map(move |e| (key(*e), n))
            })
            .collect();
        inner.oldest = inner.rows.iter().next().map_or((0, 0), |(n, _)| (n, 0));
        let max_seen = inner.next.checked_sub(1);
        inner.tail = inner
            .rows
            .iter()
            .next_back()
            .filter(|(n, r)| Some(*n) == max_seen && r.entries.len() < ROW_ENTRIES)
            .map(|(n, _)| n);
        let mut evicted = BTreeSet::new();
        while inner.live.len() > inner.cap && inner.evict(&mut evicted) {}
        // Dead entries found here are compacted on the next change to
        // their row: a load writes nothing. Lost rows the cap passed are
        // still listed until then.
        // A table sized for more than the cap (rows another cap wrote)
        // shrinks back once the cap is applied.
        inner.live.fit(&inner.rows);
        let dropped = std::mem::take(&mut inner.dropped);
        // Rows dropped to the held limit are deleted at the next flush, and
        // so are the lost rows older than them, as at run time
        // ([`Inner::drop_oldest_row`]): a newer build must never read adds
        // whose removals went with a dropped row. Lost rows only the cap
        // passed stay listed until their turn, as RP-5 left them.
        let newest_dropped = dropped.iter().max().copied();
        for n in std::mem::take(&mut inner.gone) {
            if dropped.contains(&n) || newest_dropped.is_some_and(|d| n < d) {
                inner.gone.insert(n);
            } else {
                inner.lost_rows.insert(n);
            }
        }
        let loaded = Loaded {
            len: inner.live.len(),
            lost: inner.lost_names(),
            dropped,
        };
        let mut slot = self.lock();
        *slot = inner;
        self.settle(&slot);
        loaded
    }

    /// The rows dropped since the last call because memory held
    /// `MAX_HELD_ROWS` (never, in practice), oldest first: for the writer
    /// to log after its flush.
    pub fn take_dropped(&self) -> Vec<u64> {
        // An atomic load while nothing was dropped (always, in practice):
        // the lock can be held across an API release's store write.
        if !self.dropped.load(Ordering::Acquire) {
            return Vec::new();
        }
        let mut inner = self.lock();
        let taken = std::mem::take(&mut inner.dropped);
        self.settle(&inner);
        taken
    }

    /// The `filter.stand_ins` labels of the rows that didn't read:
    /// `packs.sp4.row.<n>`.
    pub fn labels(&self) -> Vec<String> {
        self.lock()
            .lost_names()
            .iter()
            .map(|s| format!("{LABEL_PREFIX}{s}"))
            .collect()
    }

    /// The heap bytes the set holds, counted from its structures'
    /// capacities: the entries, the index, the rows and the stale counts
    /// (a std map's buckets as `hashbrown` sizes them), and the ordered
    /// sets at 32 bytes an item. What the allocator keeps beside them
    /// isn't counted. For the measurements.
    #[doc(hidden)]
    pub fn held_bytes(&self) -> usize {
        let inner = self.lock();
        let rows = &inner.rows;
        let stale_buckets = match inner.stale.capacity() {
            0 => 0,
            c => (c * 8 / 7).next_power_of_two(),
        };
        let small = inner.dirty.len()
            + inner.gone.len()
            + inner.lost_rows.len()
            + inner.lost_other.len()
            + rows.order.len();
        inner.live.buckets.capacity() * size_of::<Slot>()
            + rows.held.capacity() * size_of::<Row>()
            + rows
                .held
                .iter()
                .map(|r| r.entries.capacity() * 8)
                .sum::<usize>()
            + rows.free.capacity() * size_of::<u32>()
            + inner.removals.len() * 32
            + stale_buckets * (size_of::<(u64, u32)>() + 1)
            + small * 32
    }

    /// How many rows the set holds, and how many entries in them: for
    /// tests and the measurements.
    #[doc(hidden)]
    pub fn shape(&self) -> (usize, usize) {
        let inner = self.lock();
        (
            inner.rows.len(),
            inner.rows.iter().map(|(_, r)| r.entries.len()).sum(),
        )
    }
}

impl Inner {
    /// The rows that don't read, by number, then the other keys, then
    /// whether the row numbers ran out.
    fn lost_names(&self) -> Vec<String> {
        self.lost_rows
            .iter()
            .map(u64::to_string)
            .chain(self.lost_other.iter().cloned())
            .chain(self.out_of_numbers.then(|| OUT_OF_NUMBERS.to_owned()))
            .collect()
    }
}

/// A row number as written: decimal, no sign, no leading zeros, below
/// [`ROW_BAND`].
fn canonical(s: &str) -> Option<u64> {
    let n: u64 = s.parse().ok()?;
    (n.to_string() == s && n < ROW_BAND).then_some(n)
}

/// A key suffix that isn't a row number, as a label can carry it.
fn sanitize(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(32)
        .collect();
    // Never a bare number, which would read as a row's label, nor the
    // label of the numbers running out.
    if clean.is_empty() || clean.bytes().all(|b| b.is_ascii_digit()) || clean == OUT_OF_NUMBERS {
        format!("key-{clean}")
    } else {
        clean
    }
}

fn encode(entries: &[u64]) -> Vec<u8> {
    let mut b = Vec::with_capacity(1 + entries.len() * 8);
    b.push(FORMAT);
    for e in entries {
        b.extend_from_slice(&e.to_le_bytes());
    }
    b
}

/// A row's entries, or `None` when it isn't this format.
fn decode(bytes: &[u8]) -> Option<Vec<u64>> {
    let (&tag, body) = bytes.split_first()?;
    if tag != FORMAT || bytes.len() > MAX_ROW_BYTES || !body.len().is_multiple_of(8) {
        return None;
    }
    Some(
        body.chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HID: Original = Original {
        rule: PackRule::Sp2,
        action: Action::Hide,
    };

    fn sig(i: u64) -> u64 {
        crate::hash::fmix64(i + 1)
    }

    #[test]
    fn rows_round_trip_and_refuse_other_formats() {
        let e = [0, u64::MAX, 0x0123_4567_89ab_cdef];
        assert_eq!(decode(&encode(&e)), Some(e.to_vec()));
        assert_eq!(encode(&e).len(), 1 + 8 * 3, "packed 8 bytes an entry");
        assert_eq!(decode(&[FORMAT]), Some(Vec::new()));
        for bad in [
            &[2u8, 0, 0, 0, 0, 0, 0, 0, 0][..],
            &[1, 0, 0, 0],
            &[],
            b"1:00",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
        assert!(encode(&vec![0; ROW_ENTRIES]).len() <= ROW_BYTES);
        const { assert!(1 + 8 * (ROW_ENTRIES + 1) > ROW_BYTES) };
        // A longer row another build wrote, up to 64 KB, still reads; one
        // past it doesn't.
        let long = vec![7; (MAX_ROW_BYTES - 1) / 8];
        assert_eq!(decode(&encode(&long)), Some(long.clone()));
        let mut longer = long;
        longer.push(7);
        assert_eq!(decode(&encode(&longer)), None);
        assert_eq!(canonical("12"), Some(12));
        assert_eq!(canonical("012"), None);
        assert_eq!(canonical("+1"), None);
        assert_eq!(sanitize("a b@c~"), "abc");
        assert_eq!(sanitize("+1"), "key-1", "never a row's label");
        assert_eq!(
            sanitize("numbers"),
            "key-numbers",
            "never running out's label"
        );
        assert_eq!(canonical(&ROW_BAND.to_string()), None);
        assert_eq!(canonical(&(MAX_ROW - 1).to_string()), None);
        assert_eq!(canonical(&(ROW_BAND - 1).to_string()), Some(ROW_BAND - 1));
    }

    #[test]
    fn only_spam_hides_feed_it() {
        assert_eq!(Original::of(PackRule::Sp2, Action::Hide), Some(HID));
        assert_eq!(Original::of(PackRule::Sp2, Action::Demote), None);
        assert_eq!(Original::of(PackRule::Sp2, Action::Quarantine), None);
        assert!(Original::of(PackRule::Sp3, Action::Demote).is_some());
        assert!(Original::of(PackRule::Sp3, Action::Hide).is_some());
        assert_eq!(Original::of(PackRule::Sp3, Action::Flag), None);
        assert_eq!(Original::of(PackRule::Sp1, Action::Hide), None);
        assert_eq!(Original::of(PackRule::Sp4, Action::Hide), None);
        for o in [
            HID,
            Original::of(PackRule::Sp3, Action::Demote).unwrap(),
            Original::of(PackRule::Sp3, Action::Hide).unwrap(),
        ] {
            assert_eq!(Original::from_flags(o.flags()), o);
        }
    }

    #[test]
    fn the_flag_bits_never_split_a_signature() {
        let s = SpamSigs::with_cap(10);
        s.add(0x10, HID);
        // The same signature but for its low bits is the same entry.
        assert_eq!(s.get(0x17), Some(HID));
        assert_eq!(s.get(0x18), None);
    }

    /// Random adds and removals of `keys` signatures under `cap`, then
    /// every check that the index and the rows agree.
    fn agree_after(cap: usize, keys: u64, ops: usize) {
        let s = SpamSigs::with_cap(cap);
        let mut rng = 7u64;
        for _ in 0..ops {
            rng = crate::hash::fmix64(rng);
            let i = rng % keys;
            if rng.is_multiple_of(5) {
                s.lock().remove(sig(i), None);
            } else {
                s.add(sig(i), HID);
            }
        }
        check(&s.lock(), cap, keys);
        // The rows it stores, loaded again, agree as well.
        let again = SpamSigs::with_cap(cap);
        again.load(&stored(&s));
        check(&again.lock(), cap, keys);
    }

    /// Every check that the index and the rows agree.
    fn check(inner: &Inner, cap: usize, keys: u64) {
        assert!(inner.live.len() <= cap);
        // The removals index names every stored removal, and only those.
        let removals: BTreeSet<(u64, u64)> = inner
            .rows
            .iter()
            .flat_map(|(n, r)| {
                r.entries
                    .iter()
                    .filter(|e| *e & REMOVED != 0)
                    .map(move |e| (key(*e), n))
            })
            .collect();
        assert_eq!(removals, inner.removals);
        assert!(inner.freed.is_empty());
        // The stale counts are the stored adds that aren't live, exactly.
        let mut stale: HashMap<u64, u32> = HashMap::new();
        for (n, r) in inner.rows.iter() {
            let place = inner.rows.place(n).unwrap();
            for (i, e) in r.entries.iter().enumerate() {
                if *e & REMOVED == 0 && !inner.live.at(key(*e), Rows::slot(place, i), &inner.rows) {
                    *stale.entry(key(*e)).or_default() += 1;
                }
            }
        }
        assert_eq!(stale, inner.stale, "stale counts");
        assert!(
            inner.live.len() * 2 <= inner.live.buckets.len(),
            "at most half full"
        );
        // A signature never added is never found.
        for i in keys..keys + 1_000 {
            assert_eq!(inner.live.get(key(sig(i)), &inner.rows), None);
        }
        assert_eq!(inner.live.iter(&inner.rows).count(), inner.live.len());
        for (k, l) in inner.live.iter(&inner.rows) {
            let e = inner.rows.get(l.row()).unwrap().entries[l.index()];
            assert_eq!(key(e), k);
            assert_eq!(e & REMOVED, 0);
            assert_eq!(inner.live.get(k, &inner.rows), Some(l), "found");
        }
        for (n, r) in inner.rows.iter() {
            let live = inner
                .live
                .iter(&inner.rows)
                .filter(|(_, l)| l.row() == n)
                .count();
            assert_eq!(r.entries.len() - r.dead, live, "row {n}");
            assert!(r.entries.len() <= ROW_ENTRIES);
        }
    }

    #[test]
    fn the_index_and_the_rows_agree_after_any_ops() {
        agree_after(50, 120, 20_000);
        // Long probe runs, removals shifting them back, rows compacted
        // and deleted with their places reused.
        agree_after(5_000, 12_000, 200_000);
    }

    /// Rounds of hides, then a release of every fifth of them: each run
    /// of releases fills a row with removals only, while the rows of the
    /// adds they cancel stay under a quarter dead, so each removal is
    /// still needed. The cap later drops those adds and compacts their
    /// rows; the rows of removals then go too, so what's stored stays
    /// bounded however long it churns.
    #[test]
    fn rows_of_removals_dont_outlive_what_they_cancel() {
        let cap = 3_000;
        let s = SpamSigs::with_cap(cap);
        let per_round = 5 * ROW_ENTRIES as u64;
        let mut most = 0;
        for round in 0..60u64 {
            let first = round * per_round;
            for i in first..first + per_round {
                s.add(sig(i), HID);
            }
            for i in (first..first + per_round).step_by(5) {
                assert!(s.lock().remove(sig(i), None), "round {round}: {i}");
            }
            most = most.max(s.shape().1);
        }
        let (rows, entries) = s.shape();
        assert!(
            most <= 2 * cap + 4 * ROW_ENTRIES,
            "{most} entries at most, {entries} in {rows} rows at the end"
        );
    }

    #[test]
    fn a_slot_never_reads_as_empty() {
        let last = Rows::slot((MAX_HELD_ROWS - 1) as u32, (MAX_ROW_BYTES - 1) / 8 - 1);
        assert_ne!(last, EMPTY);
        assert_eq!(Rows::slot(1, 0), 1 << INDEX_BITS);
    }

    /// The stored rows as they'd read on a restart: for reload checks.
    fn stored(s: &SpamSigs) -> Vec<RawSetting> {
        s.lock()
            .rows
            .iter()
            .map(|(n, r)| (format!("{PREFIX}{n}"), Err(encode(&r.entries))))
            .collect()
    }

    #[test]
    fn past_what_memory_holds_the_oldest_row_goes() {
        // A cap above what three rows hold, so only the limit drops rows.
        let s = SpamSigs::with_cap(10_000);
        s.lock().rows.limit = 3;
        let full = 3 * ROW_ENTRIES as u64;
        for i in 0..full {
            s.add(sig(i), HID);
        }
        assert!(s.take_dropped().is_empty());
        // A fourth row drops the first, its signatures with it, as the cap
        // drops the oldest. Nothing is listed: it's logged instead.
        s.add(sig(full), HID);
        assert_eq!(s.take_dropped(), [0]);
        assert!(s.take_dropped().is_empty(), "taken once");
        assert_eq!(s.get(sig(0)), None);
        assert_eq!(s.get(sig(ROW_ENTRIES as u64 - 1)), None);
        assert!(s.get(sig(ROW_ENTRIES as u64)).is_some() && s.get(sig(full)).is_some());
        assert_eq!(s.len(), 2 * ROW_ENTRIES + 1);
        assert!(s.labels().is_empty());
        assert!(s.lock().gone.contains(&0), "deleted at the next flush");
        // It keeps changing: releases, then more adds, each dropping the
        // oldest row whenever a new one must start.
        for i in ROW_ENTRIES as u64..ROW_ENTRIES as u64 + 600 {
            assert!(s.lock().remove(sig(i), None), "{i}");
            assert_eq!(s.get(sig(i)), None);
        }
        for i in full + 1..full + 2_000 {
            s.add(sig(i), HID);
            assert!(s.get(sig(i)).is_some(), "{i}");
        }
        assert!(s.labels().is_empty());
        assert!(s.lock().rows.len() <= 3);
        check(&s.lock(), 10_000, full + 2_000);
        // Across a reload, with the same limit: the same signatures, and
        // it still takes adds and releases.
        let again = SpamSigs::with_cap(10_000);
        again.lock().rows.limit = 3;
        let loaded = again.load(&stored(&s));
        assert!(loaded.lost.is_empty() && loaded.dropped.is_empty());
        assert_eq!(loaded.len, s.len());
        for i in 0..full + 2_000 {
            assert_eq!(again.get(sig(i)), s.get(sig(i)), "{i}");
        }
        for i in full + 2_000..full + 3_000 {
            again.add(sig(i), HID);
            assert!(again.get(sig(i)).is_some(), "{i}");
        }
        assert!(again.lock().remove(sig(full + 2_500), None));
        assert!(again.labels().is_empty());
        check(&again.lock(), 10_000, full + 3_000);
    }

    /// A release whose removal must start a row, at the limit, drops the
    /// oldest row: when that row held the released add itself, nothing is
    /// left to count stale or to cancel.
    #[test]
    fn a_release_that_drops_its_own_add_leaves_nothing_behind() {
        let s = SpamSigs::with_cap(10_000);
        s.lock().rows.limit = 3;
        let full = 3 * ROW_ENTRIES as u64;
        for i in 0..full {
            s.add(sig(i), HID);
        }
        // sig(0) is in row 0, the oldest; the tail is full, so its removal
        // starts row 3 and row 0 goes.
        {
            let mut inner = s.lock();
            assert!(inner.remove(sig(0), None));
            s.settle(&inner);
        }
        assert_eq!(s.take_dropped(), [0]);
        assert_eq!(s.get(sig(0)), None);
        {
            let inner = s.lock();
            assert!(
                !inner.stale.contains_key(&key(sig(0))),
                "no phantom stale add"
            );
            assert!(
                !inner.removals.iter().any(|(k, _)| *k == key(sig(0))),
                "its removal cancels nothing, so it isn't kept"
            );
        }
        check(&s.lock(), 10_000, full);
        // And across a reload.
        let again = SpamSigs::with_cap(10_000);
        again.lock().rows.limit = 3;
        again.load(&stored(&s));
        assert_eq!(again.get(sig(0)), None);
        assert_eq!(again.len(), s.len());
        check(&again.lock(), 10_000, full);
    }

    #[test]
    fn a_load_past_what_memory_holds_drops_the_oldest_rows() {
        // Row 0 doesn't read; rows 1 to 5 do, one past the limit of 3 plus
        // the lost row's place being free: rows 1 and 2 are dropped.
        let mut rows: Vec<RawSetting> = (1..6u64)
            .map(|n| (format!("{PREFIX}{n}"), Err(encode(&[key(sig(n)) | HIDE]))))
            .collect();
        rows.push((format!("{PREFIX}0"), Err(vec![9, 9])));
        let s = SpamSigs::with_cap(100);
        s.lock().rows.limit = 3;
        let loaded = s.load(&rows);
        assert_eq!(loaded.len, 3);
        assert_eq!(loaded.dropped, [1, 2]);
        // The lost row older than a dropped one goes too, not listed.
        assert!(loaded.lost.is_empty(), "{:?}", loaded.lost);
        assert!(s.get(sig(2)).is_none() && s.get(sig(3)).is_some());
        // Deleted at the next flush, and the set keeps working.
        assert_eq!(s.lock().gone, BTreeSet::from([0, 1, 2]));
        s.add(sig(99), HID);
        assert!(s.get(sig(99)).is_some());
        // A deleted row's place is taken again.
        let mut rows = Rows::new(3);
        for n in 0..3 {
            assert_eq!(rows.insert(n, Vec::new()), Some(n as u32));
        }
        assert_eq!(rows.insert(3, Vec::new()), None);
        rows.remove(0);
        assert_eq!(rows.insert(9, Vec::new()), Some(0));
        assert_eq!(rows.insert(10, Vec::new()), None);
    }

    #[test]
    fn two_rows_under_one_number_load_as_the_last() {
        let row = |sigs: &[u64]| {
            let e: Vec<u64> = sigs.iter().map(|i| key(sig(*i)) | HIDE).collect();
            (format!("{PREFIX}6"), Err(encode(&e)))
        };
        let s = SpamSigs::with_cap(16);
        let loaded = s.load(&[row(&[1, 2]), row(&[3])]);
        assert_eq!(loaded.len, 1);
        assert!(s.get(sig(3)).is_some() && s.get(sig(1)).is_none());
        check(&s.lock(), 16, 10);
        s.add(sig(4), HID);
        assert!(s.get(sig(4)).is_some());
    }

    #[test]
    fn a_load_of_more_than_the_cap_shrinks_the_index_back() {
        let big = SpamSigs::with_cap(20_000);
        for i in 0..20_000 {
            big.add(sig(i), HID);
        }
        let s = SpamSigs::with_cap(5_000);
        assert_eq!(s.load(&stored(&big)).len, 5_000);
        let inner = s.lock();
        assert_eq!(
            inner.live.buckets.len(),
            16_384,
            "sized for 5,000, not 20,000"
        );
        check(&inner, 5_000, 20_000);
    }
}
