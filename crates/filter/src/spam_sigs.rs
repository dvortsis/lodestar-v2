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
//! up to [`MAX_ROW_BYTES`], 64 KB),
//! `filter.sp4.sigs.<n>` ([`PREFIX`]), numbered in the order they're
//! started. A row is `1:` and then 16 hex digits per entry: the
//! signature with its low three bits replaced by flags ([`REMOVED`],
//! [`HIDE`], [`BY_SP3`]). Entries are appended to the newest row only;
//! when it's full the next one starts. So a batch of hides rewrites one
//! row of at most 4 KB, never the set.
//!
//! - **A release appends a removal** (an entry with [`REMOVED`]) rather
//!   than rewriting the row that holds the signature. On load the last
//!   entry for a signature wins.
//! - **Compaction:** once a quarter of a row's entries are dead (removed,
//!   added again later, or dropped by the cap), that row alone is
//!   rewritten without them, or deleted when nothing in it is left. A
//!   removal is kept while an older row still holds an add it cancels.
//! - **The cap isn't written:** it's applied again on load, oldest first.
//!   Dropped entries go when their row is compacted. Until then a restart
//!   after releases took the set below its cap can bring a few of them
//!   back: at most a quarter of a row, the newest of those dropped. That
//!   errs strict.
//!
//! # Fail closed
//! A row that doesn't read (not text, not this format: a newer version's
//! row after a rollback, or a corrupted one) loses only its own
//! signatures. It's left as it is, never written over, and listed
//! ([`SpamSigs::labels`]) in `filter.stand_ins` and the Filters & Safety
//! notice, as RP-0 lists lost version state. New rows are numbered past
//! it. It's deleted once the cap has dropped something newer than
//! anything it can hold: everything in it would have gone by then.
//!
//! # Memory
//! About 45 MB at the cap: 8 bytes an entry as it's held, and an index from
//! signature to entry. Keys are hashed with SipHash under keys drawn at
//! start, because file sizes, and so signatures, are the sender's to
//! choose.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
/// The format tag a row starts with.
const FORMAT: &str = "1:";
/// Entries in a full row: 255.
pub const ROW_ENTRIES: usize = (ROW_BYTES - FORMAT.len()) / 16;
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

/// Where a live add is: its row, its index there, and its flags, packed
/// as `row << 19 | index << 3 | flags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Loc(u64);

impl Loc {
    fn new(row: u64, index: usize, flags: u64) -> Loc {
        Loc(row << 19 | (index as u64) << 3 | (flags & FLAGS))
    }
    fn row(self) -> u64 {
        self.0 >> 19
    }
    fn index(self) -> usize {
        ((self.0 >> 3) & 0xffff) as usize
    }
    fn flags(self) -> u64 {
        self.0 & FLAGS
    }
}

#[derive(Debug, Clone, Default)]
struct Row {
    entries: Vec<u64>,
    /// Entries that no longer count: adds no longer live, and removals
    /// (each one dead from the start, but kept while it's needed).
    dead: usize,
}

#[derive(Debug, Clone)]
struct Inner {
    cap: usize,
    /// The rows that read, by number.
    rows: BTreeMap<u64, Row>,
    /// Live signatures, by [`key`].
    live: HashMap<u64, Loc>,
    /// The rows that don't read: their keys' suffixes, numeric or not.
    lost: BTreeSet<String>,
    /// The number of the row entries are appended to, if it's still open.
    tail: Option<u64>,
    /// The number a new row takes: past every row seen, lost ones too.
    next: u64,
    /// Where the oldest live add may be: everything before it is dead.
    oldest: (u64, usize),
    /// Rows to write and rows to delete at the next flush.
    dirty: BTreeSet<u64>,
    gone: BTreeSet<String>,
}

impl Inner {
    fn new(cap: usize) -> Inner {
        Inner {
            cap,
            rows: BTreeMap::new(),
            live: HashMap::new(),
            lost: BTreeSet::new(),
            tail: None,
            next: 0,
            oldest: (0, 0),
            dirty: BTreeSet::new(),
            gone: BTreeSet::new(),
        }
    }

