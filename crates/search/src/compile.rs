//! Query AST → Tantivy query (architecture §8).

use std::ops::Bound;
use std::sync::Arc;

use lodestar_classify::Category;
use lodestar_store::{Col, HotColumns};
use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, FuzzyTermQuery, Occur,
    PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};

use crate::files::{IdSet, IdsQuery};
use crate::filter::{CheckQuery, FastRange};
use crate::query::{DateField, Node, NumField, ShareBy, TagKey, TextField, When};
use crate::schema::{Fields, NAME_BOOST, PATH_BOOST, STEM_SCORE, TITLE_BOOST, share_field};
use crate::tokenizer::{self, Tok, TokKind};

type Q = Box<dyn Query>;

/// The torrent ids with a matching file, for each `file:(…)` clause in
/// a query, keyed by the clause's inner node (`{:?}`). The caller runs
/// them on the file index first.
pub(crate) type FileSets = std::collections::HashMap<String, Arc<IdSet>>;

/// What compiling needs: the torrent index's fields (it derefs to them),
/// the `file:(…)` results, whether words may also match by their stem in
/// the title (the fallback tier, §8), the hot columns and the user's zone.
#[derive(Clone, Copy)]
pub(crate) struct Cx<'a> {
    pub fields: &'a Fields,
    pub files: &'a FileSets,
    pub stems: bool,
    pub hot: Option<&'a Arc<HotColumns>>,
    /// Seconds east of UTC, for `time:`.
    pub utc_offset: i32,
}

impl std::ops::Deref for Cx<'_> {
    type Target = Fields;

    fn deref(&self) -> &Fields {
        self.fields
    }
}

impl Cx<'_> {
    /// Negated clauses match exactly: the stem tier widens what a query
    /// finds, never what it excludes.
    fn exact(&self) -> Self {
        Cx {
            stems: false,
            ..*self
        }
    }

    fn ids(&self, of: &Node) -> Arc<IdSet> {
        self.files
            .get(&format!("{of:?}"))
            .cloned()
            .unwrap_or_default()
    }
}

/// Every `file:(…)` clause's inner node, in order.
pub(crate) fn file_clauses(node: &Node, out: &mut Vec<Node>) {
    match node {
        Node::And { of } | Node::Or { of } => {
            for n in of {
                file_clauses(n, out);
            }
        }
        Node::Not { of } => file_clauses(of, out),
        Node::File { of } => out.push((**of).clone()),
        _ => {}
    }
}

/// A compiled query in its parts.
pub(crate) struct Parts {
    /// Selects the matches and gives the phase-1 score.
    pub query: Q,
    /// Top-level ranges, checked in the collector (`filter`).
    pub filters: Vec<FastRange>,
    /// Name boosts of top-level words, added to the phase-1 candidates
    /// only (phase 2, §8).
    pub boosts: Option<Q>,
}

/// Compiles `node`; `now` (unix seconds) resolves relative dates.
///
/// The top-level conjunction gets two treatments:
/// - Ranges come back separately, to be checked in the collector,
///   whenever another clause can drive the query; otherwise the first
///   one stays in the query to drive it.
/// - An unfielded word matches on `text` alone; its name boost comes
///   back separately for the re-ranker. A conjunction of plain terms is
///   a flat intersection, about twice as fast as one carrying optional
///   boost clauses.
pub(crate) fn compile(node: &Node, f: &Cx<'_>, now: i64) -> Parts {
    let of: Vec<&Node> = match node {
        Node::And { of } => of.iter().collect(),
        n => vec![n],
    };
    // Hot-column and time-of-day clauses aren't in the index: at the top
    // level they're always checked in the collector, negated ones too, and
    // never drive (a filter-only query becomes everything, checked).
    let (checked, of): (Vec<&Node>, Vec<&Node>) = of.into_iter().partition(|n| match n {
        Node::Not { of } => always_lifted(of),
        n => always_lifted(n),
    });
    let (ranges, mut rest): (Vec<&Node>, Vec<&Node>) = of.into_iter().partition(|n| liftable(n));
    // A clause drives when it narrows the matches by itself; a negation
    // (`-x`, `private:no`) only removes a few from everything.
    let drives = |n: &&Node| {
        !matches!(n, Node::Not { .. } | Node::Flag { value: false, .. })
            && compile_opt(n, f, now).is_some()
    };
    let keep = usize::from(!rest.iter().any(drives)).min(ranges.len());
    rest.extend(&ranges[..keep]);
    let filters = ranges[keep..]
        .iter()
        .chain(&checked)
        .map(|n| fast_range(n, f, now).expect("liftable"))
        .collect();
    let mut clauses = Vec::new();
    let mut boosts = Vec::new();
    for n in rest {
        match n {
            Node::Not { of } => {
                if let Some(q) = compile_opt(of, &f.exact(), now) {
                    clauses.push((Occur::MustNot, q));
                }
            }
            Node::Word {
                field: TextField::Any,
                word,
            } => {
                if let Some((m, boost)) = any_word(word, f) {
                    push_must(&mut clauses, m);
                    boosts.push((Occur::Should, boost));
                }
            }
            n => {
                if let Some(q) = compile_opt(n, f, now) {
                    push_must(&mut clauses, q);
                }
            }
        }
    }
    let boosts = match boosts.len() {
        0 => None,
        1 => boosts.pop().map(|(_, q)| q),
        _ => Some(Box::new(BooleanQuery::new(boosts)) as Q),
    };
    Parts {
        query: conjunction(clauses),
        filters,
        boosts,
    }
}

