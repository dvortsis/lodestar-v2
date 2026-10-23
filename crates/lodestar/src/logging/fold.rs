//! Flood folding (architecture §12.1, "Floods"): a burst of identical
//! lines becomes the first line and one "repeated 1,204 times in 10 s"
//! summary, in every output.
//!
//! - **Identity** is the line exactly as that output shows it, without its
//!   time and id ([`Entry::identity`]), kept as a 64-bit keyed hash, never
//!   the text. Each output folds by its own rendering, so a viewer learns
//!   nothing about values hidden from it. The file and the ring share one
//!   table: their masks are equal.
//! - **Window:** 10 s from the first occurrence. The first is always
//!   written at once; identical lines inside the window are counted and not
//!   written; when the window ends with a count, one summary goes out (the
//!   timer flushes within a second). The next identical line after the
//!   window starts a new one, so a sustained flood costs at most two lines
//!   per key per window.
//! - **Cap:** [`MAX_KEYS`] per table. Keys whose window ended go first; if
//!   it's still full, new lines pass unfolded, never dropped.
//! - **Never folded** (the caller's rule): required events, the runtime's
//!   notes about itself, and summaries.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, Instant};

use super::entry::{Entry, Repeated, cut};

/// Keys per table.
pub const MAX_KEYS: usize = 1_024;
/// The window.
pub const WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Slot {
    started: Instant,
    count: u64,
    first: Option<u64>,
    /// The first line, for its summary (fields are dropped from it).
    template: Entry,
}

/// What to do with a line.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Write it; `Some(key)` when it opened a window, so its sequence can
    /// be noted as the summary's `first`.
    Write(Option<u64>),
    /// Counted in an open window; don't write it.
    Fold,
}

/// One output's table.
#[derive(Debug)]
pub struct Table {
    hasher: RandomState,
    keys: HashMap<u64, Slot>,
    window: Duration,
    /// The earliest window's start (or earlier): until a window can have
    /// ended, nothing is scanned.
    earliest: Option<Instant>,
}

impl Table {
    pub fn new(window: Duration) -> Table {
        Table {
            hasher: RandomState::new(),
            keys: HashMap::new(),
            window,
            earliest: None,
        }
    }

    fn key(&self, e: &Entry) -> u64 {
        let mut h = self.hasher.build_hasher();
        e.identity(&mut h);
        std::hash::Hasher::finish(&h)
    }

    /// Decides for `e` at `now`. Summaries of windows that had to end
    /// first (its own, or ended ones flushed to make room) are added to
    /// `due`, to be written before it.
    pub fn check(&mut self, e: &Entry, now: Instant, due: &mut Vec<Entry>) -> Verdict {
        let key = self.key(e);
        let window = self.window;
        if let Some(slot) = self.keys.get_mut(&key) {
            if now.duration_since(slot.started) < window {
                slot.count += 1;
                return Verdict::Fold;
            }
            let slot = self.keys.remove(&key).expect("present");
            due.extend(summary(slot, window));
        }
        if self.keys.len() >= MAX_KEYS {
            self.flush(now, false, due);
            if self.keys.len() >= MAX_KEYS {
                return Verdict::Write(None);
            }
        }
        let mut template = e.summary(
            e.millis,
            Repeated {
                count: 0,
                seconds: 0,
                first: None,
            },
        );
        // A table of long untyped messages mustn't hold megabytes.
        if let Cow::Owned(m) = &mut template.msg {
            cut(m, 4096);
        }
        self.earliest = Some(self.earliest.map_or(now, |t| t.min(now)));
        self.keys.insert(
            key,
            Slot {
                started: now,
                count: 0,
                first: None,
                template,
            },
        );
        Verdict::Write(Some(key))
    }

    /// Notes the sequence the window's first line got.
    pub fn first(&mut self, key: u64, seq: u64) {
        if let Some(slot) = self.keys.get_mut(&key) {
            slot.first.get_or_insert(seq);
        }
    }