    /// Appends `entry` to the open row, starting one if needed. Returns
    /// where it went.
    fn append(&mut self, entry: u64) -> (u64, usize) {
        let open = self.tail.filter(|t| {
            self.rows
                .get(t)
                .is_some_and(|r| r.entries.len() < ROW_ENTRIES)
        });
        let n = match open {
            Some(n) => n,
            None => {
                let n = self.next;
                self.next += 1;
                self.rows.insert(n, Row::default());
                self.tail = Some(n);
                n
            }
        };
        let row = self.rows.get_mut(&n).expect("open row");
        row.entries.push(entry);
        self.dirty.insert(n);
        (n, row.entries.len() - 1)
    }

    /// Marks one entry of row `n` dead.
    fn kill(&mut self, n: u64, touched: &mut BTreeSet<u64>) {
        if let Some(r) = self.rows.get_mut(&n) {
            r.dead += 1;
            touched.insert(n);
        }
    }

    fn add(&mut self, sig: u64, o: Original) {
        let k = key(sig);
        let mut touched = BTreeSet::new();
        if let Some(old) = self.live.remove(&k) {
            self.kill(old.row(), &mut touched);
        }
        let flags = o.flags();
        let (n, i) = self.append(k | flags);
        self.live.insert(k, Loc::new(n, i, flags));
        while self.live.len() > self.cap {
            self.evict(&mut touched);
        }
        self.compact(touched);
    }

    fn remove(&mut self, sig: u64, rule: Option<PackRule>) -> bool {
        let k = key(sig);
        let Some(loc) = self.live.get(&k).copied() else {
            // Dropped by the cap but still stored, it would come back on
            // load (the module docs): a removal cancels it. Only entries
            // before the eviction point can be such adds; that's at most
            // a quarter of a row and rows that hold only removals.
            if self.dropped_add(k, rule) {
                let mut touched = BTreeSet::new();
                let (n, _) = self.append(k | REMOVED);
                self.kill(n, &mut touched);
                self.compact(touched);
            }
            return false;
        };
        if rule.is_some_and(|r| Original::from_flags(loc.flags()).rule != r) {
            return false;
        }
        self.live.remove(&k);
        let mut touched = BTreeSet::new();
        self.kill(loc.row(), &mut touched);
        let (n, _) = self.append(k | REMOVED);
        self.kill(n, &mut touched);
        self.compact(touched);
        true
    }

    /// Whether an add of `k` (by `rule`, if given) the cap dropped is still
    /// stored: one before the eviction point that no later entry cancels.
    fn dropped_add(&self, k: u64, rule: Option<PackRule>) -> bool {
        let (on, oi) = self.oldest;
        let mut last = None;
        for (n, r) in self.rows.range(..=on) {
            let upto = if *n == on {
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
                let from = if *n == on { oi.min(r.entries.len()) } else { 0 };
                r.entries[from..].iter()
            })
            .any(|x| key(*x) == k);
        !cancelled && rule.is_none_or(|r| Original::from_flags(e).rule == r)
    }

    /// Drops the oldest live signature.
    fn evict(&mut self, touched: &mut BTreeSet<u64>) {
        let (mut n, mut i) = self.oldest;
        let (n, i, k) = 'search: loop {
            let Some((&rn, row)) = self.rows.range(n..).next() else {
                return;
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
                        .get(&k)
                        .is_some_and(|l| l.row() == n && l.index() == i)
                {
                    break 'search (n, i, k);
                }
                i += 1;
            }
            let Some(after) = n.checked_add(1) else {
                return;
            };
            (n, i) = (after, 0);
        };
        self.live.remove(&k);
        self.oldest = (n, i + 1);
        self.kill(n, touched);
        // Everything a lost row older than this one holds would have gone
        // first.
        let older: Vec<String> = self
            .lost
            .iter()
            .filter(|s| s.parse::<u64>().is_ok_and(|l| l < n))
            .cloned()
            .collect();
        for s in older {
            self.lost.remove(&s);
            self.gone.insert(s);
        }
    }

    /// Rewrites each touched row once a quarter of it is dead.
    fn compact(&mut self, touched: BTreeSet<u64>) {
        for n in touched {
            let Some(row) = self.rows.get(&n) else {
                continue;
            };
            if row.dead == 0 || row.dead * 4 < row.entries.len() {
                continue;
            }
            // A removal is kept while an older row holds an add of its
            // signature: dropping it would bring that add back on load.
            let removals: BTreeSet<u64> = row
                .entries
                .iter()
                .filter(|e| *e & REMOVED != 0)
                .map(|e| key(*e))
                .collect();
            let needed: BTreeSet<u64> = if removals.is_empty() {
                BTreeSet::new()
            } else {
                self.rows
                    .range(..n)
                    .flat_map(|(_, r)| r.entries.iter())
                    .filter(|e| *e & REMOVED == 0 && removals.contains(&key(**e)))
                    .map(|e| key(*e))
                    .collect()
            };
            let live = &self.live;
            let kept: Vec<u64> = row
                .entries
                .iter()
                .enumerate()
                .filter(|(i, e)| {
                    let k = key(**e);
                    if **e & REMOVED != 0 {
                        needed.contains(&k)
                    } else {
                        live.get(&k)
                            .is_some_and(|l| l.row() == n && l.index() == *i)
                    }
                })
                .map(|(_, e)| *e)
                .collect();
            if self.oldest.0 == n {
                self.oldest.1 = 0;
            }
            if kept.is_empty() {
                self.rows.remove(&n);
                self.dirty.remove(&n);
                self.gone.insert(n.to_string());
                if self.tail == Some(n) {
                    self.tail = None;
                }
                continue;
            }
            for (i, e) in kept.iter().enumerate() {
                if e & REMOVED == 0 {
                    self.live.insert(key(*e), Loc::new(n, i, *e));
                }
            }
            // A kept removal still counts as dead: it's no live add.
            let dead = kept.iter().filter(|e| *e & REMOVED != 0).count();
            self.rows.insert(
                n,
                Row {
                    entries: kept,
                    dead,
                },
            );
            self.dirty.insert(n);
        }
    }
}

