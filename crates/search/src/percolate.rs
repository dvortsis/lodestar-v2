//! Percolation (architecture §8, "Saved searches → alerts"): after an NRT
//! commit, each saved search runs against only the torrents indexed since
//! its watermark.
//!
//! - **Exact tier only:** the query as [`SearchIndex::query`] compiles it,
//!   never the stem fallback, so an alert is a torrent the search's own
//!   first page could have shown.
//! - **One searcher snapshot per run**, taken once and shared by every
//!   search in it. Neither a lease nor the result cache is involved: the
//!   ranges are new every run, so nothing would be reused.
//! - **No visibility clauses.** An alert is stored for every match (B8);
//!   what's hidden is left out when alerts are read (the API).
//! - **Bounded:** each search covers at most `max_range` ids per run and
//!   keeps at most `keep` hits (the highest ids). The token is checked
//!   between searches, so a budget stops a run cleanly; what it didn't
//!   reach runs next time.
//!
//! No store writes here: the caller records the result in one
//! transaction per range ([`lodestar_store::Store::record_percolation`]).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Bound;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery};
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader, Term};

use crate::cancel::CancelToken;
use crate::filter::PostFilter;
use crate::index::{Result, SearchIndex};
use crate::query::Node;

/// How far one run goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PercolateLimits {
    /// Hits kept per search per run: the highest ids.
    pub keep: usize,
    /// Torrent ids one search covers per run, so a catch-up after
    /// downtime takes several runs instead of one long one.
    pub max_range: i64,
}

/// A saved search due to run.
#[derive(Debug, Clone, Copy)]
pub struct Due<'a> {
    pub id: i64,
    /// The highest id already evaluated against it.
    pub watermark: i64,
    pub query: &'a Node,
}

/// One search's result: every torrent id in `(watermark, through]` it
/// matched, at most [`PercolateLimits::keep`] of them, highest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub id: i64,
    /// Where its watermark moves once this is recorded.
    pub through: i64,
    pub torrent_ids: Vec<i64>,
    /// More matched than `keep`; the lowest were left out.
    pub truncated: bool,
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Percolation {
    /// Searches that ran, in the order given.
    pub matched: Vec<Matched>,
    /// The highest id in the snapshot, which bounds every range: a
    /// search never moves past what it could see.
    pub snapshot_max: i64,
    /// The token stopped the run before every search ran.
    pub stopped: bool,
    /// Searches whose range was cut by `max_range`: they're still behind.
    pub capped: usize,
    /// Searches that failed in the index, by id. They keep their
    /// watermark and run again next time.
    pub failed: Vec<i64>,
}

impl Percolation {
    /// Whether another run right away would find more to do.
    pub fn more(&self) -> bool {
        self.stopped || self.capped > 0
    }
}