    /// Ends the windows that are over at `now` (every one with `all`),
    /// adding a summary for each that counted lines.
    pub fn flush(&mut self, now: Instant, all: bool, due: &mut Vec<Entry>) {
        let window = self.window;
        // No window can have ended yet: a full table of open windows costs
        // a comparison, not a scan, per new line.
        if !all && self.earliest.is_none_or(|t| now.duration_since(t) < window) {
            return;
        }
        let ended: Vec<u64> = self
            .keys
            .iter()
            .filter(|(_, s)| all || now.duration_since(s.started) >= window)
            .map(|(k, _)| *k)
            .collect();
        let mut out: Vec<Entry> = ended
            .into_iter()
            .filter_map(|k| self.keys.remove(&k))
            .filter_map(|s| summary(s, window))
            .collect();
        out.sort_by_key(|e| e.millis);
        due.extend(out);
        self.earliest = self.keys.values().map(|s| s.started).min();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

fn summary(slot: Slot, window: Duration) -> Option<Entry> {
    (slot.count > 0).then(|| {
        let millis = slot.template.millis + window.as_millis() as i64;
        slot.template.summary(
            millis,
            Repeated {
                count: slot.count,
                seconds: window.as_secs().max(1),
                first: slot.first,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use lodestar_log::{Component, Level, Mask};

    use super::*;
    use crate::logging::entry::{Field, FieldType, Shown, Source};

    fn line(msg: &str, value: &str) -> Entry {
        Entry {
            seq: 0,
            millis: 1_000,
            level: Level::Warn,
            component: Component::Fetch,
            source: Source::Target("t".into()),
            msg: Cow::Owned(msg.into()),
            fields: vec![Field {
                name: Cow::Borrowed("v"),
                kind: FieldType::Untyped,
                value: Shown::Text(value.into()),
                shown_as: None,
            }],
            shown: Mask::NONE,
            opt_in: None,
            truncated: false,
            repeated: None,
        }
    }

    #[test]
    fn a_burst_is_the_first_line_and_one_summary() {
        let mut t = Table::new(WINDOW);
        let t0 = Instant::now();
        let mut due = Vec::new();
        let Verdict::Write(Some(key)) = t.check(&line("m", "a"), t0, &mut due) else {
            panic!("the first is written")
        };
        t.first(key, 7);
        for i in 1..=1204 {
            let at = t0 + Duration::from_millis(i);
            assert_eq!(t.check(&line("m", "a"), at, &mut due), Verdict::Fold);
        }
        // Another value is another line.
        assert!(matches!(
            t.check(&line("m", "b"), t0, &mut due),
            Verdict::Write(Some(_))
        ));
        t.flush(t0 + Duration::from_secs(9), false, &mut due);
        assert!(due.is_empty(), "the window is open");
        t.flush(t0 + WINDOW, false, &mut due);
        assert_eq!(due.len(), 1, "one summary; `b` counted nothing");
        let s = &due[0];
        assert_eq!(
            s.repeated,
            Some(Repeated {
                count: 1204,
                seconds: 10,
                first: Some(7)
            })
        );
        assert!(s.fields.is_empty() && s.level == Level::Warn);
        assert_eq!(t.len(), 0);
        // After the window, the next one is written in full again.
        assert!(matches!(
            t.check(&line("m", "a"), t0 + WINDOW, &mut due),
            Verdict::Write(Some(_))
        ));
    }

    #[test]
    fn a_window_that_ended_unflushed_summarises_before_the_next_line() {
        let mut t = Table::new(WINDOW);
        let t0 = Instant::now();
        let mut due = Vec::new();
        t.check(&line("m", "a"), t0, &mut due);
        t.check(&line("m", "a"), t0, &mut due);
        let v = t.check(
            &line("m", "a"),
            t0 + WINDOW + Duration::from_secs(1),
            &mut due,
        );
        assert!(matches!(v, Verdict::Write(Some(_))));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].repeated.unwrap().count, 1);
    }

    #[test]
    fn the_cap_passes_lines_through() {
        let mut t = Table::new(WINDOW);
        let t0 = Instant::now();
        let mut due = Vec::new();
        for i in 0..MAX_KEYS {
            t.check(&line("m", &i.to_string()), t0, &mut due);
        }
        assert_eq!(t.len(), MAX_KEYS);
        // Full of open windows: a new line passes, unfolded, every time.
        for _ in 0..3 {
            assert_eq!(
                t.check(&line("new", "x"), t0, &mut due),
                Verdict::Write(None)
            );
        }
        // Once they've ended, they make room.
        let later = t0 + WINDOW;
        assert!(matches!(
            t.check(&line("new", "x"), later, &mut due),
            Verdict::Write(Some(_))
        ));
        assert_eq!(t.len(), 1);
        assert!(due.is_empty(), "nothing was counted");
    }

    #[test]
    fn flushing_everything() {
        let mut t = Table::new(WINDOW);
        let t0 = Instant::now();
        let mut due = Vec::new();
        t.check(&line("m", "a"), t0, &mut due);
        t.check(&line("m", "a"), t0, &mut due);
        t.flush(t0, true, &mut due);
        assert_eq!(due.len(), 1);
        assert_eq!(t.len(), 0);
    }
}
