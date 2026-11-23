//! Regex search's back half (architecture §8 and §3, plan 2026-11-05
//! RX-1): a `/…/` clause runs its trigram [`Plan`] on `name_trigram` as a
//! prefilter, then verifies each candidate's stored `name` with `regex`,
//! under a hard time budget.
//!
//! - **The prefilter** is `marker OR plan` ([`crate::doc::TRIGRAM_MARKER`]):
//!   the field holds the grams of a name's first
//!   [`crate::doc::TRIGRAM_PREFIX_BYTES`] only, so a longer name always
//!   reaches the verifier. It runs only when the field is complete in
//!   the snapshot (`Cx::trigrams`); otherwise every name is verified.
//! - **The verifier** compiles [`Pattern::verify_pattern`] with Unicode
//!   on, `regex`'s size and lazy-DFA limits spelled out
//!   ([`SIZE_LIMIT`], [`DFA_SIZE_LIMIT`]), so it matches exactly what was
//!   planned. A pattern `regex` still refuses matches nothing.
//! - **The budget** (S8-11). Every clause of one search verifies against
//!   one deadline, [`VERIFY_BUDGET`] from the search's first regex
//!   compile ([`crate::CancelToken::regex_deadline`]), so planning and
//!   compiling count too, and so do the search's facet variants and
//!   recovery passes. A query holds at most
//!   [`crate::query::MAX_REGEX_CLAUSES`] clauses, so one search plans and
//!   compiles at most that many patterns, each bounded by RX-0's limits.
//! - **Verifying.** [`seal`] verifies every segment of a searcher under
//!   the *pass's* cancel token, before the pass runs; a pass that didn't
//!   seal verifies a segment when it first reaches it, under the deadline
//!   only (never under the token of the request that compiled the query,
//!   which may be long gone). Each segment's matches are kept (a sorted
//!   id list, or a bitset when that's smaller), so every pass over one
//!   compiled query sees one answer. A stop leaves the clause **partial**
//!   ([`stopped`]): what was verified, never a silent truncation, for the
//!   UI's "Stopped after 2 s" with a ≈ count.
//! - **A lease** seals its snapshot and then [`release`]s the verifiers
//!   and prefilters: it keeps only the verified matches, whose size
//!   [`retained_bytes`] reports for the lease limits (S8-10).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::regex::{Regex, RegexBuilder};
use tantivy::index::SegmentId;
use tantivy::query::{
    BooleanQuery, ConstScorer, EmptyScorer, EnableScoring, Explanation, Occur, Query, Scorer,
    TermQuery, Weight,
};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{
    BitSet, DocId, DocSet, Score, Searcher, SegmentReader, TERMINATED, TantivyDocument, Term,
};

use crate::CancelToken;
use crate::regex::{Pattern, Plan};

/// How long one search's regex clauses may plan, compile and verify,
/// together (§3: p95 < 500 ms; the UI says "Stopped after 2 s"). Past it
/// the clauses not done are partial.
pub const VERIFY_BUDGET: Duration = Duration::from_secs(2);
/// The budget a search's first regex compile starts
/// ([`VERIFY_BUDGET`], or a shorter one tests set on their thread with
/// [`set_budget_on_this_thread`]).
pub(crate) fn budget() -> Duration {
    BUDGET.with(std::cell::Cell::get).unwrap_or(VERIFY_BUDGET)
}

