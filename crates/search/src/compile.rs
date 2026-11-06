//! Query AST → Tantivy query (architecture §8).

use std::ops::Bound;
use std::sync::Arc;

use lodestar_classify::Category;
use lodestar_store::{Col, HotColumns};
use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, FuzzyTermQuery, Occur,
    PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::{Field, IndexRecordOption};

use crate::files::{IdSet, IdsQuery};
use crate::filter::{CheckQuery, FastRange};
use crate::query::{
    DateField, Network, Node, NumField, ShareBy, TagKey, TextField, Verified, When,
};
use crate::schema::{Fields, NAME_BOOST, PATH_BOOST, STEM_SCORE, TITLE_BOOST, share_field};
use crate::seek_guard::SeekGuard;
use crate::tokenizer::{self, Tok, TokKind};

type Q = Box<dyn Query>;

/// A field of this build's schema that the index being queried lacks:
/// an older index serving while a newer one is built (§8). A clause on it
/// matches nothing. Tantivy must never see it: it isn't in the schema.
/// (Documents hold field ids up to `u16::MAX`; no schema has that many.)
pub(crate) const ABSENT: Field = Field::from_field_id(u16::MAX as u32);

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
    /// Whether the torrent index's v7 bucket terms (`file_bucket`,
    /// `file_ext_bucket`) are complete, so [`bucket_clause`]s read them
    /// ([`search_file_clauses`] says which file sets that needs).
    pub buckets: bool,
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

/// The extensions of a `file:(…)` clause that asks for nothing else
/// (`file:(ext:mkv)`, `file:(ext:flac,mp3)`), which the torrent index
/// answers: its `ext` holds every file's extension, so no file-index scan
/// or join is needed (X-search-files-4, option (a), runner 2026-11-05).
/// It differs from the file index only by coverage: smart coverage
/// leaves small images and documents out of a large pack's file
/// documents, and hidden files out of every torrent's, and the torrent's
/// extensions keep them. A clause of two or more atoms still runs on the
/// file index, since it asks about one file; alternatives of extensions
/// alone (`file:(ext:a | ext:b)`) are one clause, `ext:a,b`.
///
/// Such a clause is still listed by [`file_clauses`], for its matched
/// files; `files::compile` marks it so the search's file pass skips it.
/// The torrent index has had `ext` since its first schema, so it's never
/// `ABSENT` on an index that opens; one built without it (only tests do)
/// matches nothing here, as any clause on a missing field does.
pub(crate) fn file_exts(of: &Node) -> Option<Vec<String>> {
    match of {
        Node::Ext { any } => Some(any.clone()),
        // `ext:a | ext:b` is `ext:a,b`.
        Node::Or { of } if !of.is_empty() => {
            let mut all = Vec::new();
            for n in of {
                all.extend(file_exts(n)?);
            }
            Some(all)
        }
        _ => None,
    }
}

/// What the search's file pass runs on the file index for `node`: as
/// [`file_clauses`], except that a [`bucket_clause`] the torrent index
/// answers (when `buckets`) needs only its edge buckets' files, and none
/// when its range is whole buckets ([`bucket_edges`]).
pub(crate) fn search_file_clauses(node: &Node, buckets: bool, out: &mut Vec<Node>) {
    match node {
        Node::And { of } | Node::Or { of } => {
            for n in of {
                search_file_clauses(n, buckets, out);
            }
        }
        Node::Not { of } => search_file_clauses(of, buckets, out),
        Node::File { of } if buckets && bucket_clause(of).is_some() => {
            out.extend(bucket_edges(of));
        }
        Node::File { of } => out.push((**of).clone()),
        _ => {}
    }
}

/// A `file:(…)` clause the torrent index's v7 bucket terms answer
/// (X-search-files-5): one size range, alone or beside one extension
/// atom (`file:(size:>1GB)`, `file:(ext:mkv,mp4 size:>1GB)`). A file has
/// one size, so one bucket, and one extension, so the torrent's
/// `file_bucket` and `file_ext_bucket` terms say exactly whether one of
/// its covered files is in a bucket (with that extension). Buckets wholly
/// inside the range are answered there; the file index checks the files
/// of the (at most two) edge buckets ([`bucket_edges`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BucketClause {
    pub exts: Option<Vec<String>>,
    pub lo: u64,
    pub hi: u64,
}

