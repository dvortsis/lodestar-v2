//! Searcher leases (architecture §8, "Stable pagination"): a lease pins
//! one index snapshot (a Tantivy `Searcher`) with the compiled query and
//! sort, so every page a cursor reads comes from the same documents.
//! Near-real-time commits and an index swap (`rebuild.rs`) then can't
//! cause a duplicate or a gap while someone scrolls.
//!
//! A lease holds the index it was taken from (an `Arc`), the searcher,
//! the compiled query and the count: nothing per hit. [`Leases`] keeps
//! them under random ids for [`LeaseConfig::ttl`] from their last use,
//! bounded in number overall and per client, the least recently used
//! going first. A retired index stays open (and on disk) until its last
//! lease is gone.

use std::cmp::{Ordering as Cmp, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lodestar_store::{Col as HotCol, HotColumns, NEVER};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::{EnableScoring, Weight};
use tantivy::{
    COLLECT_BLOCK_BUFFER_LEN, DocAddress, DocId, DocSet, Score, Searcher, SegmentOrdinal,
    SegmentReader, TERMINATED,
};

use crate::cancel::{self, CHECK_EVERY, CancelToken};
use crate::count::{self, Total};
use crate::cursor::{LeaseId, Position};
use crate::filter::{self, FastRange};
use crate::index::{
    self, Compiled, Error, Hit, RERANK_WINDOW, Result, STEM_FALLBACK_BELOW, ScoreParts,
    SearchIndex, Sort,
};
use crate::query::Node;
use crate::sorted::{self, By, Order};

/// How long an exact count on demand may run before the estimate stands.
pub const EXACT_BUDGET: Duration = Duration::from_secs(5);

/// A pinned snapshot of one search, for paging through it.
pub struct Lease {
    index: Arc<SearchIndex>,
    searcher: Searcher,
    /// The query it was taken for, as given.
    node: Node,
    compiled: Compiled,
    stemmed: bool,
    sort: Sort,
    total: Mutex<Total>,
    /// The time relative dates were resolved against.
    now: i64,
    /// The offset `time:` clauses were resolved in (seconds east of UTC).
    utc_offset: i32,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("sort", &self.sort)
            .field("stemmed", &self.stemmed)
            .field("total", &self.total())
            .finish_non_exhaustive()
    }
}

/// Where a page starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    /// This many hits in (the first pages, and offset paging).
    Offset(usize),
    /// After a cursor's position.
    After(Position),
}

/// One page, and where the next one starts (`None`: this was the last).
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub hits: Vec<Hit>,
    pub next: Option<Position>,
    /// Each hit's score breakdown, when the order is relevance with a
    /// second phase (§8 `explain`); empty otherwise.
    pub explain: Vec<ScoreParts>,
}