/// The set, shared by every version of the engine ([`crate::Engine`]
/// holds it in an `Arc`), the writer that adds to it and the API that
/// releases from it. Every write to the store is made under its lock, so
/// two writers' rows can't land out of order.
#[derive(Debug)]
pub struct SpamSigs {
    inner: Mutex<Inner>,
    /// Live signatures, read without the lock.
    len: AtomicUsize,
    /// Something to write.
    pending: AtomicBool,
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
}

impl SpamSigs {
    /// An empty set holding at most `cap` signatures. The build's cap is
    /// [`CAP`]; tests use a smaller one through the same code.
    pub fn with_cap(cap: usize) -> SpamSigs {
        SpamSigs {
            inner: Mutex::new(Inner::new(cap.max(1))),
            len: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
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
            .get(&key(sig))
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
        let set: Vec<(String, String)> = inner
            .dirty
            .iter()
            .filter_map(|n| {
                inner
                    .rows
                    .get(n)
                    .map(|r| (format!("{PREFIX}{n}"), encode(&r.entries)))
            })
            .collect();
        let gone: Vec<String> = inner.gone.iter().map(|s| format!("{PREFIX}{s}")).collect();
        let set_refs: Vec<(&str, &str)> =
            set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let gone_refs: Vec<&str> = gone.iter().map(String::as_str).collect();
        store.update_settings_text(&set_refs, &gone_refs)?;
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
        let mut inner = Inner::new(self.lock().cap);
        let mut readable: Vec<(u64, Vec<u64>)> = Vec::new();
        for (k, v) in rows {
            let Some(suffix) = k.strip_prefix(PREFIX) else {
                continue;
            };
            let number = canonical(suffix);
            if let Some(n) = number {
                inner.next = inner.next.max(n.saturating_add(1));
            }
            match (number, v.as_ref().ok().and_then(|t| decode(t))) {
                (Some(n), Some(entries)) => readable.push((n, entries)),
                _ => {
                    inner.lost.insert(match number {
                        Some(n) => n.to_string(),
                        None => sanitize(suffix),
                    });
                }
            }
        }
        // Keys sort as text; rows go by number.
        readable.sort_unstable_by_key(|(n, _)| *n);
        let mut touched = BTreeSet::new();
        for (n, entries) in readable {
            let mut row = Row::default();
            for (i, e) in entries.iter().enumerate() {
                let k = key(*e);
                if let Some(old) = inner.live.remove(&k) {
                    if old.row() == n {
                        row.dead += 1;
                    } else {
                        inner.kill(old.row(), &mut touched);
                    }
                }
                if e & REMOVED != 0 {
                    row.dead += 1;
                } else {
                    inner.live.insert(k, Loc::new(n, i, *e));
                }
            }
            row.entries = entries;
            inner.rows.insert(n, row);
        }
        inner.oldest = inner.rows.keys().next().map_or((0, 0), |n| (*n, 0));
        let max_seen = inner.next.checked_sub(1);
        inner.tail = inner
            .rows
            .last_key_value()
            .filter(|(n, r)| Some(**n) == max_seen && r.entries.len() < ROW_ENTRIES)
            .map(|(n, _)| *n);
        let mut evicted = BTreeSet::new();
        while inner.live.len() > inner.cap {
            inner.evict(&mut evicted);
        }
        // Dead entries found here are compacted on the next change to
        // their row: a load writes nothing. Lost rows the cap passed are
        // still listed until then.
        inner.lost.extend(inner.gone.iter().cloned());
        inner.gone.clear();
        let loaded = Loaded {
            len: inner.live.len(),
            lost: inner.lost.iter().cloned().collect(),
        };
        let mut slot = self.lock();
        *slot = inner;
        self.settle(&slot);
        loaded
    }

    /// The `filter.stand_ins` labels of the rows that didn't read:
    /// `packs.sp4.row.<n>`.
    pub fn labels(&self) -> Vec<String> {
        self.lock()
            .lost
            .iter()
            .map(|s| format!("{LABEL_PREFIX}{s}"))
            .collect()
    }

    /// How many rows the set holds, and how many entries in them: for
    /// tests and the measurements.
    #[doc(hidden)]
    pub fn shape(&self) -> (usize, usize) {
        let inner = self.lock();
        (
            inner.rows.len(),
            inner.rows.values().map(|r| r.entries.len()).sum(),
        )
    }
}

/// A row number as written: decimal, no sign, no leading zeros.
fn canonical(s: &str) -> Option<u64> {
    let n: u64 = s.parse().ok()?;
    (n.to_string() == s).then_some(n)
}

/// A key suffix that isn't a row number, as a label can carry it.
fn sanitize(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(32)
        .collect();
    if clean.is_empty() {
        "unnamed".to_owned()
    } else {
        clean
    }
}

fn encode(entries: &[u64]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(FORMAT.len() + entries.len() * 16);
    s.push_str(FORMAT);
    for e in entries {
        let _ = write!(s, "{e:016x}");
    }
    s
}

/// A row's entries, or `None` when it isn't this format.
fn decode(text: &str) -> Option<Vec<u64>> {
    let hex = text.strip_prefix(FORMAT)?;
    if text.len() > MAX_ROW_BYTES || !hex.len().is_multiple_of(16) {
        return None;
    }
    hex.as_bytes()
        .chunks_exact(16)
        .map(|c| {
            let c = std::str::from_utf8(c).ok()?;
            // Lowercase hex digits only, as written.
            if !c
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            u64::from_str_radix(c, 16).ok()
        })
        .collect()
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
        assert_eq!(decode("1:"), Some(Vec::new()));
        for bad in [
            "2:0000000000000000",
            "1:000",
            "1:000000000000000g",
            "1:000000000000000A",
            "x",
        ] {
            assert_eq!(decode(bad), None, "{bad}");
        }
        assert!(encode(&vec![0; ROW_ENTRIES]).len() <= ROW_BYTES);
        assert!(FORMAT.len() + 16 * (ROW_ENTRIES + 1) > ROW_BYTES);
        // A longer row another build wrote, up to 64 KB, still reads.
        let long = vec![7; (MAX_ROW_BYTES - FORMAT.len()) / 16];
        assert_eq!(decode(&encode(&long)), Some(long));
        assert_eq!(canonical("12"), Some(12));
        assert_eq!(canonical("012"), None);
        assert_eq!(canonical("+1"), None);
        assert_eq!(sanitize("a b@c~"), "abc");
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

    #[test]
    fn the_index_and_the_rows_agree_after_any_ops() {
        let s = SpamSigs::with_cap(50);
        let mut rng = 7u64;
        for _ in 0..20_000 {
            rng = crate::hash::fmix64(rng);
            let i = rng % 120;
            if rng.is_multiple_of(5) {
                s.lock().remove(sig(i), None);
            } else {
                s.add(sig(i), HID);
            }
        }
        let inner = s.lock();
        assert!(inner.live.len() <= 50);
        for (k, l) in &inner.live {
            let e = inner.rows[&l.row()].entries[l.index()];
            assert_eq!(key(e), *k);
            assert_eq!(e & REMOVED, 0);
        }
        for (n, r) in &inner.rows {
            let live = inner.live.values().filter(|l| l.row() == *n).count();
            assert_eq!(r.entries.len() - r.dead, live, "row {n}");
            assert!(r.entries.len() <= ROW_ENTRIES);
        }
    }
}