/// The clauses as one query; everything when none constrain.
fn conjunction(mut clauses: Vec<(Occur, Q)>) -> Q {
    if clauses.iter().all(|(o, _)| *o == Occur::MustNot) {
        clauses.push((Occur::Must, Box::new(AllQuery)));
    }
    if let [(Occur::Must, _)] = clauses.as_slice() {
        return clauses.pop().expect("one").1;
    }
    Box::new(BooleanQuery::new(clauses))
}

/// An unfielded word: required on `text` (or by its stem in the title),
/// plus its boosts on `name` and `title`.
fn any_word(word: &str, f: &Cx<'_>) -> Option<(Q, Q)> {
    let toks = tokenizer::tokenize(word);
    if toks.is_empty() {
        return None;
    }
    let boost = BooleanQuery::new(vec![
        (
            Occur::Should,
            Box::new(BoostQuery::new(word_query(f.name, &toks, true), NAME_BOOST)) as Q,
        ),
        (
            Occur::Should,
            Box::new(BoostQuery::new(
                word_query(f.title, &toks, true),
                TITLE_BOOST,
            )),
        ),
    ]);
    Some((
        with_stem(word_query(f.text, &toks, false), &toks, f),
        Box::new(boost),
    ))
}

/// In the fallback tier (`f.stems`): `exact`, or the word's light stem
/// in `title_stem` when it's a single stemmable word. This is §8's
/// "plurals and suffixes" tier, scored as a constant below exact
/// matches. Otherwise just `exact`.
fn with_stem(exact: Q, toks: &[Tok], f: &Cx<'_>) -> Q {
    if !f.stems {
        return exact;
    }
    let units = units(toks);
    let [one] = units.as_slice() else {
        return exact;
    };
    let Some(stem) = crate::stem::light_stem(&one.text) else {
        return exact;
    };
    let by_stem = ConstScoreQuery::new(
        term(f.title_stem, &stem, IndexRecordOption::Basic),
        STEM_SCORE,
    );
    Box::new(BooleanQuery::new(vec![
        (Occur::Should, exact),
        (Occur::Should, Box::new(by_stem)),
    ]))
}

fn resolve(w: &When, now: i64) -> i64 {
    match w {
        When::Ago(s) => now.saturating_sub(*s),
        When::At(t) => *t,
    }
}

fn num_field(field: NumField, f: &Fields) -> Field {
    match field {
        NumField::Size => f.size,
        NumField::Files => f.files,
        NumField::Largest => f.largest,
        NumField::Piece => f.piece_len,
        NumField::Year => f.year,
        NumField::Season => f.season,
        NumField::Episode => f.episode,
        NumField::Junk => f.junk,
        NumField::Seeders | NumField::Leechers | NumField::Trackers | NumField::Dht => {
            unreachable!("hot columns aren't indexed")
        }
    }
}

fn fast_name(field: NumField) -> &'static str {
    match field {
        NumField::Size => "size",
        NumField::Files => "files",
        NumField::Largest => "largest",
        NumField::Piece => "piece_len",
        NumField::Junk => "junk",
        NumField::Year
        | NumField::Season
        | NumField::Episode
        | NumField::Seeders
        | NumField::Leechers
        | NumField::Trackers
        | NumField::Dht => unreachable!("not fast"),
    }
}