/// How a lease's results are ordered.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Phase-1 score only (a query with nothing to re-rank).
    Plain,
    /// Phase-1 windows re-ranked with the name boosts and, with hot
    /// columns, the swarm and freshness priors (§8).
    TwoPhase,
    Field(Order),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Lease {
    /// [`Lease::new_at`] with times of day in UTC.
    pub fn new(
        index: Arc<SearchIndex>,
        node: &Node,
        now: i64,
        sort: Sort,
        cancel: &CancelToken,
    ) -> Result<Lease> {
        Lease::new_at(index, node, now, 0, sort, cancel)
    }

    /// Takes `index`'s current snapshot and compiles `node` for it
    /// (`now` resolves relative dates, once for every page, and
    /// `utc_offset`, seconds east of UTC, times of day). The stem
    /// fallback (§8) is decided here by the count, so every page takes
    /// the same branch; a sort on a field the index lacks becomes
    /// relevance.
    ///
    /// Swarm values (§7) aren't pinned: hot sorts and priors read them as
    /// each page is served, so a torrent scraped between two pages can
    /// move across the cursor.
    pub fn new_at(
        index: Arc<SearchIndex>,
        node: &Node,
        now: i64,
        utc_offset: i32,
        sort: Sort,
        cancel: &CancelToken,
    ) -> Result<Lease> {
        let searcher = index.searcher();
        let exact = index.query_at(node, now, utc_offset);
        let mut total = count::total(&searcher, &exact, cancel)?;
        let (mut compiled, mut stemmed) = (exact, false);
        if total.value < STEM_FALLBACK_BELOW
            && let Some(wide) = index.query_stemmed(node, now, utc_offset)
        {
            let wider = count::total(&searcher, &wide, cancel)?;
            if wider.value > total.value {
                (compiled, stemmed, total) = (wide, true, wider);
            }
        }
        let sort = if index.sort_available(sort) {
            sort
        } else {
            Sort::Relevance
        };
        Ok(Lease {
            index,
            searcher,
            node: node.clone(),
            compiled,
            stemmed,
            sort,
            total: Mutex::new(total),
            now,
            utc_offset,
        })
    }

    /// A lease over `index`'s current snapshot that runs `compiled` as is
    /// (tests: slow or tripwire queries).
    #[cfg(test)]
    pub(crate) fn raw(index: Arc<SearchIndex>, compiled: Compiled, total: Total) -> Lease {
        Lease {
            searcher: index.searcher(),
            index,
            node: crate::Node::All,
            compiled,
            stemmed: false,
            sort: Sort::Relevance,
            total: Mutex::new(total),
            now: 0,
            utc_offset: 0,
        }
    }

    /// The snapshot, for facets over the same documents (`facets.rs`).
    pub(crate) fn searcher(&self) -> &Searcher {
        &self.searcher
    }

    pub(crate) fn compiled(&self) -> &Compiled {
        &self.compiled
    }

    /// The query this lease was taken for.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// The time the query's relative dates resolved against: facets
    /// compile their variants of it at the same instant.
    pub fn now(&self) -> i64 {
        self.now
    }

    /// The offset (seconds east of UTC) `time:` clauses resolved in:
    /// facets compile their variants in it too.
    pub fn utc_offset(&self) -> i32 {
        self.utc_offset
    }

    /// The index the snapshot belongs to (for matched files).
    pub fn index(&self) -> &Arc<SearchIndex> {
        &self.index
    }

    /// The sort in effect.
    pub fn sort(&self) -> Sort {
        self.sort
    }

    pub fn stemmed(&self) -> bool {
        self.stemmed
    }

    /// The count: exact up to [`count::EXACT_UP_TO`], else estimated
    /// unless an exact count was asked for.
    pub fn total(&self) -> Total {
        *lock(&self.total)
    }

    /// Counts every match, under `cancel` and [`EXACT_BUDGET`], and keeps
    /// the result for later pages. Out of time, the estimate stands.
    pub fn exact_total(&self, cancel: &CancelToken) -> Result<Total> {
        self.exact_total_within(cancel, EXACT_BUDGET)
    }

    pub(crate) fn exact_total_within(
        &self,
        cancel: &CancelToken,
        budget: Duration,
    ) -> Result<Total> {
        let now = self.total();
        if !now.approx {
            return Ok(now);
        }
        match count::exact(&self.searcher, &self.compiled, &cancel.with_budget(budget)) {
            Ok(value) => {
                let t = Total {
                    value,
                    approx: false,
                };
                *lock(&self.total) = t;
                Ok(t)
            }
            Err(Error::OutOfTime) => Ok(now),
            Err(e) => Err(e),
        }
    }

    /// The sampled estimate alone, as an index too large to count would
    /// show it, for checking its error (the search bench).
    pub fn sampled_estimate(&self, cancel: &CancelToken) -> Result<Total> {
        count::estimate_with(&self.searcher, &self.compiled, cancel, count::SAMPLE_ABOVE)
    }

    fn mode(&self) -> Mode {
        let (by, desc) = match self.sort {
            Sort::Relevance => {
                return if self.compiled.boosts.is_some() || self.compiled.hot.is_some() {
                    Mode::TwoPhase
                } else {
                    Mode::Plain
                };
            }
            Sort::Seeders { desc } => (By::Hot(HotCol::Seeders), desc),
            Sort::Leechers { desc } => (By::Hot(HotCol::Leechers), desc),
            Sort::Trackers { desc } => (By::Hot(HotCol::Trackers), desc),
            Sort::Seen { desc } => (By::Hot(HotCol::LastSeen), desc),
            Sort::Discovered { desc } | Sort::Size { desc } | Sort::Files { desc } => {
                let (field, signed) = index::sort_field(self.sort).expect("a field sort");
                (By::Fast { field, signed }, desc)
            }
        };
        Mode::Field(Order { by, desc })
    }

    /// Whether `p` is a position in this lease's order: a cursor's
    /// kind matches its lease's sort unless it was forged.
    pub fn fits(&self, p: &Position) -> bool {
        matches!(
            (self.mode(), p),
            (Mode::Plain, Position::Score { .. })
                | (Mode::TwoPhase, Position::Window { .. })
                | (Mode::Field(_), Position::Field { .. })
        )
    }

    /// Up to `limit` hits from `start`, and the position after them.
    pub fn page(&self, start: Start, limit: usize, cancel: &CancelToken) -> Result<Page> {
        Ok(self.page_at(start, limit, cancel)?.0)
    }

    /// [`Lease::page`], with each hit's address (`explain.rs`).
    pub(crate) fn page_at(
        &self,
        start: Start,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<(Page, Vec<DocAddress>)> {
        cancel.check()?;
        let limit = limit.max(1);
        let s = &self.searcher;
        let mut explain = Vec::new();
        let (ranked, next) = match (self.mode(), start) {
            (Mode::Plain, Start::Offset(o)) => {
                let top = score_top(s, &self.compiled, None, o + limit + 1, cancel)?;
                cut_scored(top.into_iter().skip(o).collect(), limit)
            }
            (Mode::Plain, Start::After(Position::Score { score, addr })) => {
                let after = Scored { score, addr };
                cut_scored(
                    score_top(s, &self.compiled, Some(after), limit + 1, cancel)?,
                    limit,
                )
            }
            (Mode::TwoPhase, Start::Offset(o)) => {
                let (ranked, parts, next) = self.windows_from_offset(o, limit, cancel)?;
                explain = parts;
                (ranked, next)
            }
            (Mode::TwoPhase, Start::After(Position::Window { start, last })) => {
                let start = start.map(|(score, addr)| Scored { score, addr });
                let last = last.map(|(score, addr)| Scored { score, addr });
                let (ranked, parts, next) =
                    self.windows(Windows::Live, start, From::After(last), limit, cancel)?;
                explain = parts;
                (ranked, next)
            }
            (Mode::Field(order), Start::Offset(o)) => {
                let cache = self.index.segment_cache();
                let (addrs, _) = sorted::search_cancellable(
                    s,
                    cache,
                    &self.compiled,
                    order,
                    o..o + limit + 1,
                    self.index.sort_piece(),
                    cancel,
                )?;
                let ranks = field_ranks(s, &self.compiled, order, &addrs)?;
                cut_field(ranks, limit)
            }
            (Mode::Field(order), Start::After(Position::Field { present, key, addr })) => {
                let after = FieldRank { present, key, addr };
                let top = field_top(s, &self.compiled, order, Some(after), limit + 1, cancel)?;
                cut_field(top, limit)
            }
            _ => return Err(Error::Corrupt("the cursor doesn't fit its lease")),
        };
        let page = Page {
            hits: hits(s, &ranked)?,
            next,
            explain,
        };
        Ok((page, ranked.iter().map(|r| r.addr).collect()))
    }

    /// The windows covering hits `o..o + limit`, from one phase-1 pass.
    fn windows_from_offset(
        &self,
        o: usize,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<Windowed> {
        let w = RERANK_WINDOW;
        let first = o / w;
        let last = (o + limit - 1) / w;
        // One more than the windows needed: whether a later one exists.
        let cands = score_top(
            &self.searcher,
            &self.compiled,
            None,
            (last + 1) * w + 1,
            cancel,
        )?;
        let start = first
            .checked_sub(1)
            .and_then(|p| cands.get(p * w + w - 1))
            .copied();
        let src = Windows::Fixed { cands, at: first };
        self.windows(src, start, From::Skip(o - first * w), limit, cancel)
    }

    /// Serves `limit` hits window by window, from the window after the
    /// phase-1 bound `start`.
    fn windows(
        &self,
        mut src: Windows,
        mut start: Option<Scored>,
        mut from: From,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<Windowed> {
        let boosts = self.compiled.boosts.as_deref();
        let priors = self.index.priors(self.compiled.now);
        if boosts.is_none() && priors.is_none() {
            return Err(Error::Corrupt("a two-phase lease with nothing to re-rank"));
        }
        let mut out = Vec::with_capacity(limit);
        let mut parts = Vec::with_capacity(limit);
        loop {
            let (cands, more) = match &mut src {
                Windows::Fixed { cands, at } => {
                    let lo = (*at * RERANK_WINDOW).min(cands.len());
                    let hi = (lo + RERANK_WINDOW).min(cands.len());
                    *at += 1;
                    (cands[lo..hi].to_vec(), cands.len() > hi)
                }
                Windows::Live => {
                    let mut c = score_top(
                        &self.searcher,
                        &self.compiled,
                        start,
                        RERANK_WINDOW + 1,
                        cancel,
                    )?;
                    let more = c.len() > RERANK_WINDOW;
                    c.truncate(RERANK_WINDOW);
                    (c, more)
                }
            };
            let Some(&bound) = cands.last() else {
                return Ok((out, parts, None));
            };
            cancel.check()?;
            let reranked = index::rerank(
                &self.searcher,
                boosts,
                priors.as_ref(),
                cands.iter().map(|c| (c.score, c.addr)).collect(),
            )?;
            let ranked: Vec<Scored> = reranked
                .iter()
                .map(|(addr, p)| Scored {
                    score: p.score,
                    addr: *addr,
                })
                .collect();
            let begin = match from {
                From::Skip(n) => n.min(ranked.len()),
                From::After(None) => 0,
                From::After(Some(last)) => ranked
                    .iter()
                    .position(|e| last.before(e))
                    .unwrap_or(ranked.len()),
            };
            for (i, e) in ranked.iter().enumerate().skip(begin) {
                out.push(*e);
                parts.push(reranked[i].1);
                if out.len() == limit {
                    let next = if i + 1 < ranked.len() {
                        Some(Position::Window {
                            start: start.map(Scored::pair),
                            last: Some(e.pair()),
                        })
                    } else if more {
                        Some(Position::Window {
                            start: Some(bound.pair()),
                            last: None,
                        })
                    } else {
                        None
                    };
                    return Ok((out, parts, next));
                }
            }
            if !more {
                return Ok((out, parts, None));
            }
            start = Some(bound);
            from = From::After(None);
        }
    }
}

/// A page of re-ranked hits, their score parts, and where the next starts.
type Windowed = (Vec<Scored>, Vec<ScoreParts>, Option<Position>);

/// Where in a window the page starts.
#[derive(Debug, Clone, Copy)]
enum From {
    /// After this many re-ranked hits.
    Skip(usize),
    /// After this re-ranked hit (`None`: from the top).
    After(Option<Scored>),
}

/// Where windows come from: already fetched (offset paging), or one
/// phase-1 pass each (cursors).
enum Windows {
    Fixed { cands: Vec<Scored>, at: usize },
    Live,
}

/// A hit in relevance order: higher scores first, then lower addresses.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    score: Score,
    addr: DocAddress,
}

impl Scored {
    /// `Greater` when `self` ranks first.
    fn rank(&self, other: &Scored) -> Cmp {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.addr.cmp(&self.addr))
    }

    fn before(&self, other: &Scored) -> bool {
        self.rank(other) == Cmp::Greater
    }

    fn pair(self) -> (f32, DocAddress) {
        (self.score, self.addr)
    }
}

/// [`Scored`] ordered by rank, for heaps.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ByRank(Scored);

impl Eq for ByRank {}

impl PartialOrd for ByRank {
    fn partial_cmp(&self, other: &Self) -> Option<Cmp> {
        Some(self.cmp(other))
    }
}

impl Ord for ByRank {
    fn cmp(&self, other: &Self) -> Cmp {
        self.0.rank(&other.0)
    }
}

fn cut_scored(mut top: Vec<Scored>, limit: usize) -> (Vec<Scored>, Option<Position>) {
    if top.len() <= limit {
        return (top, None);
    }
    top.truncate(limit);
    let next = top.last().map(|s| Position::Score {
        score: s.score,
        addr: s.addr,
    });
    (top, next)
}

/// The best `k` matches by phase-1 score that rank after `after`, best
/// first.
fn score_top(
    searcher: &Searcher,
    compiled: &Compiled,
    after: Option<Scored>,
    k: usize,
    cancel: &CancelToken,
) -> Result<Vec<Scored>> {
    let collector = ScoreTop {
        k,
        after,
        ranges: &compiled.filters,
        hot: compiled.hot.as_ref(),
        cancel,
    };
    searcher
        .search(compiled.query.as_ref(), &collector)
        .map_err(|e| cancel::stopped_or(cancel, e))
}

struct ScoreTop<'a> {
    k: usize,
    after: Option<Scored>,
    ranges: &'a [FastRange],
    hot: Option<&'a Arc<HotColumns>>,
    cancel: &'a CancelToken,
}

