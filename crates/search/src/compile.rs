//! Query AST → Tantivy query (architecture §8).

use std::ops::Bound;

use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, EmptyQuery, FuzzyTermQuery, Occur, PhraseQuery, Query,
    RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};

use crate::filter::FastRange;
use crate::query::{Node, NumField, TextField, When};
use crate::schema::{Fields, NAME_BOOST, PATH_BOOST};
use crate::tokenizer::{self, Tok, TokKind};

type Q = Box<dyn Query>;

/// Compiles `node`; `now` (unix seconds) resolves relative dates.
///
/// Ranges at the top level come back separately, to be checked in the
/// collector, whenever another clause can drive the query; otherwise the
/// first one stays in the query to drive it.
pub fn compile(node: &Node, f: &Fields, now: i64) -> (Q, Vec<FastRange>) {
    let all = || Box::new(AllQuery) as Q;
    let Node::And { of } = node else {
        return (compile_opt(node, f, now).unwrap_or_else(all), Vec::new());
    };
    let (ranges, mut rest): (Vec<&Node>, Vec<&Node>) = of
        .iter()
        .partition(|n| matches!(n, Node::Range { .. } | Node::Date { .. }));
    // A clause drives when it narrows the matches by itself; a negation
    // (`-x`, `private:no`) only removes a few from everything.
    let drives = |n: &&Node| {
        !matches!(n, Node::Not { .. } | Node::Flag { value: false, .. })
            && compile_opt(n, f, now).is_some()
    };
    let keep = usize::from(!rest.iter().any(drives)).min(ranges.len());
    rest.extend(&ranges[..keep]);
    let filters = ranges[keep..].iter().map(|n| fast_range(n, now)).collect();
    let rest = Node::And {
        of: rest.into_iter().cloned().collect(),
    };
    (compile_opt(&rest, f, now).unwrap_or_else(all), filters)
}

fn resolve(w: &When, now: i64) -> i64 {
    match w {
        When::Ago(s) => now.saturating_sub(*s),
        When::At(t) => *t,
    }
}

fn num_field(field: &NumField) -> &'static str {
    match field {
        NumField::Size => "size",
        NumField::Files => "files",
        NumField::Largest => "largest",
        NumField::Piece => "piece_len",
    }
}

/// A `Range` or `Date` node as a fast-field check.
fn fast_range(node: &Node, now: i64) -> FastRange {
    match node {
        Node::Range { field, lo, hi } => FastRange::U64 {
            field: num_field(field),
            lo: lo.unwrap_or(0),
            hi: hi.unwrap_or(u64::MAX),
        },
        Node::Date { from, to, .. } => FastRange::I64 {
            field: "discovered",
            lo: from.as_ref().map_or(i64::MIN, |w| resolve(w, now)),
            hi: to.as_ref().map_or(i64::MAX, |w| resolve(w, now)),
        },
        _ => unreachable!("only ranges are lifted"),
    }
}

/// `None` for a clause that constrains nothing (a word with no tokens,
/// like a lone `-`), so `And` can drop it.
fn compile_opt(node: &Node, f: &Fields, now: i64) -> Option<Q> {
    Some(match node {
        Node::All => Box::new(AllQuery),
        Node::And { of } => {
            let mut clauses: Vec<(Occur, Q)> = Vec::new();
            for n in of {
                match n {
                    Node::Not { of } => {
                        if let Some(q) = compile_opt(of, f, now) {
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
            if clauses.iter().all(|(o, _)| *o == Occur::MustNot) {
                clauses.push((Occur::Must, Box::new(AllQuery)));
            }
            Box::new(BooleanQuery::new(clauses))
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
            let q = compile_opt(of, f, now)?;
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Q),
                (Occur::MustNot, q),
            ]))
        }
        Node::Word {
            field: TextField::Any,
            word,
        } => {
            let toks = tokenizer::tokenize(word);
            if toks.is_empty() {
                return None;
            }
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, word_query(f.text, &toks, false)),
                (
                    Occur::Should,
                    Box::new(BoostQuery::new(word_query(f.name, &toks, true), NAME_BOOST)),
                ),
            ]))
        }
        Node::Word { field, word } => {
            let toks = tokenizer::tokenize(word);
            if toks.is_empty() {
                return None;
            }
            per_field(*field, f, |fld, positions| {
                word_query(fld, &toks, positions)
            })
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
        Node::Range { field, lo, hi } => {
            let fld = match field {
                NumField::Size => f.size,
                NumField::Files => f.files,
                NumField::Largest => f.largest,
                NumField::Piece => f.piece_len,
            };
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
fn units(toks: &[Tok]) -> Vec<Tok> {
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
fn parts_query(field: Field, parts: &[Tok], positions: bool, slop: u32) -> Q {
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
fn folded_word(word: &str) -> Option<String> {
    let toks = tokenizer::tokenize(word);
    toks.iter()
        .find(|t| t.kind == TokKind::Whole)
        .or_else(|| toks.iter().find(|t| t.kind == TokKind::Compound))
        .or_else(|| toks.first())
        .map(|t| t.text.clone())
}

fn regex_escape(s: &str) -> String {
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
