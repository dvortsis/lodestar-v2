//! Query AST → Tantivy query (architecture §8).

use std::ops::Bound;

use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, EmptyQuery, FuzzyTermQuery, Occur, PhraseQuery, Query,
    RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};

use crate::query::{Node, NumField, TextField, When};
use crate::schema::{Fields, NAME_BOOST, PATH_BOOST};
use crate::tokenizer::{self, Tok, TokKind};

type Q = Box<dyn Query>;

/// Compiles `node`; `now` (unix seconds) resolves relative dates.
pub fn compile(node: &Node, f: &Fields, now: i64) -> Q {
    compile_opt(node, f, now).unwrap_or_else(|| Box::new(AllQuery))
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
                            clauses.push((Occur::Must, q));
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
            let parts: Vec<Tok> = tokenizer::tokenize(text)
                .into_iter()
                .filter(|t| t.kind == TokKind::Part)
                .collect();
            if parts.is_empty() {
                return None;
            }
            per_field(phrase_field(*field), f, |fld, positions| {
                parts_query(fld, &parts, positions, 0)
            })
        }
        Node::Near { field, words, slop } => {
            let mut parts = Vec::new();
            for w in words {
                let next = parts.last().map_or(0, |t: &Tok| t.position + 1);
                tokenizer::tokenize_into(w, next, &mut parts, 512);
                parts.retain(|t| t.kind == TokKind::Part);
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
            let at = |w: &When| match w {
                When::Ago(s) => now.saturating_sub(*s),
                When::At(t) => *t,
            };
            let bound = |w: &Option<When>| match w {
                Some(w) => Bound::Included(Term::from_field_i64(f.discovered, at(w))),
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

/// One word: its parts as a phrase (or all of them, in a field without
/// positions), or any compound it forms (`web-dl` ↔ `webdl`).
pub(crate) fn word_query(field: Field, toks: &[Tok], positions: bool) -> Q {
    let parts: Vec<Tok> = toks
        .iter()
        .filter(|t| t.kind == TokKind::Part)
        .cloned()
        .collect();
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
        .find(|t| t.kind == TokKind::Compound)
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