struct ScoreTopSegment {
    seg: SegmentOrdinal,
    k: usize,
    after: Option<Scored>,
    checks: Vec<filter::Check>,
    /// The best `k` so far, worst on top.
    heap: BinaryHeap<Reverse<ByRank>>,
}

impl ScoreTopSegment {
    fn offer(&mut self, doc: DocId, score: Score) {
        let e = Scored {
            score,
            addr: DocAddress::new(self.seg, doc),
        };
        if self.after.is_some_and(|a| !a.before(&e)) {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(Reverse(ByRank(e)));
        } else if self.heap.peek().is_some_and(|w| e.before(&w.0.0)) {
            self.heap.pop();
            self.heap.push(Reverse(ByRank(e)));
        }
    }
}

impl SegmentCollector for ScoreTopSegment {
    type Fruit = Vec<Scored>;

    fn collect(&mut self, doc: DocId, score: Score) {
        if self.checks.iter().all(|c| c.passes(doc)) {
            self.offer(doc, score);
        }
    }

    fn harvest(self) -> Vec<Scored> {
        self.heap.into_iter().map(|r| r.0.0).collect()
    }
}

impl Collector for ScoreTop<'_> {
    type Fruit = Vec<Scored>;
    type Child = ScoreTopSegment;

    fn for_segment(
        &self,
        seg: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(ScoreTopSegment {
            seg,
            k: self.k,
            after: self.after,
            checks: filter::checks(self.ranges, reader, self.hot)?,
            heap: BinaryHeap::with_capacity(self.k.min(4096) + 1),
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(&self, fruits: Vec<Vec<Scored>>) -> tantivy::Result<Vec<Scored>> {
        let mut all = fruits.concat();
        all.sort_unstable_by(|a, b| b.rank(a));
        all.truncate(self.k);
        Ok(all)
    }

    /// Tantivy's loop, with the range checks before scoring and the
    /// token checked as it goes.
    fn collect_segment(
        &self,
        weight: &dyn Weight,
        seg: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Vec<Scored>> {
        if self.cancel.is_stopped() {
            return Err(cancel::interrupted());
        }
        let mut child = self.for_segment(seg, reader)?;
        let mut scorer = weight.scorer(reader, 1.0)?;
        let alive = reader.alive_bitset();
        let mut since = 0;
        let mut doc = scorer.doc();
        while doc != TERMINATED {
            if alive.is_none_or(|a| a.is_alive(doc)) && child.checks.iter().all(|c| c.passes(doc)) {
                child.offer(doc, scorer.score());
            }
            since += 1;
            if since == CHECK_EVERY {
                since = 0;
                if self.cancel.is_stopped() {
                    return Err(cancel::interrupted());
                }
            }
            doc = scorer.advance();
        }
        Ok(child.harvest())
    }
}

/// A hit in a fast-field order, as `sorted.rs` ranks it: documents with a
/// value first, then by the ordered key, then lower addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FieldRank {
    present: bool,
    key: u64,
    addr: DocAddress,
}

impl PartialOrd for FieldRank {
    fn partial_cmp(&self, other: &Self) -> Option<Cmp> {
        Some(self.cmp(other))
    }
}

/// `Greater` ranks first.
impl Ord for FieldRank {
    fn cmp(&self, other: &Self) -> Cmp {
        (self.present, self.key)
            .cmp(&(other.present, other.key))
            .then_with(|| other.addr.cmp(&self.addr))
    }
}

/// Reads one segment's sort values the way `sorted::segment_top` does.
struct Values {
    col: Col,
    order: Order,
}

enum Col {
    I64(tantivy::columnar::Column<i64>),
    U64(tantivy::columnar::Column<u64>),
    /// The id column, read through the hot columns (`None`: no values).
    Hot(
        tantivy::columnar::Column<u64>,
        Option<Arc<HotColumns>>,
        HotCol,
    ),
}

impl Values {
    fn new(
        reader: &SegmentReader,
        order: Order,
        hot: Option<&Arc<HotColumns>>,
    ) -> tantivy::Result<Values> {
        let fast = reader.fast_fields();
        let col = match order.by {
            By::Fast {
                field,
                signed: true,
            } => Col::I64(fast.i64(field)?),
            By::Fast {
                field,
                signed: false,
            } => Col::U64(fast.u64(field)?),
            By::Hot(c) => Col::Hot(fast.u64("id")?, hot.cloned(), c),
        };
        Ok(Values { col, order })
    }

    fn rank(&self, addr: DocAddress) -> FieldRank {
        let v = match &self.col {
            Col::I64(c) => c.first(addr.doc_id).map(|v| v as u64),
            Col::U64(c) => c.first(addr.doc_id),
            Col::Hot(ids, hot, c) => hot.as_ref().and_then(|hot| {
                let v = hot.get(*c, ids.first(addr.doc_id)?);
                (v != NEVER).then_some(u64::from(v))
            }),
        };
        let signed = matches!(self.order.by, By::Fast { signed: true, .. });
        let mut key = v.map_or(0, |v| sorted::sortable(v, signed));
        if !self.order.desc {
            key = !key;
        }
        FieldRank {
            present: v.is_some(),
            key,
            addr,
        }
    }
}

fn field_ranks(
    searcher: &Searcher,
    compiled: &Compiled,
    order: Order,
    addrs: &[DocAddress],
) -> Result<Vec<FieldRank>> {
    let mut values: HashMap<u32, Values> = HashMap::new();
    let mut out = Vec::with_capacity(addrs.len());
    for &a in addrs {
        let v = match values.entry(a.segment_ord) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(Values::new(
                searcher.segment_reader(a.segment_ord),
                order,
                compiled.hot.as_ref(),
            )?),
        };
        out.push(v.rank(a));
    }
    Ok(out)
}

fn cut_field(mut top: Vec<FieldRank>, limit: usize) -> (Vec<Scored>, Option<Position>) {
    let next = if top.len() > limit {
        top.truncate(limit);
        top.last().map(|r| Position::Field {
            present: r.present,
            key: r.key,
            addr: r.addr,
        })
    } else {
        None
    };
    let hits = top
        .into_iter()
        .map(|r| Scored {
            score: 0.0,
            addr: r.addr,
        })
        .collect();
    (hits, next)
}

/// The best `k` matches in `order` that rank after `after`, best first:
/// `sorted::search`'s order without its cache (each cursor page is a
/// different bound).
fn field_top(
    searcher: &Searcher,
    compiled: &Compiled,
    order: Order,
    after: Option<FieldRank>,
    k: usize,
    cancel: &CancelToken,
) -> Result<Vec<FieldRank>> {
    cancel.check()?;
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let segments = searcher.segment_readers().iter().enumerate();
    let tops = searcher.index().search_executor().map(
        |(ord, reader): (usize, &SegmentReader)| {
            if cancel.is_stopped() {
                return Err(cancel::interrupted());
            }
            let values = Values::new(reader, order, compiled.hot.as_ref())?;
            let checks = filter::checks(&compiled.filters, reader, compiled.hot.as_ref())?;
            let alive = reader.alive_bitset();
            let mut heap: BinaryHeap<Reverse<FieldRank>> = BinaryHeap::with_capacity(k + 1);
            let mut docs = weight.scorer(reader, 1.0)?;
            let mut buf = [0; COLLECT_BLOCK_BUFFER_LEN];
            let mut since = 0;
            loop {
                let n = docs.fill_buffer(&mut buf);
                for &doc in &buf[..n] {
                    if alive.is_some_and(|a| !a.is_alive(doc))
                        || !checks.iter().all(|c| c.passes(doc))
                    {
                        continue;
                    }
                    let r = values.rank(DocAddress::new(ord as u32, doc));
                    if after.is_some_and(|a| r >= a) {
                        continue;
                    }
                    if heap.len() < k {
                        heap.push(Reverse(r));
                    } else if heap.peek().is_some_and(|w| r > w.0) {
                        heap.pop();
                        heap.push(Reverse(r));
                    }
                }
                since += n as u32;
                if since >= CHECK_EVERY {
                    since = 0;
                    if cancel.is_stopped() {
                        return Err(cancel::interrupted());
                    }
                }
                if n < buf.len() {
                    break;
                }
            }
            Ok(heap.into_iter().map(|r| r.0).collect::<Vec<_>>())
        },
        segments,
    );
    let mut all: Vec<FieldRank> = tops.map_err(|e| cancel::stopped_or(cancel, e))?.concat();
    all.sort_unstable_by(|a, b| b.cmp(a));
    all.truncate(k);
    Ok(all)
}

/// Torrent ids from the `id` fast field (the docstore is only for names).
fn hits(searcher: &Searcher, ranked: &[Scored]) -> Result<Vec<Hit>> {
    let mut columns = HashMap::new();
    let mut out = Vec::with_capacity(ranked.len());
    for s in ranked {
        let col = match columns.entry(s.addr.segment_ord) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(
                searcher
                    .segment_reader(s.addr.segment_ord)
                    .fast_fields()
                    .u64("id")?,
            ),
        };
        let id = col
            .first(s.addr.doc_id)
            .ok_or(Error::Corrupt("document without an id"))?;
        out.push(Hit { id, score: s.score });
    }
    Ok(out)
}

