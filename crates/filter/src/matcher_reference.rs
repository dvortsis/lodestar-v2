//! The matcher before compiled queries (§20.8), kept as the reference
//! the faster one is property-tested against.

use std::cell::OnceCell;

use lodestar_classify::{Category, FileInfo, Record};
use lodestar_search::DocEnrichment;
use lodestar_search::query::{DateField, NumField, ShareBy, TextField, When};
use lodestar_search::tokenizer::{Tok, TokKind, tokenize};
use lodestar_search::{Node, flag_names};

use super::Facts;
use crate::core::Item;

/// Whether `node` matches, as the previous matcher decided it.
pub(super) fn matches(item: &Item, facts: Option<&Facts>, node: &Node, neutral: &[String]) -> bool {
    Doc::new(item, facts).eval(node, None, neutral)
}

/// A torrent prepared for many rule evaluations: tokens and enrichment
/// are computed once, on first use.
pub(super) struct Doc<'a> {
    pub item: &'a Item<'a>,
    pub facts: Option<&'a Facts<'a>>,
    name: OnceCell<Vec<Tok>>,
    files: OnceCell<Vec<Vec<Tok>>>,
    titles: OnceCell<Vec<Vec<Tok>>>,
    enrichment: OnceCell<Option<DocEnrichment>>,
    content: OnceCell<Content>,
}

/// The content files' count, total size and largest size.
#[derive(Debug, Default)]
struct Content {
    count: u64,
    bytes: u64,
    largest: Option<u64>,
}

impl std::fmt::Debug for Doc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doc")
            .field("name", &self.item.name)
            .finish_non_exhaustive()
    }
}

impl<'a> Doc<'a> {
    pub fn new(item: &'a Item<'a>, facts: Option<&'a Facts<'a>>) -> Doc<'a> {
        Doc {
            item,
            facts,
            name: OnceCell::new(),
            files: OnceCell::new(),
            titles: OnceCell::new(),
            enrichment: OnceCell::new(),
            content: OnceCell::new(),
        }
    }

    fn name(&self) -> &[Tok] {
        self.name.get_or_init(|| tokenize(self.item.name))
    }

    fn files(&self) -> &[Vec<Tok>] {
        self.files.get_or_init(|| {
            self.item
                .files
                .iter()
                .take(crate::MAX_FILES)
                .map(|f| tokenize(&f.path.join("/")))
                .collect()
        })
    }

    fn enrichment(&self) -> Option<&DocEnrichment> {
        self.enrichment
            .get_or_init(|| {
                let f = self.facts?;
                Some(DocEnrichment::new(
                    &Record::new(f.class),
                    Some(&f.class.composition),
                ))
            })
            .as_ref()
    }

    fn titles(&self) -> &[Vec<Tok>] {
        self.titles.get_or_init(|| {
            self.enrichment()
                .map(|e| e.titles.iter().map(|t| tokenize(t)).collect())
                .unwrap_or_default()
        })
    }

    /// Whether `f` holds for any text of `field`: inside `file:(…)`, that
    /// file's name or path.
    fn any_text(&self, field: TextField, file: Option<usize>, f: impl Fn(&[Tok]) -> bool) -> bool {
        match file {
            Some(i) => {
                let file = &self.item.files[i];
                let toks = match field {
                    TextField::Name => tokenize(file.name()),
                    _ => tokenize(&file.path.join("/")),
                };
                f(&toks)
            }
            None => match field {
                TextField::Name => f(self.name()),
                TextField::Path => self.files().iter().any(|t| f(t)),
                TextField::Title => self.titles().iter().any(|t| f(t)),
                TextField::Any => f(self.name()) || self.files().iter().any(|t| f(t)),
            },
        }
    }

    fn eval(&self, node: &Node, file: Option<usize>, neutral: &[String]) -> bool {
        // Inside `file:(…)`, text and size clauses read that one file.
        let texts = |field: TextField, f: &dyn Fn(&[Tok]) -> bool| self.any_text(field, file, f);
        match node {
            Node::All => true,
            Node::And { of } => of.iter().all(|n| self.eval(n, file, neutral)),
            Node::Or { of } => of.iter().any(|n| self.eval(n, file, neutral)),
            Node::Not { of } => !self.eval(of, file, neutral),
            Node::Word { field, word } => {
                !is_neutral(word, neutral) && texts(*field, &|t| has_word(t, word))
            }
            Node::Phrase { field, text } => {
                !is_neutral(text, neutral) && texts(*field, &|t| has_phrase(t, text))
            }
            Node::Near { field, words, slop } => texts(*field, &|t| has_near(t, words, *slop)),
            Node::Prefix { field, prefix } => {
                let p = folded(prefix);
                !p.is_empty() && texts(*field, &|t| t.iter().any(|x| x.text.starts_with(&p)))
            }
            Node::Fuzzy {
                field,
                word,
                distance,
            } => {
                let w = folded(word);
                texts(*field, &|t| {
                    t.iter()
                        .any(|x| edit_distance(&x.text, &w) <= usize::from(*distance))
                })
            }
            Node::Range { field, lo, hi } => {
                let inside = |v: u64| lo.is_none_or(|l| v >= l) && hi.is_none_or(|h| v <= h);
                self.any_number(*field, file, inside)
            }
            Node::Date { field, from, to } => {
                let Some(f) = self.facts else { return false };
                let at = match field {
                    DateField::Discovered => f.discovered_at,
                    DateField::Seen => return false,
                };
                let resolve = |w: &When| match *w {
                    When::Ago(secs) => f.now - secs,
                    When::At(t) => t,
                };
                from.as_ref().is_none_or(|w| at >= resolve(w))
                    && to.as_ref().is_none_or(|w| at <= resolve(w))
            }
            Node::Ext { any } => {
                let has = |f: &FileInfo| {
                    lodestar_classify::ext::extension(f.name())
                        .is_some_and(|e| any.iter().any(|a| a == e.as_str()))
                };
                match file {
                    Some(i) => has(&self.item.files[i]),
                    None => self.item.files.iter().any(has),
                }
            }
            Node::Flag { name, value } => self.flag(name) == *value,
            Node::Hash { hash } => self.item.hashes.iter().any(|h| hex(h) == *hash),
            Node::TimeOfDay { from, to } => self.facts.is_some_and(|f| {
                lodestar_search::in_time(
                    lodestar_search::minute_of_day(f.discovered_at, 0),
                    *from,
                    *to,
                )
            }),
            Node::Tag { key, any } => self.enrichment().is_some_and(|e| {
                any.iter()
                    .any(|v| e.tags.contains(&format!("{}:{v}", key.as_str())))
            }),
            Node::File { of } => {
                file.is_none()
                    && (0..self.item.files.len().min(crate::MAX_FILES))
                        .any(|i| !self.item.files[i].padding && self.eval(of, Some(i), neutral))
            }
            Node::Share { by, cat, lo, hi } => {
                let Some(f) = self.facts else { return false };
                let Some(c) = Category::ALL.iter().find(|c| c.as_str() == cat) else {
                    return false;
                };
                let pct = match by {
                    ShareBy::Count => f.class.composition.pct_count(*c),
                    ShareBy::Bytes => f.class.composition.pct_bytes(*c),
                };
                (*lo..=*hi).contains(&pct)
            }
        }
    }

    /// Whether any value of a numeric field satisfies `f`. Enrichment is
    /// only built for the fields that need it.
    fn any_number(&self, field: NumField, file: Option<usize>, f: impl Fn(u64) -> bool) -> bool {
        if let (Some(i), NumField::Size) = (file, field) {
            return f(self.item.files[i].size);
        }
        let content = self.content.get_or_init(|| {
            let mut c = Content::default();
            for file in self.item.files.iter().filter(|f| !f.padding) {
                c.count += 1;
                c.bytes += file.size;
                c.largest = Some(c.largest.map_or(file.size, |l: u64| l.max(file.size)));
            }
            c
        });
        let list = |pick: fn(&DocEnrichment) -> &[u64]| {
            self.enrichment()
                .is_some_and(|e| pick(e).iter().any(|v| f(*v)))
        };
        match field {
            NumField::Size => f(self.facts.map_or(content.bytes, |x| x.size)),
            NumField::Files => f(content.count),
            NumField::Largest => content.largest.is_some_and(&f),
            NumField::Piece => self.facts.is_some_and(|x| f(x.piece_len)),
            NumField::Year => list(|e| &e.years),
            NumField::Season => list(|e| &e.seasons),
            NumField::Episode => list(|e| &e.episodes),
            NumField::Junk => self.enrichment().is_some_and(|e| f(e.junk)),
            NumField::Seeders | NumField::Leechers | NumField::Trackers | NumField::Dht => false,
        }
    }

    fn flag(&self, name: &str) -> bool {
        if name == "ambiguous" {
            return self.enrichment().is_some_and(|e| e.ambiguous);
        }
        self.facts
            .is_some_and(|f| flag_names(f.flags).any(|n| n == name))
    }
}