thread_local! {
    static BUDGET: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    static CANCEL_AT_SEAL: std::cell::RefCell<Option<CancelToken>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    static STOP_AT_SEAL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// For tests only: after `skip` more [`seal`]s on this thread, the next
/// one finds its budget spent, as a search's last pass would after its
/// earlier ones used it up: what it would verify stays unverified, and
/// its clauses stop. Clauses already verified (shared ones) are kept.
#[doc(hidden)]
pub fn stop_at_seal_on_this_thread(skip: usize) {
    STOP_AT_SEAL.with(|c| c.set(Some(skip)));
}

/// For tests only: cancel `token` when the next [`seal`] on this thread
/// starts, as a run's budget running out mid-verify would (`api`'s live
/// queries).
#[doc(hidden)]
pub fn cancel_at_next_seal_on_this_thread(token: CancelToken) {
    CANCEL_AT_SEAL.with(|t| *t.borrow_mut() = Some(token));
}

/// For tests only (no production caller), here and in the crates that
/// percolate (`lodestar`): the budget searches compiled on this thread
/// start, `None` for [`VERIFY_BUDGET`]. A `Duration::ZERO` makes every
/// verify stop.
#[doc(hidden)]
pub fn set_budget_on_this_thread(budget: Option<Duration>) {
    BUDGET.with(|b| b.set(budget));
}

/// `regex`'s compiled-program limit, its default spelled out: the planner's
/// estimate ([`crate::regex::MAX_COMPILED_WEIGHT`]) refuses most patterns
/// over it first; this is the backstop.
pub const SIZE_LIMIT: usize = 10 << 20;
/// `regex`'s lazy-DFA cache limit, its default spelled out. Past it the
/// engine falls back to a slower one; it never grows past it.
pub const DFA_SIZE_LIMIT: usize = 2 << 20;
/// Candidates verified between two looks at the clock and the token.
const CHECK_EVERY: u32 = 64;
/// Docstore blocks each segment's verifier keeps decompressed: names are
/// read in document order, so one is enough to read a block once; a few
/// more cost little.
const STORE_BLOCKS: usize = 4;
/// What [`retained_bytes`] charges a clause, and each segment it holds
/// matches for, beyond the matches themselves.
const CLAUSE_OVERHEAD: usize = 256;
const SEGMENT_OVERHEAD: usize = 64;

type Q = Box<dyn Query>;

/// A pattern's verifier: `None` when `regex` refuses it (its own size
/// limit), which then matches nothing.
pub(crate) fn verifier(pattern: &Pattern) -> Option<Regex> {
    RegexBuilder::new(&pattern.verify_pattern())
        .unicode(true)
        .size_limit(SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
        .ok()
}

/// The plan as a query on `field`; `None` for [`Plan::All`] (no
/// constraint). Plans are at most [`crate::regex::MAX_PLAN_NODES`] nodes,
/// so the recursion is bounded.
pub(crate) fn plan_query(plan: &Plan, field: Field) -> Option<Q> {
    Some(match plan {
        Plan::All => return None,
        Plan::Nothing => Box::new(tantivy::query::EmptyQuery),
        Plan::Gram(g) => Box::new(TermQuery::new(
            Term::from_field_text(field, g),
            IndexRecordOption::Basic,
        )),
        Plan::And(kids) | Plan::Or(kids) => {
            let occur = if matches!(plan, Plan::And(_)) {
                Occur::Must
            } else {
                Occur::Should
            };
            let mut clauses = Vec::with_capacity(kids.len());
            for k in kids {
                match plan_query(k, field) {
                    Some(q) => clauses.push((occur, q)),
                    // An OR with an unconstrained child is unconstrained;
                    // an AND just drops it. (The planner never nests
                    // `All`, but a stored plan isn't trusted to.)
                    None if occur == Occur::Should => return None,
                    None => {}
                }
            }
            if clauses.is_empty() {
                return None;
            }
            Box::new(BooleanQuery::new(clauses))
        }
    })
}

/// A `/…/` clause: the names `pattern` matches, the candidates narrowed
/// by `prefilter` when there is one.
#[derive(Clone)]
pub(crate) struct RegexQuery {
    source: String,
    name: Field,
    trigram: Option<Field>,
    state: Arc<State>,
}

/// The verifier and the prefilter, built on first use: the compiler may
/// build a clause it then drops, and a pattern costs milliseconds to
/// plan and compile.
struct Prepared {
    /// `None` when the pattern is refused (a stored query may hold one):
    /// it matches nothing.
    verifier: Option<Regex>,
    /// `None`: verify every name.
    prefilter: Option<Q>,
}

enum Prep {
    Unbuilt,
    Ready(Arc<Prepared>),
    /// Dropped by [`release`]: every segment the clause will see is
    /// verified.
    Released,
}

/// One segment's verified matches, in the smaller of two forms.
enum Matches {
    /// Ascending ids: 4 bytes a match.
    Sparse(Vec<DocId>),
    /// A bit per document: `max_doc / 8` bytes.
    Dense(BitSet),
}

impl Matches {
    /// Adds `doc` (ascending, below `max_doc`), switching to a bitset
    /// once the ids would take more room than one.
    fn insert(&mut self, doc: DocId, max_doc: DocId) {
        match self {
            Matches::Sparse(ids) => {
                let dense = (max_doc as usize).div_ceil(64) * 8;
                if (ids.len() + 1) * 4 > dense {
                    let mut set = BitSet::with_max_value(max_doc);
                    for id in ids.iter() {
                        set.insert(*id);
                    }
                    set.insert(doc);
                    *self = Matches::Dense(set);
                } else {
                    ids.push(doc);
                }
            }
            Matches::Dense(set) => {
                set.insert(doc);
            }
        }
    }

    fn bytes(&self) -> usize {
        SEGMENT_OVERHEAD
            + match self {
                Matches::Sparse(ids) => ids.capacity() * 4,
                Matches::Dense(set) => (set.max_value() as usize).div_ceil(64) * 8,
            }
    }

    fn is_empty(&self) -> bool {
        match self {
            Matches::Sparse(ids) => ids.is_empty(),
            Matches::Dense(set) => set.len() == 0,
        }
    }
}

/// What every pass over one clause shares.
struct State {
    /// The search's regex deadline ([`crate::CancelToken::regex_deadline`]).
    deadline: Instant,
    prepared: Mutex<Prep>,
    partial: AtomicBool,
    /// Each segment's verified matches.
    done: Mutex<HashMap<SegmentId, Arc<Matches>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl std::fmt::Debug for RegexQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegexQuery")
            .field("pattern", &self.source)
            .field("prefilter", &self.trigram.is_some())
            .finish()
    }
}

impl RegexQuery {
    /// The clause for `pattern` on the stored `name` field, verifying
    /// until `deadline`. `trigram` is `name_trigram` when it's complete
    /// in the snapshot searched (a prefilter on a field still being
    /// filled would drop names it hasn't reached), else `None`: every
    /// name is then verified.
    pub(crate) fn new(
        pattern: &str,
        name: Field,
        trigram: Option<Field>,
        deadline: Instant,
    ) -> RegexQuery {
        RegexQuery {
            source: pattern.to_owned(),
            name,
            trigram,
            state: Arc::new(State {
                deadline,
                prepared: Mutex::new(Prep::Unbuilt),
                partial: AtomicBool::new(false),
                done: Mutex::default(),
            }),
        }
    }

    fn out_of_time(&self, cancel: Option<&CancelToken>) -> bool {
        Instant::now() >= self.state.deadline || cancel.is_some_and(CancelToken::is_stopped)
    }

    fn stop(&self) {
        self.state.partial.store(true, Ordering::Relaxed);
    }

    /// The verifier and prefilter; `None` once [`release`]d.
    fn prepared(&self) -> Option<Arc<Prepared>> {
        let mut prep = lock(&self.state.prepared);
        match &*prep {
            Prep::Ready(p) => return Some(p.clone()),
            Prep::Released => return None,
            Prep::Unbuilt => {}
        }
        let built = Arc::new(self.build());
        *prep = Prep::Ready(built.clone());
        Some(built)
    }

    fn build(&self) -> Prepared {
        let nothing = || Prepared {
            verifier: None,
            prefilter: Some(Box::new(tantivy::query::EmptyQuery) as Q),
        };
        // Out of time before planning: nothing is planned or compiled,
        // and the clause is partial.
        if self.out_of_time(None) {
            self.stop();
            return nothing();
        }
        // Checked again here: the node may not come from the parser.
        let Ok(pattern) = crate::regex::parse(&self.source) else {
            return nothing();
        };
        let prefilter = match (pattern.plan(), self.trigram) {
            // Nothing can match: no candidate at all.
            (Plan::Nothing, _) => Some(Box::new(tantivy::query::EmptyQuery) as Q),
            (plan, Some(field)) => plan_query(plan, field).map(|q| {
                let marker: Q = Box::new(TermQuery::new(
                    Term::from_field_text(field, crate::doc::TRIGRAM_MARKER),
                    IndexRecordOption::Basic,
                ));
                Box::new(BooleanQuery::new(vec![
                    (Occur::Should, marker),
                    (Occur::Should, q),
                ])) as Q
            }),
            (_, None) => None,
        };
        Prepared {
            verifier: verifier(&pattern),
            prefilter,
        }
    }

    /// Whether the budget or a pass's cancel token stopped this clause
    /// before it had verified every candidate.
    pub(crate) fn stopped(&self) -> bool {
        self.state.partial.load(Ordering::Relaxed)
    }

    fn prefilter_weight(
        &self,
        prepared: Option<&Prepared>,
        scoring: EnableScoring<'_>,
    ) -> tantivy::Result<Option<Box<dyn Weight>>> {
        // The prefilter only selects: no scores.
        let unscored = match scoring.searcher() {
            Some(s) => EnableScoring::disabled_from_searcher(s),
            None => EnableScoring::disabled_from_schema(scoring.schema()),
        };
        prepared
            .and_then(|p| p.prefilter.as_ref())
            .map(|q| q.weight(unscored))
            .transpose()
    }

    /// This segment's matches, verified now (under `cancel`, when the
    /// pass gives one) or by an earlier pass.
    fn matches(
        &self,
        prepared: Option<&Prepared>,
        prefilter: Option<&dyn Weight>,
        reader: &SegmentReader,
        cancel: Option<&CancelToken>,
    ) -> tantivy::Result<Arc<Matches>> {
        let id = reader.segment_id();
        if let Some(set) = lock(&self.state.done).get(&id).cloned() {
            return Ok(set);
        }
        let set = Arc::new(match prepared {
            Some(p) => self.verify(p, prefilter, reader, cancel)?,
            // Released, yet a segment wasn't sealed: never, since a lease
            // seals its whole snapshot. Reported as a stop, not answered
            // as "nothing".
            None => {
                debug_assert!(false, "a released regex clause met a new segment");
                self.stop();
                Matches::Sparse(Vec::new())
            }
        });
        // Two passes may race on one segment, such as an unsealed sorted
        // search's parallel pieces of it: both verify (accepted: it only
        // happens outside a lease, within one budget), and the first kept
        // wins, so every pass sees one answer.
        Ok(lock(&self.state.done).entry(id).or_insert(set).clone())
    }

    fn verify(
        &self,
        prepared: &Prepared,
        prefilter: Option<&dyn Weight>,
        reader: &SegmentReader,
        cancel: Option<&CancelToken>,
    ) -> tantivy::Result<Matches> {
        let max = reader.max_doc();
        let Some(re) = &prepared.verifier else {
            return Ok(Matches::Sparse(Vec::new()));
        };
        if self.out_of_time(cancel) {
            self.stop();
            return Ok(Matches::Sparse(Vec::new()));
        }
        let store = reader.get_store_reader(STORE_BLOCKS)?;
        let alive = reader.alive_bitset();
        // Ids until they'd outgrow a bitset, then the bitset: never more
        // than the smaller of the two while verifying, either.
        let mut found = Matches::Sparse(Vec::new());
        let mut n = 0u32;
        let mut check = |doc: DocId| -> tantivy::Result<bool> {
            n += 1;
            if n.is_multiple_of(CHECK_EVERY) && self.out_of_time(cancel) {
                self.stop();
                return Ok(false);
            }
            if alive.is_some_and(|a| !a.is_alive(doc)) {
                return Ok(true);
            }
            let d: TantivyDocument = store.get(doc)?;
            if d.get_first(self.name)
                .and_then(|v| v.as_str())
                .is_some_and(|name| re.is_match(name))
            {
                found.insert(doc, max);
            }
            Ok(true)
        };
        match prefilter {
            Some(w) => {
                let mut candidates = w.scorer(reader, 1.0)?;
                let mut doc = candidates.doc();
                while doc != TERMINATED {
                    if !check(doc)? {
                        break;
                    }
                    doc = candidates.advance();
                }
            }
            None => {
                for doc in 0..max {
                    if !check(doc)? {
                        break;
                    }
                }
            }
        }
        if let Matches::Sparse(ids) = &mut found {
            ids.shrink_to_fit();
        }
        Ok(found)
    }

    /// Verifies every segment of `searcher` not done yet, under `cancel`.
    fn seal(
        &self,
        searcher: &Searcher,
        cancel: &CancelToken,
        only: &dyn Fn(&SegmentReader) -> bool,
    ) -> tantivy::Result<()> {
        let prepared = self.prepared();
        let prefilter = self.prefilter_weight(
            prepared.as_deref(),
            EnableScoring::disabled_from_searcher(searcher),
        )?;
        for reader in searcher.segment_readers().iter().filter(|r| only(r)) {
            self.matches(
                prepared.as_deref(),
                prefilter.as_deref(),
                reader,
                Some(cancel),
            )?;
        }
        Ok(())
    }

    /// Every segment of `searcher` not verified yet stays unverified, and
    /// the clause stops: what a pass out of budget leaves ([`seal`]'s
    /// test hook).
    fn stop_unverified(&self, searcher: &Searcher, only: &dyn Fn(&SegmentReader) -> bool) {
        let mut done = lock(&self.state.done);
        for reader in searcher.segment_readers().iter().filter(|r| only(r)) {
            if let std::collections::hash_map::Entry::Vacant(e) = done.entry(reader.segment_id()) {
                e.insert(Arc::new(Matches::Sparse(Vec::new())));
                self.stop();
            }
        }
    }

    fn release(&self) {
        *lock(&self.state.prepared) = Prep::Released;
    }

    fn retained_bytes(&self) -> usize {
        CLAUSE_OVERHEAD
            + self.source.len()
            + lock(&self.state.done)
                .values()
                .map(|m| m.bytes())
                .sum::<usize>()
    }
}

impl Query for RegexQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let prepared = self.prepared();
        let prefilter = self.prefilter_weight(prepared.as_deref(), scoring)?;
        Ok(Box::new(RegexWeight {
            query: self.clone(),
            prepared,
            prefilter,
        }))
    }
}

