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
//! - **The budget.** One clause verifies for at most [`VERIFY_BUDGET`]
//!   from its first candidate, across every pass the search makes over
//!   it (count, page, facets, a lease's later pages), and stops early
//!   when the search's cancel token does. Each segment's matches are
//!   kept, so every pass sees the same answer. A stop leaves the clause
//!   **partial** ([`stopped`]): what was verified, never a silent
//!   truncation, for the UI's "Stopped after 2 s" with a ≈ count.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ::regex::{Regex, RegexBuilder};
use tantivy::index::SegmentId;
use tantivy::query::{
    BitSetDocSet, BooleanQuery, ConstScorer, EmptyScorer, EnableScoring, Explanation, Occur, Query,
    Scorer, TermQuery, Weight,
};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{BitSet, DocId, DocSet, Score, SegmentReader, TantivyDocument, Term};

use crate::CancelToken;
use crate::regex::{Pattern, Plan};

/// How long one regex clause may verify (§3: p95 < 500 ms; the UI says
/// "Stopped after 2 s"). Past it the clause is partial.
pub const VERIFY_BUDGET: Duration = Duration::from_secs(2);
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

/// What every pass over one clause shares.
struct State {
    budget: Duration,
    prepared: OnceLock<Prepared>,
    /// Set by the first candidate verified.
    deadline: OnceLock<Instant>,
    partial: AtomicBool,
    cancel: Option<CancelToken>,
    /// Each segment's verified matches.
    done: Mutex<HashMap<SegmentId, Arc<BitSet>>>,
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
    /// The clause for `pattern` on the stored `name` field. `trigram` is
    /// `name_trigram` when it's complete in the snapshot searched (a
    /// prefilter on a field still being filled would drop names it
    /// hasn't reached), else `None`: every name is then verified.
    pub(crate) fn new(
        pattern: &str,
        name: Field,
        trigram: Option<Field>,
        budget: Duration,
        cancel: Option<CancelToken>,
    ) -> RegexQuery {
        RegexQuery {
            source: pattern.to_owned(),
            name,
            trigram,
            state: Arc::new(State {
                budget,
                prepared: OnceLock::new(),
                deadline: OnceLock::new(),
                partial: AtomicBool::new(false),
                cancel,
                done: Mutex::default(),
            }),
        }
    }

    fn prepared(&self) -> &Prepared {
        self.state.prepared.get_or_init(|| {
            // Checked again here: the node may not come from the parser.
            let Ok(pattern) = crate::regex::parse(&self.source) else {
                return Prepared {
                    verifier: None,
                    prefilter: None,
                };
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
        })
    }

    /// Whether the budget or the cancel token stopped this clause before
    /// it had verified every candidate.
    pub(crate) fn stopped(&self) -> bool {
        self.state.partial.load(Ordering::Relaxed)
    }

    /// This segment's matches, verified now or by an earlier pass.
    fn matches(
        &self,
        prefilter: Option<&dyn Weight>,
        reader: &SegmentReader,
    ) -> tantivy::Result<Arc<BitSet>> {
        let id = reader.segment_id();
        let done = |s: &State| {
            s.done
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&id)
                .cloned()
        };
        if let Some(set) = done(&self.state) {
            return Ok(set);
        }
        let set = Arc::new(self.verify(prefilter, reader)?);
        let mut map = self
            .state
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Two passes may race on one segment; the first kept wins, so
        // every pass sees one answer.
        Ok(map.entry(id).or_insert(set).clone())
    }