/// The hot column a count field reads.
pub(crate) fn hot_col(field: NumField) -> Option<Col> {
    Some(match field {
        NumField::Seeders => Col::Seeders,
        NumField::Leechers => Col::Leechers,
        NumField::Trackers => Col::Trackers,
        NumField::Dht => Col::Dht,
        _ => return None,
    })
}

/// Whether a clause reads what the index doesn't hold (hot columns, the
/// time of day), so it's always checked rather than searched.
fn always_lifted(n: &Node) -> bool {
    match n {
        Node::Range { field, .. } => field.is_hot(),
        Node::Date { field, .. } => *field == DateField::Seen,
        Node::TimeOfDay { .. } => true,
        _ => false,
    }
}

/// Whether a query reads the hot columns anywhere.
pub(crate) fn reads_hot(n: &Node) -> bool {
    match n {
        Node::And { of } | Node::Or { of } => of.iter().any(reads_hot),
        Node::Not { of } => reads_hot(of),
        Node::Range { field, .. } => field.is_hot(),
        Node::Date { field, .. } => *field == DateField::Seen,
        _ => false,
    }
}

/// Whether a query has a time-of-day clause (which depends on the zone).
pub(crate) fn has_time_of_day(n: &Node) -> bool {
    match n {
        Node::And { of } | Node::Or { of } => of.iter().any(has_time_of_day),
        Node::Not { of } => has_time_of_day(of),
        Node::TimeOfDay { .. } => true,
        _ => false,
    }
}

/// A category by name, or `None` for a name that isn't one (only
/// possible in AST JSON).
fn category(cat: &str) -> Option<Category> {
    Category::ALL.into_iter().find(|c| c.as_str() == cat)
}

fn share_name(by: ShareBy, cat: &str) -> Option<String> {
    category(cat).map(|c| share_field(by == ShareBy::Bytes, c))
}

/// Whether a top-level clause can be checked in the collector.
fn liftable(n: &Node) -> bool {
    match n {
        Node::Range { field, .. } => field.is_fast() || field.is_hot(),
        Node::Date { .. } | Node::File { .. } | Node::TimeOfDay { .. } => true,
        Node::Share { by, cat, .. } => share_name(*by, cat).is_some(),
        _ => false,
    }
}

/// A liftable node as a fast-field check.
fn fast_range(node: &Node, cx: &Cx<'_>, now: i64) -> Option<FastRange> {
    Some(match node {
        Node::Range { field, lo, hi } if field.is_fast() => FastRange::U64 {
            field: fast_name(*field).to_owned(),
            lo: lo.unwrap_or(0),
            hi: hi.unwrap_or(u64::MAX),
            missing: None,
        },
        Node::Range { field, lo, hi } => FastRange::Hot {
            col: hot_col(*field)?,
            lo: lo.unwrap_or(0),
            hi: hi.unwrap_or(u64::MAX),
        },
        Node::Date {
            field: DateField::Discovered,
            from,
            to,
        } => FastRange::I64 {
            field: "discovered",
            lo: from.as_ref().map_or(i64::MIN, |w| resolve(w, now)),
            hi: to.as_ref().map_or(i64::MAX, |w| resolve(w, now)),
        },
        // Last seen is unix seconds in a u32 column; a bound before 1970
        // can't be met and one past 2106 is no bound.
        Node::Date {
            field: DateField::Seen,
            from,
            to,
        } => {
            let lo = from.as_ref().map_or(0, |w| resolve(w, now).max(0) as u64);
            let hi = match to.as_ref().map(|w| resolve(w, now)) {
                None => u64::MAX,
                Some(t) if t < 0 => return Some(nothing()),
                Some(t) => t as u64,
            };
            FastRange::Hot {
                col: Col::LastSeen,
                lo,
                hi,
            }
        }
        Node::TimeOfDay { from, to } => FastRange::TimeOfDay {
            from: *from,
            to: *to,
            offset: cx.utc_offset,
        },
        Node::Not { of } if always_lifted(of) => FastRange::Not(Box::new(fast_range(of, cx, now)?)),
        Node::Share { by, cat, lo, hi } => FastRange::U64 {
            field: share_name(*by, cat)?,
            lo: u64::from(*lo),
            hi: u64::from(*hi),
            missing: Some(0),
        },
        Node::File { of } => FastRange::Ids { ids: cx.ids(of) },
        _ => return None,
    })
}