pub(crate) fn bucket_clause(of: &Node) -> Option<BucketClause> {
    let range = |n: &Node| match n {
        Node::Range {
            field: NumField::Size,
            lo,
            hi,
        } => Some((lo.unwrap_or(0), hi.unwrap_or(u64::MAX))),
        _ => None,
    };
    let (exts, (lo, hi)) = match of {
        n @ Node::Range { .. } => (None, range(n)?),
        Node::And { of } if of.len() == 2 => match (file_exts(&of[0]), file_exts(&of[1])) {
            (Some(e), None) => (Some(e), range(&of[1])?),
            (None, Some(e)) => (Some(e), range(&of[0])?),
            _ => return None,
        },
        _ => return None,
    };
    // An empty range matches nothing either way; leave it to the file
    // index, which says so without a term.
    (lo <= hi).then_some(BucketClause { exts, lo, hi })
}

/// The file-index clause for a [`bucket_clause`]'s edge buckets: its
/// extension (if any) and the part of its range in each edge bucket.
/// `None` when the range covers whole buckets only.
pub(crate) fn bucket_edges(of: &Node) -> Option<Node> {
    let b = bucket_clause(of)?;
    let (_, edges) = crate::files::code_plan(b.lo, b.hi);
    let mut ranges: Vec<Node> = edges
        .into_iter()
        .map(|(lo, hi)| Node::Range {
            field: NumField::Size,
            lo: Some(lo),
            hi: Some(hi),
        })
        .collect();
    let sizes = match ranges.len() {
        0 => return None,
        1 => ranges.pop().expect("one"),
        _ => Node::Or { of: ranges },
    };
    Some(match b.exts {
        None => sizes,
        Some(any) => Node::And {
            of: vec![Node::Ext { any }, sizes],
        },
    })
}

/// A `file:(…)` clause of one extension atom and negated words
/// (`file:(ext:mkv,mp4,avi -path:sample)`), which the torrent index
/// answers with the file index's help when the words are rare
/// (X-search-files-6): a torrent matches when one of its covered files
/// has one of the extensions and none of the words. So it matches
/// exactly when it has a covered file with the extensions
/// (`file_ext_bucket`, the candidates) and isn't one of the *bad*
/// torrents: those with such a file that has a word (the suspects, read
/// from the file index driven by the words' postings) and none without
/// (checked in the file index on the suspects' files only). The file
/// pass computes the bad set ([`crate::files::bad_torrents`]) and
/// stores it under [`bad_key`]; when it doesn't (a common word, or
/// the bucket terms not ready), the clause runs on the file index whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NegatedWords {
    pub exts: Vec<String>,
    /// The negated words, each a `Node::Word` that isn't on the title.
    pub words: Vec<Node>,
}

pub(crate) fn negated_words(of: &Node) -> Option<NegatedWords> {
    if !negated_shape(of) {
        return None;
    }
    let Node::And { of } = of else {
        return None;
    };
    let mut exts = None;
    let mut words = Vec::new();
    for n in of {
        match n {
            Node::Not { of: w } if matches!(**w, Node::Word { field, .. } if field != TextField::Title) =>
            {
                words.push((**w).clone());
            }
            n => match file_exts(n) {
                Some(e) if exts.is_none() => exts = Some(e),
                _ => return None,
            },
        }
    }
    let exts = exts?;
    (!words.is_empty() && !exts.is_empty()).then_some(NegatedWords { exts, words })
}

/// Whether `of` is a [`negated_words`] clause, without building one:
/// one extension atom and at least one negated word not on the title.
fn negated_shape(of: &Node) -> bool {
    fn exts_only(n: &Node) -> bool {
        match n {
            Node::Ext { any } => !any.is_empty(),
            Node::Or { of } => !of.is_empty() && of.iter().all(exts_only),
            _ => false,
        }
    }
    let Node::And { of } = of else {
        return false;
    };
    let (mut exts, mut words) = (0, 0);
    for n in of {
        match n {
            Node::Not { of: w } if matches!(**w, Node::Word { field, .. } if field != TextField::Title) => {
                words += 1
            }
            n if exts_only(n) => exts += 1,
            _ => return false,
        }
    }
    exts == 1 && words > 0
}

/// The [`FileSets`] key of a [`negated_words`] clause's bad torrents.
pub(crate) fn bad_key(of: &Node) -> String {
    format!("{of:?}@bad")
}

/// Whether the torrent index answers a `file:(…)` clause itself: a lone
/// extension ([`file_exts`]), or a [`bucket_clause`] when its terms are
/// complete.
/// It also answers a [`negated_words`] clause whose bad torrents the
/// file pass found.
fn torrent_level(of: &Node, f: &Cx<'_>) -> bool {
    file_exts(of).is_some()
        || (f.buckets
            && (bucket_clause(of).is_some()
                || (negated_shape(of) && f.files.contains_key(&bad_key(of)))))
}