/// Bounds on [`Leases`].
#[derive(Debug, Clone)]
pub struct LeaseConfig {
    /// A lease expires this long after its last use (§8: 10 minutes).
    pub ttl: Duration,
    /// And this long after it was taken, however busy: a retired index
    /// is deleted at most this long after a swap.
    pub max_age: Duration,
    /// Leases kept in all; past it the least recently used goes.
    pub max_leases: usize,
    /// Leases kept per client (session); past it, that client's least
    /// recently used goes.
    pub per_client: usize,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        LeaseConfig {
            ttl: Duration::from_secs(600),
            max_age: Duration::from_secs(3600),
            max_leases: 1024,
            per_client: 64,
        }
    }
}

struct Slot {
    lease: Arc<Lease>,
    client: u64,
    taken: Instant,
    used: Instant,
}

/// The live leases.
pub struct Leases {
    config: LeaseConfig,
    map: Mutex<HashMap<LeaseId, Slot>>,
    /// Keyed by the OS's randomness, so ids can't be predicted from one
    /// another; cursors are authenticated anyway (the API's HMAC).
    keys: RandomState,
    counter: AtomicU64,
}

impl std::fmt::Debug for Leases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Leases")
            .field("config", &self.config)
            .field("live", &self.len())
            .finish()
    }
}