fn hex(h: &[u8; 20]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn folded(s: &str) -> String {
    tokenize(s)
        .into_iter()
        .map(|t| t.text)
        .collect::<Vec<_>>()
        .join("")
}

fn parts(text: &str) -> Vec<Tok> {
    tokenize(text)
        .into_iter()
        .filter(|t| t.kind == TokKind::Part)
        .collect()
}

fn is_neutral(word: &str, neutral: &[String]) -> bool {
    !neutral.is_empty() && neutral.iter().any(|n| folded(word) == *n)
}

/// The field tokens at each position, for phrase checks.
fn at(field: &[Tok], pos: usize, text: &str) -> bool {
    field.iter().any(|t| t.position == pos && t.text == text)
}

/// A word: its whole or compound form as one token, or its parts in a row.
fn has_word(field: &[Tok], word: &str) -> bool {
    let q = tokenize(word);
    if q.is_empty() {
        return false;
    }
    let whole = q.iter().filter(|t| t.kind != TokKind::Part);
    for w in whole {
        if field.iter().any(|t| t.text == w.text) {
            return true;
        }
    }
    let p: Vec<&Tok> = q.iter().filter(|t| t.kind == TokKind::Part).collect();
    sequence(
        field,
        &p.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
    )
}

fn sequence(field: &[Tok], words: &[&str]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    field.iter().filter(|t| t.text == *first).any(|start| {
        words
            .iter()
            .enumerate()
            .skip(1)
            .all(|(i, w)| at(field, start.position + i, w))
    })
}

fn has_phrase(field: &[Tok], text: &str) -> bool {
    let p = parts(text);
    sequence(
        field,
        &p.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
    )
}

/// Every word within a window of `slop` extra positions, in any order.
fn has_near(field: &[Tok], words: &[String], slop: u32) -> bool {
    let positions: Vec<Vec<usize>> = words
        .iter()
        .map(|w| {
            let f = folded(w);
            field
                .iter()
                .filter(|t| t.text == f)
                .map(|t| t.position)
                .collect()
        })
        .collect();
    if positions.iter().any(Vec::is_empty) {
        return false;
    }
    let span = words.len() + slop as usize;
    positions[0].iter().any(|&p0| {
        positions
            .iter()
            .all(|ps| ps.iter().any(|&p| p.abs_diff(p0) < span))
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == *cb {
                prev
            } else {
                1 + prev.min(row[j]).min(cur)
            };
            prev = cur;
        }
    }
    row[b.len()]
}