/// The torrent-index query for a [`torrent_level`] clause, unscored.
fn torrent_level_query(of: &Node, f: &Cx<'_>, now: i64) -> Option<Q> {
    if let Some(any) = file_exts(of) {
        return compile_opt(&Node::Ext { any }, f, now);
    }
    if let Some(neg) = negated_words(of) {
        return Some(negated_query(&neg, f, f.files.get(&bad_key(of))?));
    }
    let b = bucket_clause(of)?;
    let (codes, _) = crate::files::code_plan(b.lo, b.hi);
    let mut clauses: Vec<(Occur, Q)> = Vec::new();
    for bucket in codes {
        match &b.exts {
            None if f.file_bucket != ABSENT => clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.file_bucket, bucket),
                    IndexRecordOption::Basic,
                )),
            )),
            Some(any) => {
                for e in any {
                    clauses.push((
                        Occur::Should,
                        term(
                            f.file_ext_bucket,
                            &crate::files::ext_bucket_term(e, bucket),
                            IndexRecordOption::Basic,
                        ),
                    ));
                }
            }
            None => {}
        }
    }
    // The edge buckets' files, from the file pass.
    if let Some(edges) = bucket_edges(of) {
        let ids = f.ids(&edges);
        if !ids.is_empty() {
            clauses.push((Occur::Should, Box::new(IdsQuery { ids })));
        }
    }
    Some(if clauses.is_empty() {
        Box::new(EmptyQuery)
    } else {
        Box::new(BooleanQuery::new(clauses))
    })
}