struct RegexWeight {
    query: RegexQuery,
    prepared: Option<Arc<Prepared>>,
    prefilter: Option<Box<dyn Weight>>,
}

/// A segment's sparse matches as a doc set.
struct IdsDocSet {
    matches: Arc<Matches>,
    at: usize,
}

impl IdsDocSet {
    fn ids(&self) -> &[DocId] {
        match &*self.matches {
            Matches::Sparse(ids) => ids,
            Matches::Dense(_) => &[],
        }
    }
}

impl DocSet for IdsDocSet {
    fn advance(&mut self) -> DocId {
        self.at += 1;
        self.doc()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        let ids = self.ids();
        let rest = &ids[self.at.min(ids.len())..];
        self.at += rest.partition_point(|&d| d < target);
        self.doc()
    }

    fn doc(&self) -> DocId {
        self.ids().get(self.at).copied().unwrap_or(TERMINATED)
    }

    fn size_hint(&self) -> u32 {
        u32::try_from(self.ids().len().saturating_sub(self.at)).unwrap_or(u32::MAX)
    }
}

/// A segment's dense matches as a doc set, reading the shared bitset a
/// 64-document bucket at a time.
struct DenseDocSet {
    matches: Arc<Matches>,
    bucket: u32,
    /// The current bucket's documents not yet visited.
    rest: u64,
    doc: DocId,
}