    fn verify(
        &self,
        prefilter: Option<&dyn Weight>,
        reader: &SegmentReader,
    ) -> tantivy::Result<BitSet> {
        let max = reader.max_doc();
        let mut set = BitSet::with_max_value(max);
        let Some(re) = &self.prepared().verifier else {
            return Ok(set);
        };
        let state = &self.state;
        let deadline = *state.deadline.get_or_init(|| {
            Instant::now()
                .checked_add(state.budget)
                .unwrap_or_else(Instant::now)
        });
        let out_of_time = || {
            Instant::now() >= deadline || state.cancel.as_ref().is_some_and(CancelToken::is_stopped)
        };
        if out_of_time() {
            state.partial.store(true, Ordering::Relaxed);
            return Ok(set);
        }
        let store = reader.get_store_reader(STORE_BLOCKS)?;
        let alive = reader.alive_bitset();
        let mut check = |doc: DocId, n: &mut u32| -> tantivy::Result<bool> {
            *n += 1;
            if (*n).is_multiple_of(CHECK_EVERY) && out_of_time() {
                state.partial.store(true, Ordering::Relaxed);
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
                set.insert(doc);
            }
            Ok(true)
        };
        let mut n = 0u32;
        match prefilter {
            Some(w) => {
                let mut candidates = w.scorer(reader, 1.0)?;
                let mut doc = candidates.doc();
                while doc != tantivy::TERMINATED {
                    if !check(doc, &mut n)? {
                        break;
                    }
                    doc = candidates.advance();
                }
            }
            None => {
                for doc in 0..max {
                    if !check(doc, &mut n)? {
                        break;
                    }
                }
            }
        }
        Ok(set)
    }
}

impl Query for RegexQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        // The prefilter only selects: no scores.
        let unscored = match scoring.searcher() {
            Some(s) => EnableScoring::disabled_from_searcher(s),
            None => EnableScoring::disabled_from_schema(scoring.schema()),
        };
        let prefilter = self
            .prepared()
            .prefilter
            .as_ref()
            .map(|q| q.weight(unscored))
            .transpose()?;
        Ok(Box::new(RegexWeight {
            query: self.clone(),
            prefilter,
        }))
    }
}

struct RegexWeight {
    query: RegexQuery,
    prefilter: Option<Box<dyn Weight>>,
}

impl Weight for RegexWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let set = self.query.matches(self.prefilter.as_deref(), reader)?;
        if set.len() == 0 {
            return Ok(Box::new(EmptyScorer));
        }
        Ok(Box::new(ConstScorer::new(
            BitSetDocSet::from(BitSet::clone(&set)),
            boost,
        )))
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

/// Whether any `/…/` clause of `query` stopped before verifying every
/// candidate ([`RegexQuery::stopped`]): the results are then partial,
/// and the count a lower bound. Looks through the boolean queries
/// [`crate::compile`] builds, which are the only ones a regex clause
/// sits in.
pub(crate) fn query_stopped(query: &dyn Query) -> bool {
    if let Some(r) = query.downcast_ref::<RegexQuery>() {
        return r.stopped();
    }
    query
        .downcast_ref::<BooleanQuery>()
        .is_some_and(|b| b.clauses().iter().any(|(_, q)| query_stopped(q.as_ref())))
}

/// Whether the search `compiled` describes was cut short by a regex
/// clause's time budget (or its cancel token) on any pass so far: its
/// hits are what was verified in time, and its total a lower bound
/// (UI review 02 §5.2: "Stopped after 2 s", the count marked ≈). A
/// partial result must not be cached as a complete one.
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
        RegexQuery::new(pattern, f.name, Some(f.name_trigram), budget, None)
    }

    const NONE: [DocId; 0] = [];

    #[test]
    fn the_prefilter_keeps_long_names_and_drops_names_without_the_grams() {
        let long = format!("{}needle", "x".repeat(crate::doc::TRIGRAM_PREFIX_BYTES));
        let (s, f) = index(&["needle in front", "nothing here", &long, "needl"]);
        let q = query("needle", &f, VERIFY_BUDGET);
        let pre = q.prepared().prefilter.as_ref().unwrap().box_clone();
        // The short names without the grams never reach the verifier;
        // the long one does, through the marker.
        assert_eq!(docs(&s, pre.as_ref()), [0, 2]);
        assert_eq!(docs(&s, &q), [0, 2]);
        assert!(!q.stopped());
        // Without the field complete, every name is verified: same answer.
        let all = RegexQuery::new("needle", f.name, None, VERIFY_BUDGET, None);
        assert!(all.prepared().prefilter.is_none());
        assert_eq!(docs(&s, &all), [0, 2]);
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
