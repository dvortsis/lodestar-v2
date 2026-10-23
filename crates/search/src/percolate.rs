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
//! - **Cost tracks the new data, not the index:** only segments whose
//!   ids can fall in a search's range are collected (by the `id`
//!   column's minimum and maximum), so a run over the last commit reads
//!   the newest segments, not the whole corpus.
//! - **Bounded:** each search covers at most `max_range` ids per run and
//!   keeps at most `keep` hits (the highest ids). The token is checked
//!   between searches and between segments, and, once a search has
//!   completed in the run, every few thousand documents inside a scan
//!   too, so one heavy search can't overrun the budget by much. A run's
//!   first search is never stopped inside its scan, so a run that starts
//!   one always finishes one. A search the token stops is reported as
//!   not run; what a run didn't reach runs next time.
//!
//! No store writes here: the caller records the result in one
//! transaction per range ([`lodestar_store::Store::record_percolation`]).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::{BooleanQuery, EnableScoring, Occur, Query, RangeQuery};
use tantivy::{DocId, Score, Searcher, SegmentOrdinal, SegmentReader, Term};

use crate::cancel::{CHECK_EVERY, CancelToken};
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
        let stopped = Arc::new(AtomicBool::new(false));
        let collector = PostFilter {
            ranges: &compiled.filters,
            // Swarm clauses read the hot columns as search does; a torrent
            // just indexed is usually not scraped yet, so it matches no
            // swarm range until it is (M4).
            hot: compiled.hot.as_ref(),
            inner: TopIds {
                keep: limits.keep,
                // The first search of a run always completes.
                cancel: (!out.matched.is_empty()).then(|| cancel.clone()),
                stopped: stopped.clone(),
            },
        };
        match collect(&searcher, &q, &collector, (d.watermark, end), cancel) {
            Ok(_) if stopped.load(Ordering::Relaxed) => {
                out.stopped = true;
                break;
            }
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
            Err(_) => {
                // The error isn't logged: Tantivy's can carry query terms.
                lodestar_log::log_event!(
                    crate::log_events::PERCOLATE_SEARCH_FAILED,
                    saved_search = lodestar_log::Count(d.id.unsigned_abs())
                );
                out.failed.push(d.id);
            }
        }
    }
    Ok(out)
}

/// Runs `q` over the segments that can hold an id in `(after, upto]`,
/// skipping the rest by their `id` column's range. A token that stops
/// between segments marks the collector stopped.
fn collect(
    searcher: &Searcher,
    q: &dyn Query,
    collector: &PostFilter<'_, TopIds>,
    (after, upto): (i64, i64),
    cancel: &CancelToken,
) -> Result<(Vec<i64>, bool)> {
    let weight = q.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut fruits = Vec::new();
    for (ord, seg) in searcher.segment_readers().iter().enumerate() {
        if !may_hold(seg, after, upto)? {
            continue;
        }
        if collector.inner.cancel.is_some() && cancel.is_stopped() {
            collector.inner.stopped.store(true, Ordering::Relaxed);
            break;
        }
        let ord = SegmentOrdinal::try_from(ord).unwrap_or(SegmentOrdinal::MAX);
        fruits.push(collector.collect_segment(weight.as_ref(), ord, seg)?);
    }
    Ok(collector.merge_fruits(fruits)?)
}

/// Whether `seg` may hold a live document with an id in `(after, upto]`.
fn may_hold(seg: &SegmentReader, after: i64, upto: i64) -> Result<bool> {
    if seg.num_docs() == 0 {
        return Ok(false);
    }
    let col = seg.fast_fields().u64("id")?;
    let (lo, hi) = (col.min_value(), col.max_value());
    let after = u64::try_from(after).unwrap_or(0);
    let upto = u64::try_from(upto).unwrap_or(0);
    Ok(hi > after && lo <= upto)
}