impl DenseDocSet {
    fn new(matches: Arc<Matches>) -> DenseDocSet {
        let mut d = DenseDocSet {
            matches,
            bucket: 0,
            rest: 0,
            doc: TERMINATED,
        };
        d.from_bucket(0);
        d
    }

    fn bits(&self) -> Option<&BitSet> {
        match &*self.matches {
            Matches::Dense(bits) => Some(bits),
            Matches::Sparse(_) => None,
        }
    }

    /// Moves to the first document in bucket `from` or later.
    fn from_bucket(&mut self, from: u32) {
        let next = self.bits().and_then(|bits| {
            let buckets = bits.max_value().div_ceil(64);
            if from >= buckets {
                return None;
            }
            let b = bits.first_non_empty_bucket(from)?;
            Some((b, u64::from_le_bytes(bits.tinyset(b).into_bytes())))
        });
        match next {
            Some((b, set)) => {
                self.bucket = b;
                self.rest = set;
                self.pop();
            }
            None => self.doc = TERMINATED,
        }
    }

    /// The lowest document left in the current bucket, else the next.
    fn pop(&mut self) {
        if self.rest == 0 {
            self.from_bucket(self.bucket + 1);
            return;
        }
        let low = self.rest.trailing_zeros();
        self.rest &= self.rest - 1;
        self.doc = self.bucket * 64 + low;
    }
}