/// Runs `due` against `index` up to `through`, on one searcher snapshot,
/// checking `cancel` before each search. `now` resolves relative dates.
pub fn percolate(
    index: &SearchIndex,
    due: &[Due<'_>],
    through: i64,
    limits: PercolateLimits,
    now: i64,
    cancel: &CancelToken,
) -> Result<Percolation> {
    let searcher = index.searcher();
    let mut snapshot_max = 0i64;
    for seg in searcher.segment_readers() {
        let col = seg.fast_fields().u64("id")?;
        if seg.num_docs() > 0 {
            snapshot_max = snapshot_max.max(i64::try_from(col.max_value()).unwrap_or(i64::MAX));
        }
    }
    let upto = through.min(snapshot_max);
    let mut out = Percolation {
        snapshot_max,
        ..Percolation::default()
    };
    let id_field = index.fields().id;
    // Ids start at 1, so a watermark of 0 or below never changes a range.
    let term = |v: i64| Term::from_field_u64(id_field, v.max(0) as u64);
    for d in due {
        let end = upto.min(d.watermark.saturating_add(limits.max_range.max(1)));
        if end <= d.watermark {
            continue;
        }
        if cancel.check().is_err() {
            out.stopped = true;
            break;
        }
        let compiled = index.query(d.query, now);
        let range: Box<dyn Query> = Box::new(RangeQuery::new(
            Bound::Excluded(term(d.watermark)),
            Bound::Included(term(end)),
        ));
        let q = BooleanQuery::new(vec![(Occur::Must, compiled.query), (Occur::Must, range)]);
        let collector = PostFilter {
            ranges: &compiled.filters,
            inner: TopIds { keep: limits.keep },
        };
        match searcher.search(&q, &collector) {
            Ok((ids, truncated)) => {
                if end < upto {
                    out.capped += 1;
                }
                out.matched.push(Matched {
                    id: d.id,
                    through: end,
                    torrent_ids: ids,
                    truncated,
                });
            }
            Err(e) => {
                tracing::warn!("percolator: saved search {} failed in the index: {e}", d.id);
                out.failed.push(d.id);
            }
        }
    }
    Ok(out)
}

/// Collects the `keep` highest torrent ids, highest first, and whether
/// any match was left out.
struct TopIds {
    keep: usize,
}

struct TopIdsSegment {
    col: tantivy::columnar::Column<u64>,
    keep: usize,
    /// A min-heap of the highest ids so far.
    heap: BinaryHeap<Reverse<u64>>,
    over: bool,
}

/// Up to `keep` highest of `ids`, highest first, and whether any were cut.
fn top(ids: impl IntoIterator<Item = u64>, keep: usize) -> (Vec<i64>, bool) {
    let mut heap: BinaryHeap<Reverse<u64>> = BinaryHeap::with_capacity(keep.min(1024) + 1);
    let mut over = false;
    for id in ids {
        push(&mut heap, keep, id, &mut over);
    }
    let mut out: Vec<i64> = heap
        .into_iter()
        .map(|Reverse(id)| i64::try_from(id).unwrap_or(i64::MAX))
        .collect();
    out.sort_unstable_by(|a, b| b.cmp(a));
    (out, over)
}

fn push(heap: &mut BinaryHeap<Reverse<u64>>, keep: usize, id: u64, over: &mut bool) {
    if heap.len() < keep {
        heap.push(Reverse(id));
        return;
    }
    *over = true;
    if let Some(&Reverse(low)) = heap.peek()
        && id > low
    {
        heap.pop();
        heap.push(Reverse(id));
    }
}

impl Collector for TopIds {
    type Fruit = (Vec<i64>, bool);
    type Child = TopIdsSegment;

    fn for_segment(
        &self,
        _: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(TopIdsSegment {
            col: reader.fast_fields().u64("id")?,
            keep: self.keep,
            heap: BinaryHeap::new(),
            over: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<(Vec<u64>, bool)>) -> tantivy::Result<Self::Fruit> {
        let over = fruits.iter().any(|(_, o)| *o);
        let (ids, cut) = top(fruits.into_iter().flat_map(|(ids, _)| ids), self.keep);
        Ok((ids, over || cut))
    }
}

impl SegmentCollector for TopIdsSegment {
    type Fruit = (Vec<u64>, bool);

    fn collect(&mut self, doc: DocId, _: Score) {
        if let Some(id) = self.col.first(doc) {
            push(&mut self.heap, self.keep, id, &mut self.over);
        }
    }

    fn harvest(self) -> Self::Fruit {
        (
            self.heap.into_iter().map(|Reverse(id)| id).collect(),
            self.over,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::query::parse;
    use crate::testing::{append, delete, index_with};

    const ALL: PercolateLimits = PercolateLimits {
        keep: usize::MAX,
        max_range: i64::MAX,
    };

    fn run(
        index: &SearchIndex,
        q: &str,
        watermark: i64,
        through: i64,
        limits: PercolateLimits,
    ) -> Percolation {
        let node = parse(q).unwrap();
        let due = [Due {
            id: 7,
            watermark,
            query: &node,
        }];
        percolate(index, &due, through, limits, 0, &CancelToken::new()).unwrap()
    }

    fn ids(p: &Percolation) -> Vec<i64> {
        p.matched[0].torrent_ids.clone()
    }

    /// The reference: what `(lo, hi]` holds that `q` matches, by brute force
    /// over the names `testing` gives.
    fn expected(q: &str, n: u64, lo: i64, hi: i64) -> Vec<i64> {
        let word = q.to_ascii_lowercase();
        (1..=n as i64)
            .rev()
            .filter(|&id| id > lo && id <= hi)
            .filter(|&id| {
                crate::testing::name((id - 1) as u64, n)
                    .to_ascii_lowercase()
                    .split('.')
                    .any(|w| w == word)
            })
            .collect()
    }

    #[test]
    fn only_the_range_matches_highest_first() {
        let (index, _) = index_with(300, 4);
        let p = run(&index, "Rare", 0, 300, ALL);
        assert_eq!(ids(&p), expected("rare", 300, 0, 300));
        assert_eq!(ids(&p), [201, 101, 1]);
        let p = run(&index, "Rare", 1, 200, ALL);
        assert_eq!(ids(&p), [101]);
        assert_eq!(p.matched[0].through, 200);
        for (lo, hi) in [(0, 10), (37, 150), (150, 151), (299, 300)] {
            let p = run(&index, "Half", lo, hi, ALL);
            assert_eq!(ids(&p), expected("half", 300, lo, hi), "({lo}, {hi}]");
        }
        // Nothing new: the search doesn't run and isn't reported.
        assert!(run(&index, "Half", 300, 300, ALL).matched.is_empty());
    }

    #[test]
    fn ranges_on_fast_fields_still_apply() {
        // `size:` is lifted into the post-filter (`filter.rs`): sizes are
        // 1000 + position, so ids 1..=5 are 1000..=1004 bytes.
        let (index, _) = index_with(50, 2);
        let p = run(&index, "Common size:<1003B", 0, 50, ALL);
        assert_eq!(ids(&p), [3, 2]);
    }

    #[test]
    fn keep_takes_the_highest_ids() {
        let (index, _) = index_with(100, 3);
        let p = run(
            &index,
            "Half",
            0,
            100,
            PercolateLimits {
                keep: 3,
                max_range: i64::MAX,
            },
        );
        assert_eq!(ids(&p), [99, 97, 95]);
        assert!(p.matched[0].truncated);
        let p = run(&index, "Rare", 0, 100, PercolateLimits { keep: 3, ..ALL });
        assert!(!p.matched[0].truncated);
    }

    #[test]
    fn the_range_cap_leaves_the_rest_for_the_next_run() {
        let (index, _) = index_with(100, 2);
        let limits = PercolateLimits {
            keep: usize::MAX,
            max_range: 30,
        };
        let mut watermark = 0;
        let mut all: Vec<i64> = Vec::new();
        let mut runs = 0;
        loop {
            let p = run(&index, "Half", watermark, 100, limits);
            let Some(m) = p.matched.first() else { break };
            all.extend(&m.torrent_ids);
            watermark = m.through;
            runs += 1;
            assert_eq!(p.more(), watermark < 100);
        }
        assert_eq!(runs, 4);
        all.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(
            all,
            expected("half", 100, 0, 100),
            "nothing lost or repeated"
        );
    }

    #[test]
    fn a_range_stops_at_the_snapshot() {
        let (index, _) = index_with(10, 1);
        // The caller asks for more than the snapshot holds: the search
        // moves only as far as the snapshot reaches.
        let p = run(&index, "Show12", 0, 20, ALL);
        assert_eq!(p.snapshot_max, 10);
        assert_eq!(p.matched[0].through, 10);
        assert!(!p.more());
        append(&index, 11..21, 20);
        let p = run(&index, "Common", 10, 20, ALL);
        assert_eq!(p.snapshot_max, 20);
        assert_eq!(ids(&p).len(), 9);
        // A deleted torrent is simply not matched.
        delete(&index, [19]);
        let p = run(&index, "Common", 10, 20, ALL);
        assert!(!ids(&p).contains(&19));
    }

    #[test]
    fn the_token_is_checked_between_searches() {
        let (index, _) = index_with(50, 1);
        let node = parse("Common").unwrap();
        let due: Vec<Due<'_>> = (1..=3)
            .map(|id| Due {
                id,
                watermark: 0,
                query: &node,
            })
            .collect();
        let token = CancelToken::new();
        token.cancel();
        let p = percolate(&index, &due, 50, ALL, 0, &token).unwrap();
        assert!(p.stopped && p.more() && p.matched.is_empty());
        let spent = CancelToken::new().with_budget(Duration::ZERO);
        assert!(percolate(&index, &due, 50, ALL, 0, &spent).unwrap().stopped);
        let p = percolate(&index, &due, 50, ALL, 0, &CancelToken::new()).unwrap();
        assert_eq!(p.matched.len(), 3);
        assert!(!p.more());
    }

    #[test]
    fn an_empty_index_matches_nothing() {
        let index = SearchIndex::in_memory().unwrap();
        let p = run(&index, "x", 0, 100, ALL);
        assert_eq!(p.snapshot_max, 0);
        assert!(p.matched.is_empty());
    }

    #[test]
    fn top_keeps_the_highest() {
        assert_eq!(top([5, 1, 9, 3], 2), (vec![9, 5], true));
        assert_eq!(top([5, 1], 2), (vec![5, 1], false));
        assert_eq!(top([5], 0), (vec![], true));
        assert_eq!(top(std::iter::empty(), 3), (vec![], false));
    }
}