/// A [`negated_words`] clause on the torrent index: a covered file with
/// one of the extensions (any of its `{ext}:{code}` terms), less the
/// file pass's bad torrents (by their indexed `id`).
fn negated_query(neg: &NegatedWords, f: &Cx<'_>, bad: &IdSet) -> Q {
    if f.file_ext_bucket == ABSENT {
        return Box::new(EmptyQuery);
    }
    let exts: Vec<String> = neg.exts.iter().map(|e| regex_escape(e)).collect();
    let pattern = format!("({}):[0-9]+", exts.join("|"));
    let candidates: Q = match RegexQuery::from_pattern(&pattern, f.file_ext_bucket) {
        Ok(q) => Box::new(q),
        Err(_) => return Box::new(EmptyQuery),
    };
    // Every index that opens has `id` (since v1); one without can't
    // leave the bad torrents out, so it matches nothing.
    if f.id == ABSENT {
        return Box::new(EmptyQuery);
    }
    let bad = bad.to_vec();
    if bad.is_empty() {
        return candidates;
    }
    let ids = TermSetQuery::new(bad.into_iter().map(|id| Term::from_field_u64(f.id, id)));
    Box::new(BooleanQuery::new(vec![
        (Occur::Must, candidates),
        (Occur::MustNot, Box::new(ids)),
    ]))
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
    let (ranges, mut rest): (Vec<&Node>, Vec<&Node>) = of.into_iter().partition(|n| liftable(n, f));
    // A clause drives when it narrows the matches by itself; a negation
    // (`-x`, `private:no`) only removes a few from everything.
    let drives = |n: &&Node| {
        !matches!(n, Node::Not { .. } | Node::Flag { value: false, .. })
            && !excludes_only(n)
            && compile_opt(n, f, now).is_some()
    };
    let keep = usize::from(!rest.iter().any(drives)).min(ranges.len());
    rest.extend(&ranges[..keep]);
    let filters = ranges[keep..]
        .iter()
        .chain(&checked)
        .map(|n| fast_range(n, f, now).expect("liftable"))
        .collect();
    // Whether a clause other than a lone extension drives the query, for
    // a lone extension's score below.
    let lone_ext = |n: &&Node| matches!(n, Node::File { of } if torrent_level(of, f));
    let others_drive = rest.iter().any(|n| drives(n) && !lone_ext(n));
    let mut scored_lone = false;
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
            // `verified:yes` (the hidden-by-default clause on every
            // search) and `network:bt`: their excluded terms join the
            // top-level conjunction as plain `MustNot`s, so it stays one
            // flat intersection and the score is untouched.
            n if excludes_only(n) => {
                for (field, t) in excluded(n, f) {
                    clauses.push((Occur::MustNot, term(field, t, IndexRecordOption::Basic)));
                }
            }
            // `file:(ext:…)` at the top level scores as the id set it
            // replaced did (X-search-files-4). Beside another clause that
            // drives, that set was lifted into the collector's filters and
            // added nothing: a constant in `text` would squeeze the
            // re-ranker's text differences. With nothing else to drive,
            // the first such set stayed in the query and scored 1.0
            // (`IdsQuery`), the text the priors multiply, and the rest
            // were lifted; all 0 would leave every hit at 0, unranked.
            Node::File { of } if torrent_level(of, f) => {
                if let Some(q) = torrent_level_query(of, f, now) {
                    let score = if others_drive || scored_lone {
                        0.0
                    } else {
                        1.0
                    };
                    scored_lone = true;
                    push_must(&mut clauses, Box::new(ConstScoreQuery::new(q, score)));
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
        // Combinations of them are one check too: `(seeders:>=5 |
        // trackers:>=3)` is checked on the candidates, not run as a
        // query that reads every document of the segment.
        Node::Or { of } | Node::And { of } => !of.is_empty() && of.iter().all(always_lifted),
        Node::Not { of } => always_lifted(of),
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
fn liftable(n: &Node, f: &Cx<'_>) -> bool {
    match n {
        Node::Range { field, .. } => field.is_fast() || field.is_hot(),
        Node::Date { .. } | Node::TimeOfDay { .. } => true,
        // `file:(ext:…)` and bucketed sizes are the torrent's own
        // terms, searched.
        Node::File { of } => !torrent_level(of, f),
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
        Node::Or { of } if always_lifted(node) => FastRange::Any(
            of.iter()
                .map(|n| fast_range(n, cx, now))
                .collect::<Option<_>>()?,
        ),
        Node::And { of } if always_lifted(node) => FastRange::All(
            of.iter()
                .map(|n| fast_range(n, cx, now))
                .collect::<Option<_>>()?,
        ),
        Node::Share { by, cat, lo, hi } => {
            let c = category(cat)?;
            let fld = match by {
                ShareBy::Count => cx.cat_count[c.index()],
                ShareBy::Bytes => cx.cat_bytes[c.index()],
            };
            FastRange::U64 {
                field: share_name(*by, cat)?,
                lo: u64::from(*lo),
                hi: u64::from(*hi),
                // Unwritten shares are 0, but a field the index lacks
                // matches nothing.
                missing: (fld != ABSENT).then_some(0),
            }
        }
        Node::File { of } if !torrent_level(of, cx) => FastRange::Ids { ids: cx.ids(of) },
        _ => return None,
    })
}

/// Whether a clause compiles to everything minus a few terms
/// (`verified:yes`, `network:bt`; see [`provenance`]), so like a negation
/// it shouldn't drive the query.
fn excludes_only(n: &Node) -> bool {
    match n {
        Node::Verified { any } => any.contains(&Verified::Yes),
        Node::Network { any } => any.contains(&Network::Bt),
        _ => false,
    }
}

/// The terms an [`excludes_only`] clause leaves out: it matches
/// everything without them.
fn excluded(n: &Node, f: &Cx<'_>) -> Vec<(Field, &'static str)> {
    match n {
        Node::Verified { any } if !any.contains(&Verified::No) => {
            vec![(f.verified, Verified::No.as_str())]
        }
        Node::Network { any } => Network::ALL
            .into_iter()
            .filter(|t| !any.contains(t))
            .map(|t| (f.network, t.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Everything but the documents with any of `terms`.
fn all_but(terms: Vec<(Field, &'static str)>) -> Q {
    let mut clauses: Vec<(Occur, Q)> = vec![(Occur::Must, Box::new(AllQuery))];
    clauses.extend(
        terms
            .into_iter()
            .map(|(fld, t)| (Occur::MustNot, term(fld, t, IndexRecordOption::Basic))),
    );
    if clauses.len() == 1 {
        return Box::new(AllQuery);
    }
    Box::new(BooleanQuery::new(clauses))
}

/// Any of `terms` in `field`.
fn any_term<'t>(field: Field, terms: impl Iterator<Item = &'t str>) -> Q {
    let mut clauses: Vec<(Occur, Q)> = terms
        .map(|t| (Occur::Should, term(field, t, IndexRecordOption::Basic)))
        .collect();
    match clauses.len() {
        0 => Box::new(EmptyQuery),
        1 => clauses.pop().expect("one").1,
        _ => Box::new(BooleanQuery::new(clauses)),
    }
}

/// `verified:`, `source:` and `network:` (§18.6, SRC-3).
///
/// While the population contract backfills these fields, documents
/// indexed before SRC-3 carry none of them, and every one of those is a
/// stored, verified clearnet torrent: unverified and other-network items
/// are only ever indexed with their fields. So the values a backfill
/// would write are compiled as "not any other value": `verified:yes` is
/// everything but `verified:no`, `network:bt` everything but the other
/// tiers. They're complete on a partly backfilled index, and hiding
/// unverified items by default never hides a torrent the walk hasn't
/// reached. `source:` has no such complement: it matches only documents
/// that carry it, and the search reports the field while it's partial
/// ([`Node::population_fields`]).
fn provenance(node: &Node, f: &Cx<'_>) -> Q {
    match node {
        n if excludes_only(n) => all_but(excluded(n, f)),
        Node::Verified { any } if any.contains(&Verified::No) => {
            term(f.verified, Verified::No.as_str(), IndexRecordOption::Basic)
        }
        Node::Verified { .. } => Box::new(EmptyQuery),
        Node::Network { any } => any_term(f.network, any.iter().map(|n| n.as_str())),
        Node::Source { any } => any_term(f.source, any.iter().map(String::as_str)),
        _ => unreachable!("provenance clauses only"),
    }
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
            if fld == ABSENT {
                return Some(Box::new(EmptyQuery));
            }
            let bound = |v: &Option<u64>| match v {
                Some(v) => Bound::Included(Term::from_field_u64(fld, *v)),
                None => Bound::Unbounded,
            };
            Box::new(RangeQuery::new(bound(lo), bound(hi)))
        }
        Node::Date { from, to, .. } => {
            if f.discovered == ABSENT {
                return Some(Box::new(EmptyQuery));
            }
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
        Node::Verified { .. } | Node::Source { .. } | Node::Network { .. } => provenance(node, f),
        Node::Share { by, cat, lo, hi } => {
            let Some(c) = category(cat) else {
                return Some(Box::new(EmptyQuery));
            };
            let fld = match by {
                ShareBy::Count => f.cat_count[c.index()],
                ShareBy::Bytes => f.cat_bytes[c.index()],
            };
            if fld == ABSENT {
                return Some(Box::new(EmptyQuery));
            }
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
        // A filter, scored as a file clause's id set is.
        Node::File { of } if torrent_level(of, f) => {
            Box::new(ConstScoreQuery::new(torrent_level_query(of, f, now)?, 1.0))
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
            Some(bytes) if f.hash != ABSENT => Box::new(TermQuery::new(
                Term::from_field_bytes(f.hash, &bytes),
                IndexRecordOption::Basic,
            )),
            _ => Box::new(EmptyQuery),
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
    if field == ABSENT {
        return Box::new(EmptyQuery);
    }
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
    let build = |fld: Field, positions: bool| -> Q {
        if fld == ABSENT {
            Box::new(EmptyQuery)
        } else {
            build(fld, positions)
        }
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
    if field == ABSENT {
        return Box::new(EmptyQuery);
    }
    match parts {
        [one] => term(field, &one.text, IndexRecordOption::WithFreqs),
        many if positions => {
            let first = many[0].position;
            // Guarded: tantivy asserts in a phrase excluded with `-`.
            Box::new(SeekGuard(PhraseQuery::new_with_offset_and_slop(
                many.iter()
                    .map(|t| (t.position - first, Term::from_field_text(field, &t.text)))
                    .collect(),
                slop,
            )))
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

#[cfg(test)]
mod negated_tests {
    use super::*;

    fn inner(q: &str) -> Node {
        match crate::parse(q).unwrap() {
            Node::File { of } => *of,
            n => panic!("{n:?}"),
        }
    }

    /// One extension atom and negated words not on the title, in any
    /// order; nothing else (X-search-files-6).
    #[test]
    fn negated_words_takes_only_its_shape() {
        let n = negated_words(&inner("file:(ext:mkv,mp4 -path:sample)")).unwrap();
        assert_eq!(n.exts, ["mkv", "mp4"]);
        assert_eq!(n.words.len(), 1);
        let n = negated_words(&inner("file:(-sample -name:x ext:mkv)")).unwrap();
        assert_eq!(n.words.len(), 2);
        assert!(negated_words(&inner("file:((ext:a | ext:b) -x)")).is_some());
        for q in [
            "file:(ext:mkv)",
            "file:(-sample)",
            "file:(-a -b)",
            "file:(ext:mkv ext:mp4 -sample)",
            "file:(ext:mkv -title:sample)",
            "file:(ext:mkv -\"a sample\")",
            "file:(ext:mkv -samp*)",
            "file:(ext:mkv -sample size:>1MB)",
            "file:(ext:mkv sample)",
            "file:(ext:mkv -(a | b))",
        ] {
            // The parser refuses some of these already (`title:`).
            let Ok(Node::File { of }) = crate::parse(q) else {
                continue;
            };
            assert_eq!(negated_words(&of), None, "{q}");
        }
    }
}