/// A check nothing passes.
fn nothing() -> FastRange {
    FastRange::Hot {
        col: Col::LastSeen,
        lo: 1,
        hi: 0,
    }
}

/// A hot-column or time-of-day clause that has to select by itself.
fn checked(node: &Node, f: &Cx<'_>, now: i64) -> Q {
    Box::new(CheckQuery {
        range: fast_range(node, f, now).unwrap_or_else(nothing),
        hot: f.hot.cloned(),
    })
}

/// `None` for a clause that constrains nothing (a word with no tokens,
/// like a lone `-`), so `And` can drop it.
fn compile_opt(node: &Node, f: &Cx<'_>, now: i64) -> Option<Q> {
    Some(match node {
        Node::All => Box::new(AllQuery),
        Node::And { of } => {
            let mut clauses: Vec<(Occur, Q)> = Vec::new();
            for n in of {
                match n {
                    Node::Not { of } => {
                        if let Some(q) = compile_opt(of, &f.exact(), now) {
                            clauses.push((Occur::MustNot, q));
                        }
                    }
                    n => {
                        if let Some(q) = compile_opt(n, f, now) {
                            push_must(&mut clauses, q);
                        }
                    }
                }
            }
            conjunction(clauses)
        }
        Node::Or { of } => {
            let clauses: Vec<(Occur, Q)> = of
                .iter()
                .filter_map(|n| compile_opt(n, f, now))
                .map(|q| (Occur::Should, q))
                .collect();
            if clauses.is_empty() {
                return None;
            }
            Box::new(BooleanQuery::new(clauses))
        }
        Node::Not { of } => {
            let q = compile_opt(of, &f.exact(), now)?;
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Q),
                (Occur::MustNot, q),
            ]))
        }
        // Nested (under `|` or `-`): the boost stays inline.
        Node::Word {
            field: TextField::Any,
            word,
        } => {
            let (m, boost) = any_word(word, f)?;
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, m),
                (Occur::Should, boost),
            ]))
        }
        Node::Word { field, word } => {
            let toks = tokenizer::tokenize(word);
            if toks.is_empty() {
                return None;
            }
            let q = per_field(*field, f, |fld, positions| {
                word_query(fld, &toks, positions)
            });
            if *field == TextField::Title {
                with_stem(q, &toks, f)
            } else {
                q
            }
        }
        Node::Phrase { field, text } => {
            let parts = units(&tokenizer::tokenize(text));
            if parts.is_empty() {
                return None;
            }
            per_field(phrase_field(*field), f, |fld, positions| {
                parts_query(fld, &parts, positions, 0)
            })
        }
        Node::Near { field, words, slop } => {
            // Units per word (offsets restart in each word), positions
            // running on across words.
            let mut parts = Vec::new();
            let mut next = 0;
            for w in words {
                let mut toks = Vec::new();
                next = tokenizer::tokenize_into(w, next, &mut toks, 512);
                parts.extend(units(&toks));
            }
            if parts.is_empty() {
                return None;
            }
            per_field(phrase_field(*field), f, |fld, positions| {
                parts_query(fld, &parts, positions, *slop)
            })
        }
        Node::Prefix { field, prefix } => {
            let folded = folded_word(prefix)?;
            let pattern = format!("{}.*", regex_escape(&folded));
            per_field(*field, f, |fld, _| {
                match RegexQuery::from_pattern(&pattern, fld) {
                    Ok(q) => Box::new(q),
                    Err(_) => Box::new(EmptyQuery),
                }
            })
        }
        Node::Fuzzy {
            field,
            word,
            distance,
        } => {
            let folded = folded_word(word)?;
            per_field(*field, f, |fld, _| {
                Box::new(FuzzyTermQuery::new(
                    Term::from_field_text(fld, &folded),
                    (*distance).min(2),
                    true,
                ))
            })
        }
        n if always_lifted(n) => checked(n, f, now),
        Node::Range { field, lo, hi } => {
            let fld = num_field(*field, f);
            let bound = |v: &Option<u64>| match v {
                Some(v) => Bound::Included(Term::from_field_u64(fld, *v)),
                None => Bound::Unbounded,
            };
            Box::new(RangeQuery::new(bound(lo), bound(hi)))
        }
        Node::Date { from, to, .. } => {
            let bound = |w: &Option<When>| match w {
                Some(w) => Bound::Included(Term::from_field_i64(f.discovered, resolve(w, now))),
                None => Bound::Unbounded,
            };
            Box::new(RangeQuery::new(bound(from), bound(to)))
        }
        Node::Ext { any } => {
            let clauses: Vec<(Occur, Q)> = any
                .iter()
                .map(|e| (Occur::Should, term(f.ext, e, IndexRecordOption::Basic)))
                .collect();
            Box::new(BooleanQuery::new(clauses))
        }
        Node::Flag { name, value } => {
            let q = term(f.flag, name, IndexRecordOption::Basic);
            if *value {
                q
            } else {
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Q),
                    (Occur::MustNot, q),
                ]))
            }
        }
        Node::Tag { key, any } => {
            let mut clauses: Vec<(Occur, Q)> = any
                .iter()
                .map(|v| (Occur::Should, tag_term(f, *key, v)))
                .collect();
            match clauses.len() {
                0 => return None,
                1 => clauses.pop().expect("one").1,
                _ => Box::new(BooleanQuery::new(clauses)),
            }
        }
        Node::Share { by, cat, lo, hi } => {
            let Some(c) = category(cat) else {
                return Some(Box::new(EmptyQuery));
            };
            let fld = match by {
                ShareBy::Count => f.cat_count[c.index()],
                ShareBy::Bytes => f.cat_bytes[c.index()],
            };
            let range = |lo: u8, hi: u8| -> Q {
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_u64(fld, u64::from(lo))),
                    Bound::Included(Term::from_field_u64(fld, u64::from(hi))),
                ))
            };
            match (*lo, *hi) {
                (lo, hi) if lo > hi => Box::new(EmptyQuery),
                // Shares of 0 aren't written: everything but the
                // torrents above the range.
                (0, 100..) => Box::new(AllQuery),
                (0, hi) => Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Q),
                    (Occur::MustNot, range(hi + 1, 100)),
                ])),
                (lo, hi) => range(lo, hi),
            }
        }
        Node::File { of } => {
            let ids = f.ids(of);
            if ids.is_empty() {
                Box::new(EmptyQuery)
            } else {
                Box::new(IdsQuery { ids })
            }
        }
        Node::TimeOfDay { .. } => checked(node, f, now),
        Node::Hash { hash } => match hex20(hash) {
            Some(bytes) => Box::new(TermQuery::new(
                Term::from_field_bytes(f.hash, &bytes),
                IndexRecordOption::Basic,
            )),
            None => Box::new(EmptyQuery),
        },
    })
}