/// Collects the `keep` highest torrent ids, highest first, and whether
/// any match was left out. With a token, it stops collecting once the
/// token does (checked every [`CHECK_EVERY`] documents) and sets
/// `stopped`: the result is then incomplete and isn't used.
struct TopIds {
    keep: usize,
    cancel: Option<CancelToken>,
    stopped: Arc<AtomicBool>,
}

struct TopIdsSegment {
    col: tantivy::columnar::Column<u64>,
    keep: usize,
    /// A min-heap of the highest ids so far.
    heap: BinaryHeap<Reverse<u64>>,
    over: bool,
    cancel: Option<CancelToken>,
    stopped: Arc<AtomicBool>,
    since_check: u32,
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
            cancel: self.cancel.clone(),
            stopped: self.stopped.clone(),
            since_check: 0,
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
        if let Some(cancel) = &self.cancel {
            if self.stopped.load(Ordering::Relaxed) {
                return;
            }
            self.since_check += 1;
            if self.since_check >= CHECK_EVERY {
                self.since_check = 0;
                if cancel.is_stopped() {
                    self.stopped.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
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

    /// Only the segments that can hold the range are read.
    #[test]
    fn segments_outside_the_range_are_skipped() {
        // Ids 1-100, 101-200 and 201-300, one segment each.
        let (index, _) = index_with(300, 3);
        let searcher = index.searcher();
        let segs = searcher.segment_readers();
        let held = |after, upto| -> Vec<bool> {
            segs.iter()
                .map(|s| may_hold(s, after, upto).unwrap())
                .collect()
        };
        let want: Vec<Vec<bool>> = vec![
            vec![true, true, true],
            vec![false, false, true],
            vec![false, true, true],
            vec![true, false, false],
            vec![false, false, false],
        ];
        let mut got = vec![
            held(0, 300),
            held(200, 300),
            held(150, 201),
            held(0, 100),
            held(300, 400),
        ];
        // Segment order isn't id order; compare by each segment's ids.
        let order: Vec<u64> = segs
            .iter()
            .map(|s| s.fast_fields().u64("id").unwrap().min_value())
            .collect();
        let mut idx: Vec<usize> = (0..3).collect();
        idx.sort_by_key(|&i| order[i]);
        for g in &mut got {
            *g = idx.iter().map(|&i| g[i]).collect();
        }
        assert_eq!(got, want);
        // A segment whose documents are all deleted holds nothing.
        delete(&index, 201..=300);
        let searcher = index.searcher();
        assert!(
            searcher
                .segment_readers()
                .iter()
                .filter(|s| s.num_docs() > 0)
                .count()
                <= 2
        );
        // Results are what a full scan gives.
        let p = run(&index, "Half", 150, 300, ALL);
        assert_eq!(ids(&p), expected("half", 200, 150, 200));
    }

    /// Inside a scan, a stopped token stops collection within
    /// `CHECK_EVERY` documents and marks the result unusable.
    #[test]
    fn a_scan_stops_once_its_token_does() {
        let (index, _) = index_with(10_000, 1);
        let searcher = index.searcher();
        let token = CancelToken::new();
        token.cancel();
        let stopped = Arc::new(AtomicBool::new(false));
        let collector = TopIds {
            keep: usize::MAX,
            cancel: Some(token),
            stopped: stopped.clone(),
        };
        let all = tantivy::query::AllQuery;
        let weight = all
            .weight(EnableScoring::disabled_from_searcher(&searcher))
            .unwrap();
        let seg = &searcher.segment_readers()[0];
        let (ids, _) = collector.collect_segment(weight.as_ref(), 0, seg).unwrap();
        assert!(stopped.load(Ordering::Relaxed));
        assert_eq!(ids.len(), CHECK_EVERY as usize - 1);
        // Without a token the same scan collects every document.
        let free = TopIds {
            keep: usize::MAX,
            cancel: None,
            stopped: Arc::default(),
        };
        let (ids, _) = free.collect_segment(weight.as_ref(), 0, seg).unwrap();
        assert!(seg.num_docs() > CHECK_EVERY);
        assert_eq!(ids.len(), seg.num_docs() as usize);
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