impl DocSet for DenseDocSet {
    fn advance(&mut self) -> DocId {
        if self.doc != TERMINATED {
            self.pop();
        }
        self.doc
    }

    fn seek(&mut self, target: DocId) -> DocId {
        if self.doc != TERMINATED && self.doc < target && target / 64 > self.bucket {
            self.from_bucket(target / 64);
        }
        while self.doc < target {
            self.advance();
        }
        self.doc
    }

    fn doc(&self) -> DocId {
        self.doc
    }

    fn size_hint(&self) -> u32 {
        self.bits()
            .map_or(0, |b| u32::try_from(b.len()).unwrap_or(u32::MAX))
    }
}

impl Weight for RegexWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        // A pass that didn't seal: verified now, under the deadline only.
        let set = self.query.matches(
            self.prepared.as_deref(),
            self.prefilter.as_deref(),
            reader,
            None,
        )?;
        if set.is_empty() {
            return Ok(Box::new(EmptyScorer));
        }
        Ok(match &*set {
            // Shared, not cloned: a lease's every page reads it.
            Matches::Dense(_) => Box::new(ConstScorer::new(DenseDocSet::new(set.clone()), boost)),
            Matches::Sparse(_) => Box::new(ConstScorer::new(
                IdsDocSet {
                    matches: set.clone(),
                    at: 0,
                },
                boost,
            )),
        })
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} doesn't match the regex"
            )));
        }
        Ok(Explanation::new("regex", scorer.score()))
    }
}