/// Phrases and NEAR need word order, which only `name` keeps at the
/// torrent level; file names get it with the file index (M3). An explicit
/// `path:"…"` still matches all of its words in the paths.
/// Adds a required clause to a conjunction, splicing in the clauses of a
/// conjunction it holds (required terms plus optional boosts), so the
/// required terms of every word form one flat intersection, which Tantivy
/// runs much faster than nested ones.
fn push_must(clauses: &mut Vec<(Occur, Q)>, q: Q) {
    if let Some(b) = q.downcast_ref::<BooleanQuery>() {
        let inner = b.clauses();
        let splice = inner.iter().any(|(o, _)| *o == Occur::Must)
            && inner
                .iter()
                .all(|(o, _)| matches!(o, Occur::Must | Occur::Should));
        if splice {
            for (occur, q) in inner {
                match occur {
                    Occur::Must => push_must(clauses, q.box_clone()),
                    _ => clauses.push((*occur, q.box_clone())),
                }
            }
            return;
        }
    }
    clauses.push((Occur::Must, q));
}

/// A `tag` term: `key:value`.
pub(crate) fn tag_term(f: &Fields, key: TagKey, value: &str) -> Q {
    term(
        f.tag,
        &format!("{}:{value}", key.as_str()),
        IndexRecordOption::Basic,
    )
}

fn phrase_field(field: TextField) -> TextField {
    match field {
        TextField::Any => TextField::Name,
        other => other,
    }
}

fn term(field: Field, text: &str, opt: IndexRecordOption) -> Q {
    Box::new(TermQuery::new(Term::from_field_text(field, text), opt))
}