impl Default for Leases {
    fn default() -> Self {
        Leases::new(LeaseConfig::default())
    }
}

impl Leases {
    pub fn new(config: LeaseConfig) -> Leases {
        Leases {
            config,
            map: Mutex::default(),
            keys: RandomState::new(),
            counter: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &LeaseConfig {
        &self.config
    }

    fn expired(&self, s: &Slot, now: Instant) -> bool {
        now.duration_since(s.used) >= self.config.ttl
            || now.duration_since(s.taken) >= self.config.max_age
    }

    fn new_id(&self) -> LeaseId {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let a = self.keys.hash_one((n, 0u8)).to_le_bytes();
        let b = self.keys.hash_one((n, 1u8)).to_le_bytes();
        let mut id = [0; 16];
        id[..8].copy_from_slice(&a);
        id[8..].copy_from_slice(&b);
        LeaseId(id)
    }

    /// Keeps `lease` for `client`, making room first: expired leases go,
    /// then the client's least recently used past its cap, then the least
    /// recently used of all past the total cap.
    pub fn insert(&self, lease: Arc<Lease>, client: u64) -> LeaseId {
        self.insert_at(lease, client, Instant::now())
    }

    /// [`Leases::insert`] at `now` (tests pass their own clock).
    pub(crate) fn insert_at(&self, lease: Arc<Lease>, client: u64, now: Instant) -> LeaseId {
        // Dropped after the lock: the last lease on a retired index
        // unmaps its files, and no search should wait behind that.
        let mut gone = Vec::new();
        let mut map = lock(&self.map);
        gone.extend(self.take_expired(&mut map, now));
        let lru = |map: &HashMap<LeaseId, Slot>, of: Option<u64>| {
            map.iter()
                .filter(|(_, s)| of.is_none_or(|c| s.client == c))
                .min_by_key(|(_, s)| s.used)
                .map(|(id, _)| *id)
        };
        if map.values().filter(|s| s.client == client).count() >= self.config.per_client.max(1)
            && let Some(id) = lru(&map, Some(client))
        {
            gone.extend(map.remove(&id));
        }
        if map.len() >= self.config.max_leases.max(1)
            && let Some(id) = lru(&map, None)
        {
            gone.extend(map.remove(&id));
        }
        let id = loop {
            let id = self.new_id();
            if !map.contains_key(&id) {
                break id;
            }
        };
        map.insert(
            id,
            Slot {
                lease,
                client,
                taken: now,
                used: now,
            },
        );
        drop(map);
        drop(gone);
        id
    }

    /// `client`'s lease `id`, refreshed. An expired, evicted, unknown or
    /// someone else's lease is [`Error::LeaseExpired`].
    pub fn get(&self, id: &LeaseId, client: u64) -> Result<Arc<Lease>> {
        self.get_at(id, client, Instant::now())
    }

    pub(crate) fn get_at(&self, id: &LeaseId, client: u64, now: Instant) -> Result<Arc<Lease>> {
        let mut map = lock(&self.map);
        let Some(slot) = map.get_mut(id) else {
            return Err(Error::LeaseExpired);
        };
        if self.expired(slot, now) {
            let gone = map.remove(id);
            drop(map);
            drop(gone);
            return Err(Error::LeaseExpired);
        }
        if slot.client != client {
            return Err(Error::LeaseExpired);
        }
        slot.used = now;
        Ok(slot.lease.clone())
    }

    /// Removes the expired leases, for the caller to drop once it has
    /// let go of the lock.
    fn take_expired(&self, map: &mut HashMap<LeaseId, Slot>, now: Instant) -> Vec<Slot> {
        let ids: Vec<LeaseId> = map
            .iter()
            .filter(|(_, s)| self.expired(s, now))
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| map.remove(id)).collect()
    }

    /// Drops expired leases; returns how many are left.
    pub fn sweep(&self) -> usize {
        self.sweep_at(Instant::now())
    }

    pub(crate) fn sweep_at(&self, now: Instant) -> usize {
        let mut map = lock(&self.map);
        let gone = self.take_expired(&mut map, now);
        let left = map.len();
        drop(map);
        drop(gone);
        left
    }

    pub fn len(&self) -> usize {
        lock(&self.map).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests;