/// Every `/…/` clause of `query`, looking through the boolean queries
/// [`crate::compile`] builds, which are the only ones a regex clause
/// sits in.
fn each_clause(query: &dyn Query, f: &mut dyn FnMut(&RegexQuery)) {
    if let Some(r) = query.downcast_ref::<RegexQuery>() {
        f(r);
    } else if let Some(b) = query.downcast_ref::<BooleanQuery>() {
        for (_, q) in b.clauses() {
            each_clause(q.as_ref(), f);
        }
    }
}

/// A query's `/…/` clauses by pattern and prefilter field, for a query
/// compiled later on the same snapshot to reuse ([`crate::compile::Cx::regex_shared`]).
#[derive(Default)]
pub(crate) struct Shared(HashMap<(String, Option<Field>), RegexQuery>);

impl Shared {
    pub(crate) fn of(query: &dyn Query) -> Shared {
        let mut map = HashMap::new();
        // A pattern in the query twice keeps a stopped clause over a
        // whole one, so nothing reusing it reports whole while the lease
        // reports stopped.
        each_clause(query, &mut |r| {
            let kept = map
                .entry((r.source.clone(), r.trigram))
                .or_insert_with(|| r.clone());
            if r.stopped() && !kept.stopped() {
                *kept = r.clone();
            }
        });
        Shared(map)
    }

    /// The kept clause for `pattern` with prefilter `trigram`: a clone
    /// that shares its state.
    pub(crate) fn get(&self, pattern: &str, trigram: Option<Field>) -> Option<RegexQuery> {
        self.0.get(&(pattern.to_owned(), trigram)).cloned()
    }
}

/// Whether any `/…/` clause of `query` stopped before verifying every
/// candidate ([`RegexQuery::stopped`]): the results are then partial.
/// The count is a lower bound, or an upper one for a negated clause.
pub(crate) fn query_stopped(query: &dyn Query) -> bool {
    let mut any = false;
    each_clause(query, &mut |r| any |= r.stopped());
    any
}

/// Verifies every segment of `searcher` for each `/…/` clause of
/// `query`, under the pass's `cancel` (S8-10 item 6: never the token of
/// the request that compiled it). Passes over `searcher` then read the
/// kept matches. A stop leaves the clause partial, not an error.
pub(crate) fn seal(
    query: &dyn Query,
    searcher: &Searcher,
    cancel: &CancelToken,
) -> crate::index::Result<()> {
    seal_where(query, searcher, cancel, &|_| true)
}

