//! The in-memory ring buffer (architecture §12.1): the last 5,000 records,
//! at most 4 MiB accounted, that feed the live log the moment it opens.
//!
//! - **One sequence for every line.** A record gets its sequence under the
//!   ring's lock as it goes in, so the ring is always in sequence order and
//!   a stream's cursor is that sequence.
//! - **Cost:** a push is the insert, the evictions and the `allowed` check
//!   under one mutex: no I/O, no formatting, no client work. Streams are
//!   woken (a `watch`, callable from any thread) only while one is open.
//!   Readers clone `Arc`s out under the lock, a batch at a time, and
//!   filter outside it, so logging never waits on a reader.
//! - **Turning a kind off purges it** ([`Ring::purge`]): entries of an
//!   opt-in that turned off are removed, and other values of the kinds that
//!   turned off are rewritten to `[hidden]` (a hash may become its `ref:`).
//!   A record rendered under the old mask that arrives later is purged the
//!   same way as it goes in, so no window lets an off kind back in.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_log::{Kind, Mask};
use tokio::sync::watch;

use super::entry::{Entry, HIDDEN, Shown, parse_hex_hash};

/// Makes a torrent's `ref:` under this run's key.
pub type Refs<'a> = &'a dyn Fn(&[u8; 20]) -> String;

/// The most records kept.
pub const MAX_RECORDS: usize = 5_000;
/// The most bytes accounted ([`Entry::accounted`]).
pub const MAX_BYTES: usize = 4 << 20;
/// Purges remembered for streams that haven't looked since.
const PURGES_KEPT: usize = 64;
/// Entries cloned out per lock while reading.
const BATCH: usize = 256;

/// A purge, as streams are told of it: the kinds' values in lines up to
/// `through` read `[hidden]`, and lines whose opt-in is listed are gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Purge {
    pub generation: u64,
    pub kinds: Mask,
    pub through: u64,
}

#[derive(Debug)]
struct Inner {
    /// The last sequence given out (0: none yet).
    seq: u64,
    /// The highest sequence evicted by the bounds (a purge's removals
    /// aren't evictions): everything above it is here, or was purged.
    evicted: u64,
    entries: VecDeque<Arc<Entry>>,
    bytes: usize,
    /// What may show now: the live mask.
    allowed: Mask,
    purges: VecDeque<Purge>,
    generation: u64,
    max_records: usize,
    max_bytes: usize,
}

/// The ring.
#[derive(Debug)]
pub struct Ring {
    inner: Mutex<Inner>,
    /// The newest sequence, for streams: sent only while one listens.
    wake: watch::Sender<u64>,
    listeners: AtomicUsize,
}

/// What a read after a cursor found.
#[derive(Debug, Default)]
pub struct After {
    /// The first matching entries, at most `max`.
    pub lines: Vec<Arc<Entry>>,
    /// Every matching entry after the cursor, `lines` included.
    pub matching: u64,
    /// Entries after the cursor that left the ring before they were read.
    pub evicted: u64,
    /// The newest sequence read (the next cursor).
    pub head: u64,
}

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A stream's registration: wakes come while it's held.
#[derive(Debug)]
pub struct Listener<'r> {
    ring: &'r Ring,
}

