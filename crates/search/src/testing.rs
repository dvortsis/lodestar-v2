//! Test fixtures: an in-memory index filled with raw documents (no
//! store), and a query that's slow on purpose, for cancellation.

use std::time::Duration;

use tantivy::indexer::NoMergePolicy;
use tantivy::query::{AllQuery, EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, TantivyDocument, Term};

use crate::SearchIndex;
use crate::index::Compiled;

/// Torrent `i`'s name: every name has `Show{i}`; `Common` is in 9 of 10,
/// `Half` in every other, `Rare` in 1 of 100, and `Recent` in the last
/// fifth by id (a block of the doc-id space, like a recent date range).
pub(crate) fn name(i: u64, n: u64) -> String {
    let mut words = vec![format!("Show{i}")];
    if i % 10 != 0 {
        words.push("Common".into());
    }
    if i % 2 == 0 {
        words.push("Half".into());
    }
    if i % 100 == 0 {
        words.push("Rare".into());
    }
    if i >= n * 4 / 5 {
        words.push("Recent".into());
    }
    words.join(".")
}

/// An index of `n` torrents (ids 1..=n, size 1000 + position, discovered
/// in id order) in `segments` segments, never merged, and their names in
/// id order.
pub(crate) fn index_with(n: u64, segments: u64) -> (SearchIndex, Vec<String>) {
    let index = SearchIndex::in_memory().unwrap();
    let per = n.div_ceil(segments.max(1)).max(1);
    let mut from = 1;
    while from <= n {
        let to = (from + per).min(n + 1);
        append(&index, from..to, n);
        from = to;
    }
    (index, (0..n).map(|i| name(i, n)).collect())
}

/// Adds torrents `ids` (named as by [`name`] for an index of `n`) in
/// one new segment and makes them visible.
pub(crate) fn append(index: &SearchIndex, ids: std::ops::Range<u64>, n: u64) {
    let f = *index.fields();
    let (raw, reader) = index.raw();
    let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 15 << 20).unwrap();
    w.set_merge_policy(Box::new(NoMergePolicy));
    for id in ids {
        let i = id - 1;
        let name = name(i, n);
        let mut d = TantivyDocument::new();
        d.add_u64(f.id, id);
        d.add_text(f.name, &name);
        d.add_text(f.text, &name);
        d.add_u64(f.size, 1000 + i);
        d.add_u64(f.files, 1 + i % 7);
        d.add_i64(f.discovered, 1_700_000_000 + i as i64);
        w.add_document(d).unwrap();
    }
    w.commit().unwrap();
    reader.reload().unwrap();
}

/// Deletes the torrents with these ids and makes it visible.
pub(crate) fn delete(index: &SearchIndex, ids: impl IntoIterator<Item = u64>) {
    let (raw, reader) = index.raw();
    let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 15 << 20).unwrap();
    w.set_merge_policy(Box::new(NoMergePolicy));
    for id in ids {
        w.delete_term(Term::from_field_u64(index.fields().id, id));
    }
    w.commit().unwrap();
    reader.reload().unwrap();
}

/// Matches every document, taking `delay` to step to each one.
pub(crate) fn slow(index: &SearchIndex, delay: Duration) -> Compiled {
    let _ = index;
    Compiled {
        query: Box::new(SlowQuery { delay }),
        filters: Vec::new(),
        boosts: None,
        key: format!("slow{delay:?}"),
    }
}

#[derive(Debug, Clone)]
struct SlowQuery {
    delay: Duration,
}

impl Query for SlowQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(SlowWeight {
            inner: AllQuery.weight(scoring)?,
            delay: self.delay,
        }))
    }
}

struct SlowWeight {
    inner: Box<dyn Weight>,
    delay: Duration,
}

impl Weight for SlowWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(SlowScorer {
            inner: self.inner.scorer(reader, boost)?,
            delay: self.delay,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.inner.explain(reader, doc)
    }
}

struct SlowScorer {
    inner: Box<dyn Scorer>,
    delay: Duration,
}

impl DocSet for SlowScorer {
    fn advance(&mut self) -> DocId {
        spin(self.delay);
        self.inner.advance()
    }

    fn doc(&self) -> DocId {
        self.inner.doc()
    }

    fn size_hint(&self) -> u32 {
        self.inner.size_hint()
    }
}

impl Scorer for SlowScorer {
    fn score(&mut self) -> Score {
        self.inner.score()
    }
}

/// Busy-waits: sleeps are far coarser than microseconds on some hosts.
fn spin(d: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}