/// [`seal`] for the segments `only` picks: a percolation run's, which
/// reads only the segments that can hold its range.
pub(crate) fn seal_where(
    query: &dyn Query,
    searcher: &Searcher,
    cancel: &CancelToken,
    only: &dyn Fn(&SegmentReader) -> bool,
) -> crate::index::Result<()> {
    if let Some(token) = CANCEL_AT_SEAL.with(|t| t.borrow_mut().take()) {
        token.cancel();
    }
    let out_of_budget = STOP_AT_SEAL.with(|c| match c.get() {
        Some(0) => {
            c.set(None);
            true
        }
        Some(n) => {
            c.set(Some(n - 1));
            false
        }
        None => false,
    });
    let mut out = Ok(());
    each_clause(query, &mut |r| {
        if out.is_ok() {
            out = if out_of_budget {
                r.stop_unverified(searcher, only);
                Ok(())
            } else {
                r.seal(searcher, cancel, only)
            };
        }
    });
    Ok(out?)
}

/// Drops each `/…/` clause's verifier and prefilter, keeping only its
/// verified matches: for a lease, once [`seal`] has verified its
/// snapshot.
pub(crate) fn release(query: &dyn Query) {
    each_clause(query, &mut |r| r.release());
}

/// What `query`'s `/…/` clauses keep alive: their verified matches and
/// a fixed overhead each. Zero for a query without one.
pub(crate) fn retained_bytes(query: &dyn Query) -> usize {
    let mut bytes = 0;
    each_clause(query, &mut |r| bytes += r.retained_bytes());
    bytes
}