impl Drop for Listener<'_> {
    fn drop(&mut self) {
        self.ring.listeners.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Ring {
    pub fn new(allowed: Mask) -> Ring {
        Ring::with_limits(allowed, MAX_RECORDS, MAX_BYTES)
    }

    pub fn with_limits(allowed: Mask, max_records: usize, max_bytes: usize) -> Ring {
        Ring {
            inner: Mutex::new(Inner {
                seq: 0,
                evicted: 0,
                entries: VecDeque::new(),
                bytes: 0,
                allowed,
                purges: VecDeque::new(),
                generation: 0,
                max_records,
                max_bytes,
            }),
            wake: watch::channel(0).0,
            listeners: AtomicUsize::new(0),
        }
    }

    /// Puts `entry` in, purged first if it carries a kind that's off now,
    /// and returns its sequence, what was kept (`None`: an opt-in that's
    /// off, not kept) and the kinds purged from it. The sequence is given
    /// either way, so the file line has its id.
    pub fn push(
        &self,
        mut entry: Entry,
        refs: Option<Refs<'_>>,
    ) -> (u64, Option<Arc<Entry>>, Mask) {
        let mut inner = lock(&self.inner);
        inner.seq += 1;
        let seq = inner.seq;
        entry.seq = seq;
        let off = Mask(entry.shown.0 & !inner.allowed.0);
        let off = match entry.opt_in {
            Some(k) if !inner.allowed.has(k) => off.with(k),
            _ => off,
        };
        let kept = match off == Mask::NONE {
            true => Some(entry),
            false => purged(&entry, off, refs),
        };
        let kept = kept.map(|e| {
            let e = Arc::new(e);
            inner.bytes += e.accounted();
            inner.entries.push_back(e.clone());
            while inner.entries.len() > inner.max_records || inner.bytes > inner.max_bytes {
                match inner.entries.pop_front() {
                    Some(old) => {
                        inner.bytes -= old.accounted();
                        inner.evicted = old.seq;
                    }
                    None => break,
                }
            }
            e
        });
        drop(inner);
        if self.listeners.load(Ordering::Relaxed) > 0 {
            self.wake.send_replace(seq);
        }
        (seq, kept, off)
    }

    /// Sets what may show from now on, and purges the kinds in `off` from
    /// what's kept. Returns the purge streams are told of, if `off` isn't
    /// empty.
    pub fn purge(&self, allowed: Mask, off: Mask, refs: Option<Refs<'_>>) -> Option<Purge> {
        let mut inner = lock(&self.inner);
        inner.allowed = allowed;
        if off == Mask::NONE {
            return None;
        }
        let entries = std::mem::take(&mut inner.entries);
        let mut bytes = 0;
        for e in entries {
            let touches = e.shown.0 & off.0 != 0 || e.opt_in.is_some_and(|k| off.has(k));
            let kept = match touches {
                false => Some(e),
                true => purged(&e, off, refs).map(Arc::new),
            };
            if let Some(e) = kept {
                bytes += e.accounted();
                inner.entries.push_back(e);
            }
        }
        inner.bytes = bytes;
        inner.generation += 1;
        let purge = Purge {
            generation: inner.generation,
            kinds: off,
            through: inner.seq,
        };
        inner.purges.push_back(purge);
        while inner.purges.len() > PURGES_KEPT {
            inner.purges.pop_front();
        }
        drop(inner);
        self.wake.send_replace(purge.through);
        Some(purge)
    }

    /// The purges after `generation`, and the newest generation. A stream
    /// so far behind that some were forgotten gets them as one.
    pub fn purges_since(&self, generation: u64) -> (u64, Vec<Purge>) {
        let inner = lock(&self.inner);
        let newer: Vec<Purge> = inner
            .purges
            .iter()
            .filter(|p| p.generation > generation)
            .copied()
            .collect();
        let forgotten = inner
            .purges
            .front()
            .is_some_and(|p| p.generation > generation.saturating_add(1));
        // The forgotten ones' kinds aren't known any more: every kind.
        let out = match forgotten {
            true => vec![Purge {
                generation: inner.generation,
                kinds: Mask(Kind::ALL.iter().fold(0, |m, k| m | k.bit())),
                through: newer.iter().map(|p| p.through).max().unwrap_or(0),
            }],
            false => newer,
        };
        (inner.generation, out)
    }

    /// The newest purge's generation.
    pub fn generation(&self) -> u64 {
        lock(&self.inner).generation
    }

    /// The newest sequence given out, and the lowest that hasn't been
    /// evicted (every one from it on is here, or was purged).
    pub fn bounds(&self) -> (u64, u64) {
        let inner = lock(&self.inner);
        (inner.seq, inner.evicted + 1)
    }

    /// How many records and bytes it holds.
    #[cfg(test)]
    pub fn usage(&self) -> (usize, usize) {
        let inner = lock(&self.inner);
        (inner.entries.len(), inner.bytes)
    }

    /// Registers a stream: until it's dropped, pushes wake `rx`.
    pub fn listen(&self) -> (Listener<'_>, watch::Receiver<u64>) {
        self.listeners.fetch_add(1, Ordering::Relaxed);
        (Listener { ring: self }, self.wake.subscribe())
    }

    #[cfg(test)]
    pub fn listeners(&self) -> usize {
        self.listeners.load(Ordering::Relaxed)
    }

    /// Every entry after `cursor` that `keep` accepts: the first `max` of
    /// them, and how many in all.
    pub fn after(&self, cursor: u64, max: usize, keep: impl Fn(&Entry) -> bool) -> After {
        let mut out = After {
            head: cursor,
            ..After::default()
        };
        let mut pos = cursor;
        let mut first = true;
        loop {
            let batch: Vec<Arc<Entry>> = {
                let inner = lock(&self.inner);
                if first {
                    // Evicted before they were read (a purge's removals
                    // aren't counted: they were never to be shown).
                    out.evicted = inner.evicted.saturating_sub(cursor);
                    if out.evicted > 0 {
                        pos = inner.evicted;
                    }
                    first = false;
                }
                let start = inner.entries.partition_point(|e| e.seq <= pos);
                inner.entries.range(start..).take(BATCH).cloned().collect()
            };
            let Some(last) = batch.last() else { break };
            pos = last.seq;
            for e in batch {
                if keep(&e) {
                    out.matching += 1;
                    if out.lines.len() < max {
                        out.lines.push(e);
                    }
                }
            }
        }
        out.head = out.head.max(pos);
        out
    }

    /// Entries with sequence below `before`, newest first, that `keep`
    /// accepts: at most `max`. Returns them and, when there were `max`, the
    /// lowest sequence looked at (the next page's `before`); else `None`
    /// and the lowest sequence the ring still covered at its last look
    /// (anything below it was evicted, so it's in the files).
    pub fn before(
        &self,
        before: u64,
        max: usize,
        keep: impl Fn(&Entry) -> bool,
    ) -> (Vec<Arc<Entry>>, Result<u64, u64>) {
        let mut out = Vec::new();
        let mut pos = before;
        loop {
            let (batch, low): (Vec<Arc<Entry>>, u64) = {
                let inner = lock(&self.inner);
                let end = inner.entries.partition_point(|e| e.seq < pos);
                let start = end.saturating_sub(BATCH);
                let batch = inner.entries.range(start..end).rev().cloned().collect();
                (batch, inner.evicted + 1)
            };
            if batch.is_empty() {
                return (out, Err(low.min(before)));
            }
            for e in batch {
                pos = e.seq;
                if keep(&e) {
                    out.push(e);
                    if out.len() == max {
                        return (out, Ok(pos));
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<Arc<Entry>> {
        lock(&self.inner).entries.iter().cloned().collect()
    }
}

/// `e` with the kinds in `off` taken out: `None` when it's a line of an
/// opt-in that's off, else its fields of those kinds as `[hidden]` (a
/// hash as this run's `ref:` when it can be).
pub fn purged(e: &Entry, off: Mask, refs: Option<Refs<'_>>) -> Option<Entry> {
    if e.opt_in.is_some_and(|k| off.has(k)) {
        return None;
    }
    let mut e = e.clone();
    for f in &mut e.fields {
        let Some(k) = f.shown_as else { continue };
        if !off.has(k) {
            continue;
        }
        let reference = match (k, &f.value, refs) {
            (Kind::Hash, Shown::Text(s), Some(r)) => parse_hex_hash(s).map(|h| r(&h)),
            _ => None,
        };
        f.value = Shown::Text(reference.unwrap_or_else(|| HIDDEN.into()));
        f.shown_as = None;
    }
    e.shown = Mask(e.shown.0 & !off.0);
    Some(e)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use lodestar_log::{Component, Level};

    use super::*;
    use crate::logging::entry::{Field, FieldType, Source};

    fn entry(fields: Vec<(&'static str, Kind, &str)>, bytes: usize) -> Entry {
        let mut shown = Mask::NONE;
        let fields = fields
            .into_iter()
            .map(|(name, k, v)| {
                shown = shown.with(k);
                Field {
                    name: Cow::Borrowed(name),
                    kind: FieldType::Data(k),
                    value: Shown::Text(v.to_string()),
                    shown_as: Some(k),
                }
            })
            .collect();
        Entry {
            seq: 0,
            millis: 0,
            level: Level::Info,
            component: Component::Fetch,
            source: Source::Target("t".into()),
            msg: Cow::Owned("x".repeat(bytes)),
            fields,
            shown,
            opt_in: None,
            truncated: false,
            repeated: None,
        }
    }

    #[test]
    fn both_bounds_hold_and_the_oldest_go_first() {
        let ring = Ring::new(Mask(u32::MAX));
        for _ in 0..MAX_RECORDS + 10 {
            ring.push(entry(vec![], 0), None);
        }
        let (n, _) = ring.usage();
        assert_eq!(n, MAX_RECORDS);
        assert_eq!(ring.bounds(), (MAX_RECORDS as u64 + 10, 11));
        // A purge's removals aren't evictions.
        let ring2 = Ring::new(Mask(u32::MAX));
        let mut d = entry(vec![], 0);
        d.opt_in = Some(Kind::FilterChange);
        ring2.push(d, None);
        ring2.push(entry(vec![], 0), None);
        ring2.purge(Mask::PRIVATE, Mask::NONE.with(Kind::FilterChange), None);
        assert_eq!(ring2.bounds(), (2, 1));
        assert_eq!(ring2.after(0, 10, |_| true).evicted, 0);
        let ring = Ring::new(Mask(u32::MAX));
        for _ in 0..1000 {
            ring.push(entry(vec![], 10_000), None);
        }
        let (n, bytes) = ring.usage();
        assert!(bytes <= MAX_BYTES, "{bytes}");
        assert!(n < 1000 && n > 300, "{n}");
        assert_eq!(ring.bounds().1, 1001 - n as u64);
    }

    #[test]
    fn turning_a_kind_off_rewrites_and_removes() {
        let all = Mask(u32::MAX);
        let ring = Ring::new(all);
        let h = "cd".repeat(20);
        ring.push(entry(vec![("name", Kind::Name, "Fixture.Name")], 0), None);
        ring.push(entry(vec![("torrent", Kind::Hash, &h)], 0), None);
        let mut detail = entry(vec![("change", Kind::FilterChange, "term x")], 0);
        detail.opt_in = Some(Kind::FilterChange);
        ring.push(detail, None);
        let r = |_: &[u8; 20]| "ref:12345678".to_string();
        let off = Mask::NONE
            .with(Kind::Name)
            .with(Kind::Hash)
            .with(Kind::FilterChange);
        let p = ring.purge(all.without(Kind::Name), off, Some(&r)).unwrap();
        assert_eq!((p.generation, p.through), (1, 3));
        let kept = ring.snapshot();
        assert_eq!(kept.len(), 2, "the opt-in's line is gone");
        let dump = format!("{kept:?}");
        assert!(
            !dump.contains("Fixture.Name") && !dump.contains(&h),
            "{dump}"
        );
        assert_eq!(kept[0].fields[0].value, Shown::Text(HIDDEN.into()));
        assert_eq!(kept[1].fields[0].value, Shown::Text("ref:12345678".into()));
        assert_eq!(ring.purges_since(0), (1, vec![p]));
        assert_eq!(ring.purges_since(1), (1, vec![]));
    }

    #[test]
    fn a_record_rendered_before_the_purge_is_purged_as_it_arrives() {
        let ring = Ring::new(Mask(u32::MAX));
        ring.purge(Mask::PRIVATE, Mask::NONE.with(Kind::Name), None);
        let (seq, kept, off) = ring.push(entry(vec![("name", Kind::Name, "Late.Name")], 0), None);
        assert_eq!(off, Mask::NONE.with(Kind::Name));
        assert_eq!(seq, 1);
        let kept = kept.unwrap();
        assert_eq!(kept.fields[0].value, Shown::Text(HIDDEN.into()));
        let mut late = entry(vec![], 0);
        late.opt_in = Some(Kind::FilterChange);
        let (seq, kept, _) = ring.push(late, None);
        assert_eq!((seq, kept.is_none()), (2, true), "a sequence, but not kept");
        assert!(!format!("{:?}", ring.snapshot()).contains("Late.Name"));
    }

    #[test]
    fn reads_after_and_before_a_cursor() {
        let ring = Ring::with_limits(Mask(u32::MAX), 10, MAX_BYTES);
        for i in 0..29 {
            let mut e = entry(vec![], 0);
            e.level = if i % 2 == 0 {
                Level::Info
            } else {
                Level::Debug
            };
            ring.push(e, None);
        }
        let info = |e: &Entry| e.level == Level::Info;
        let (guard, mut rx) = ring.listen();
        assert_eq!(ring.listeners(), 1);
        let mut last = entry(vec![], 0);
        last.level = Level::Debug;
        ring.push(last, None);
        assert!(rx.has_changed().unwrap(), "a listener is woken");
        drop(guard);
        assert_eq!(ring.listeners(), 0);
        rx.borrow_and_update();
        let a = ring.after(25, 100, info);
        assert_eq!(a.lines.iter().map(|e| e.seq).collect::<Vec<_>>(), [27, 29]);
        assert_eq!((a.matching, a.evicted, a.head), (2, 0, 30));
        let a = ring.after(5, 2, info);
        assert_eq!(a.evicted, 15, "6..=20 left unseen");
        assert_eq!(a.matching, 5);
        assert_eq!(a.lines.len(), 2);
        let (lines, next) = ring.before(26, 2, info);
        assert_eq!(lines.iter().map(|e| e.seq).collect::<Vec<_>>(), [25, 23]);
        assert_eq!(next, Ok(23));
        let (lines, next) = ring.before(23, 10, info);
        assert_eq!(lines.iter().map(|e| e.seq).collect::<Vec<_>>(), [21]);
        assert_eq!(next, Err(21), "20 and below were evicted");
    }
}