/// Runs `build` for each field a text clause covers, boosted, as
/// alternatives. The bool says whether the field has positions.
fn per_field(field: TextField, f: &Fields, build: impl Fn(Field, bool) -> Q) -> Q {
    let targets: &[(Field, f32, bool)] = match field {
        TextField::Any => &[(f.name, NAME_BOOST, true), (f.path, PATH_BOOST, false)],
        TextField::Name => &[(f.name, 1.0, true)],
        TextField::Path => &[(f.path, 1.0, false)],
        TextField::Title => &[(f.title, 1.0, true)],
    };
    if let [(fld, _, positions)] = targets {
        return build(*fld, *positions);
    }
    Box::new(BooleanQuery::new(
        targets
            .iter()
            .map(|(fld, boost, positions)| {
                (
                    Occur::Should,
                    Box::new(BoostQuery::new(build(*fld, *positions), *boost)) as Q,
                )
            })
            .collect(),
    ))
}

/// The matching units of one tokenized string, in order: each atom's
/// whole-word token where it has one (instead of its parts, which sit
/// inside its byte range), else its single part. Joined compounds are
/// left out.
pub(crate) fn units(toks: &[Tok]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut whole: Option<(usize, usize)> = None;
    for t in toks {
        match t.kind {
            TokKind::Whole => {
                whole = Some((t.from, t.to));
                out.push(t.clone());
            }
            TokKind::Part if whole.is_some_and(|(f, e)| t.from >= f && t.to <= e) => {}
            TokKind::Part => {
                whole = None;
                out.push(t.clone());
            }
            TokKind::Compound => {}
        }
    }
    out
}

/// One word: its units as a phrase (or all of them, in a field without
/// positions), or any compound it forms (`web-dl` ↔ `webdl`).
pub(crate) fn word_query(field: Field, toks: &[Tok], positions: bool) -> Q {
    let parts = units(toks);
    let mut alternatives: Vec<(Occur, Q)> = Vec::new();
    if !parts.is_empty() {
        alternatives.push((Occur::Should, parts_query(field, &parts, positions, 0)));
    }
    for c in toks.iter().filter(|t| t.kind == TokKind::Compound) {
        alternatives.push((
            Occur::Should,
            term(field, &c.text, IndexRecordOption::WithFreqs),
        ));
    }
    if alternatives.len() == 1 {
        return alternatives.pop().expect("one").1;
    }
    Box::new(BooleanQuery::new(alternatives))
}

/// Parts in order as a phrase (with `slop`), or all of them without
/// positions.
pub(crate) fn parts_query(field: Field, parts: &[Tok], positions: bool, slop: u32) -> Q {
    match parts {
        [one] => term(field, &one.text, IndexRecordOption::WithFreqs),
        many if positions => {
            let first = many[0].position;
            Box::new(PhraseQuery::new_with_offset_and_slop(
                many.iter()
                    .map(|t| (t.position - first, Term::from_field_text(field, &t.text)))
                    .collect(),
                slop,
            ))
        }
        many => Box::new(BooleanQuery::new(
            many.iter()
                .map(|t| {
                    (
                        Occur::Must,
                        term(field, &t.text, IndexRecordOption::WithFreqs),
                    )
                })
                .collect(),
        )),
    }
}

/// A prefix or fuzzy word as it's indexed: its compound if it has one,
/// else its only part.
pub(crate) fn folded_word(word: &str) -> Option<String> {
    let toks = tokenizer::tokenize(word);
    toks.iter()
        .find(|t| t.kind == TokKind::Whole)
        .or_else(|| toks.iter().find(|t| t.kind == TokKind::Compound))
        .or_else(|| toks.first())
        .map(|t| t.text.clone())
}

pub(crate) fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn hex20(s: &str) -> Option<[u8; 20]> {
    if s.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, pair) in s.as_bytes().chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// Whether the stem tier could change what `node` matches: it has a
/// positive single-word clause (unfielded or `title:`) with a light stem.
pub(crate) fn has_stem_alternative(node: &Node) -> bool {
    match node {
        Node::And { of } | Node::Or { of } => of.iter().any(has_stem_alternative),
        Node::Word {
            field: TextField::Any | TextField::Title,
            word,
        } => matches!(
            units(&tokenizer::tokenize(word)).as_slice(),
            [one] if crate::stem::light_stem(&one.text).is_some()
        ),
        _ => false,
    }
}