/// Whether the search `compiled` describes was cut short by its regex
/// budget (or a pass's cancel token) on any pass so far: its hits are
/// what was verified in time, and its total a lower bound (an upper one
/// when the stopped clause is negated) (UI review 02 §5.2: "Stopped
/// after 2 s", the count marked ≈). A partial result must not be cached
/// as a complete one.
pub fn stopped(compiled: &crate::Compiled) -> bool {
    query_stopped(compiled.query.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::collector::DocSetCollector;
    use tantivy::{Index, IndexWriter, Searcher};

    use crate::schema::{self, Fields};

    fn index(names: &[&str]) -> (Searcher, Fields) {
        let (schema, fields) = schema::build();
        let index = Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(schema::TRIGRAM_TOKENIZER, schema::trigram_analyzer());
        index.tokenizers().register(
            crate::tokenizer::NAME_TOKENIZER,
            crate::tokenizer::NameTokenizer::default(),
        );
        let mut w: IndexWriter = index.writer(15 << 20).unwrap();
        for n in names {
            let mut d = TantivyDocument::new();
            d.add_text(fields.name, n);
            if let Some(grams) = crate::doc::trigram_tokens(n) {
                d.add_pre_tokenized_text(fields.name_trigram, grams);
            }
            w.add_document(d).unwrap();
        }
        w.commit().unwrap();
        (index.reader().unwrap().searcher(), fields)
    }

    fn docs(s: &Searcher, q: &dyn Query) -> Vec<DocId> {
        let found = s.search(q, &DocSetCollector).unwrap();
        let mut out: Vec<DocId> = found.into_iter().map(|a| a.doc_id).collect();
        out.sort_unstable();
        out
    }

    fn query(pattern: &str, f: &Fields, budget: Duration) -> RegexQuery {
        RegexQuery::new(
            pattern,
            f.name,
            Some(f.name_trigram),
            Instant::now() + budget,
        )
    }

    const NONE: [DocId; 0] = [];

    #[test]
    fn the_prefilter_keeps_long_names_and_drops_names_without_the_grams() {
        let long = format!("{}needle", "x".repeat(crate::doc::TRIGRAM_PREFIX_BYTES));
        let (s, f) = index(&["needle in front", "nothing here", &long, "needl"]);
        let q = query("needle", &f, VERIFY_BUDGET);
        let pre = q
            .prepared()
            .unwrap()
            .prefilter
            .as_ref()
            .unwrap()
            .box_clone();
        // The short names without the grams never reach the verifier;
        // the long one does, through the marker.
        assert_eq!(docs(&s, pre.as_ref()), [0, 2]);
        assert_eq!(docs(&s, &q), [0, 2]);
        assert!(!q.stopped());
        // Without the field complete, every name is verified: same answer.
        let all = RegexQuery::new("needle", f.name, None, Instant::now() + VERIFY_BUDGET);
        assert!(all.prepared().unwrap().prefilter.is_none());
        assert_eq!(docs(&s, &all), [0, 2]);
    }

    /// A pattern twice in one query: what reuses it shares the stopped
    /// clause, not the whole one.
    #[test]
    fn shared_clauses_prefer_a_stopped_one() {
        let (s, f) = index(&["needle"]);
        let whole = query("needle", &f, VERIFY_BUDGET);
        let cut = query("needle", &f, Duration::ZERO);
        let q = BooleanQuery::new(vec![
            (Occur::Should, Box::new(whole.clone()) as Q),
            (Occur::Should, Box::new(cut.clone()) as Q),
        ]);
        let _ = docs(&s, &q);
        assert!(!whole.stopped() && cut.stopped());
        let shared = Shared::of(&q);
        assert!(
            shared
                .get("needle", Some(f.name_trigram))
                .unwrap()
                .stopped()
        );
    }

    /// The shared dense doc set walks and seeks exactly the bitset's
    /// documents.
    #[test]
    fn a_dense_doc_set_reads_the_shared_bitset() {
        let max = 1000;
        let ids: Vec<DocId> = (0..max).filter(|d| d % 3 == 0 || *d == 999).collect();
        let mut m = Matches::Sparse(Vec::new());
        for id in &ids {
            m.insert(*id, max);
        }
        assert!(matches!(m, Matches::Dense(_)));
        let m = Arc::new(m);
        let mut d = DenseDocSet::new(m.clone());
        let mut got = Vec::new();
        while d.doc() != TERMINATED {
            got.push(d.doc());
            d.advance();
        }
        assert_eq!(got, ids);
        for target in [0, 1, 64, 65, 500, 998, 999, 1000] {
            let mut d = DenseDocSet::new(m.clone());
            let want = ids
                .iter()
                .copied()
                .find(|&i| i >= target)
                .unwrap_or(TERMINATED);
            assert_eq!(d.seek(target), want, "seek {target}");
        }
        let empty = Arc::new(Matches::Dense(BitSet::with_max_value(max)));
        assert_eq!(DenseDocSet::new(empty).doc(), TERMINATED);
    }

    #[test]
    fn plans_become_queries() {
        let (_, f) = index(&[]);
        assert!(plan_query(&Plan::All, f.name_trigram).is_none());
        let g = |s: &str| Plan::Gram(s.into());
        let or = Plan::Or(vec![g("abc"), Plan::All]);
        assert!(plan_query(&or, f.name_trigram).is_none());
        let and = plan_query(&Plan::And(vec![g("abc"), Plan::All]), f.name_trigram).unwrap();
        assert_eq!(
            and.downcast_ref::<BooleanQuery>().unwrap().clauses().len(),
            1
        );
    }

    #[test]
    fn an_empty_budget_stops_and_says_so_on_every_pass() {
        let (s, f) = index(&["needle", "needle two"]);
        let q = query("needle", &f, Duration::ZERO);
        assert_eq!(docs(&s, &q), NONE);
        assert!(q.stopped());
        // A clone (as a lease keeps) shares the state.
        let wrapped = BooleanQuery::new(vec![(Occur::Must, Box::new(q.clone()) as Q)]);
        assert!(query_stopped(&wrapped));
        assert_eq!(docs(&s, &wrapped), NONE);
    }

    #[test]
    fn refused_and_impossible_patterns_match_nothing() {
        let (s, f) = index(&["abc", "a(b"]);
        // A stored node isn't trusted: the planner's limits apply again.
        let long = "a".repeat(600);
        assert_eq!(docs(&s, &query(&long, &f, VERIFY_BUDGET)), NONE);
        assert_eq!(docs(&s, &query("a(b", &f, VERIFY_BUDGET)), NONE);
        assert_eq!(docs(&s, &query(r"[^\s\S]", &f, VERIFY_BUDGET)), NONE);
    }

    #[test]
    fn the_verifier_compiles_what_was_planned() {
        let p = crate::regex::parse("S0[1-9]E").unwrap();
        assert!(verifier(&p).unwrap().is_match("show.s01e02"));
        let p = crate::regex::parse("(?-i)S0").unwrap();
        assert!(!verifier(&p).unwrap().is_match("s0"));
    }
}
