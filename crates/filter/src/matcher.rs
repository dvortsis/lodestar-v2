//! Rules are saved queries (§20.2): this evaluates a parsed query against
//! one torrent in memory, with the search tokenizer, so a rule matches
//! what the same query would find in search.
//!
//! Semantics follow the index:
//! - text fields are the name, the file paths (a phrase never spans two
//!   files) and the effective and alternative titles;
//! - `file:(…)` needs one file to match every inner clause;
//! - tags, years, seasons, episodes, shares and flags come from the same
//!   enrichment the index stores ([`DocEnrichment`]).
//!
//! The one difference: fuzzy matching here is plain edit distance on the
//! folded tokens.

use std::cell::OnceCell;

use lodestar_classify::{Category, Classification, FileInfo, Record};
use lodestar_search::DocEnrichment;
use lodestar_search::query::{DateField, NumField, ShareBy, TextField, When};
use lodestar_search::tokenizer::{Tok, TokKind, tokenize};
use lodestar_search::{Node, flag_names};

use crate::core::Item;

/// What the rules can read beyond the core's [`Item`].
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    pub class: &'a Classification,
    pub size: u64,
    pub piece_len: u64,
    /// `torrents.flags` bits.
    pub flags: i64,
    pub discovered_at: i64,
    /// For relative dates.
    pub now: i64,
}

/// A torrent prepared for many rule evaluations: tokens and enrichment
/// are computed once, on first use.
pub struct Doc<'a> {
    pub item: &'a Item<'a>,
    pub facts: Option<&'a Facts<'a>>,
    name: OnceCell<Vec<Tok>>,
    files: OnceCell<Vec<Vec<Tok>>>,
    titles: OnceCell<Vec<Vec<Tok>>>,
    enrichment: OnceCell<Option<DocEnrichment>>,
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

    /// The token lists a text field covers.
    fn texts(&self, field: TextField) -> Vec<&[Tok]> {
        match field {
            TextField::Name => vec![self.name()],
            TextField::Path => self.files().iter().map(Vec::as_slice).collect(),
            TextField::Title => self.titles().iter().map(Vec::as_slice).collect(),
            TextField::Any => std::iter::once(self.name())
                .chain(self.files().iter().map(Vec::as_slice))
                .collect(),
        }
    }

    /// Whether the query matches the torrent. `neutral` words never count
    /// (CSX-5 *Neutralize term*, §20.5.5).
    pub fn matches(&self, node: &Node, neutral: &[String]) -> bool {
        self.eval(node, None, neutral)
    }

    fn eval(&self, node: &Node, file: Option<usize>, neutral: &[String]) -> bool {
        // Inside `file:(…)`, text and size clauses read that one file.
        let file_texts = |field: TextField| -> Vec<Vec<Tok>> {
            let f = &self.item.files[file.expect("in a file clause")];
            match field {
                TextField::Name => vec![tokenize(f.name())],
                _ => vec![tokenize(&f.path.join("/"))],
            }
        };
        let texts = |field: TextField| -> Vec<Vec<Tok>> {
            match file {
                Some(_) => file_texts(field),
                None => self.texts(field).into_iter().map(<[Tok]>::to_vec).collect(),
            }
        };
        match node {
            Node::All => true,
            Node::And { of } => of.iter().all(|n| self.eval(n, file, neutral)),
            Node::Or { of } => of.iter().any(|n| self.eval(n, file, neutral)),
            Node::Not { of } => !self.eval(of, file, neutral),
            Node::Word { field, word } => {
                !is_neutral(word, neutral) && texts(*field).iter().any(|t| has_word(t, word))
            }
            Node::Phrase { field, text } => {
                !is_neutral(text, neutral) && texts(*field).iter().any(|t| has_phrase(t, text))
            }
            Node::Near { field, words, slop } => {
                texts(*field).iter().any(|t| has_near(t, words, *slop))
            }
            Node::Prefix { field, prefix } => {
                let p = folded(prefix);
                !p.is_empty()
                    && texts(*field)
                        .iter()
                        .any(|t| t.iter().any(|x| x.text.starts_with(&p)))
            }
            Node::Fuzzy {
                field,
                word,
                distance,
            } => {
                let w = folded(word);
                texts(*field).iter().any(|t| {
                    t.iter()
                        .any(|x| edit_distance(&x.text, &w) <= usize::from(*distance))
                })
            }
            Node::Range { field, lo, hi } => {
                let inside = |v: u64| lo.is_none_or(|l| v >= l) && hi.is_none_or(|h| v <= h);
                self.numbers(*field, file).into_iter().any(inside)
            }
            Node::Date { field, from, to } => {
                let Some(f) = self.facts else { return false };
                let at = match field {
                    DateField::Discovered => f.discovered_at,
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

    fn numbers(&self, field: NumField, file: Option<usize>) -> Vec<u64> {
        if let (Some(i), NumField::Size) = (file, field) {
            return vec![self.item.files[i].size];
        }
        let content = || self.item.files.iter().filter(|f| !f.padding);
        let e = self.enrichment();
        match field {
            NumField::Size => vec![
                self.facts
                    .map_or_else(|| content().map(|f| f.size).sum(), |f| f.size),
            ],
            NumField::Files => vec![content().count() as u64],
            NumField::Largest => content().map(|f| f.size).max().into_iter().collect(),
            NumField::Piece => self.facts.map(|f| f.piece_len).into_iter().collect(),
            NumField::Year => e.map(|e| e.years.clone()).unwrap_or_default(),
            NumField::Season => e.map(|e| e.seasons.clone()).unwrap_or_default(),
            NumField::Episode => e.map(|e| e.episodes.clone()).unwrap_or_default(),
            NumField::Junk => e.map(|e| e.junk).into_iter().collect(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::{ContentType, classify};
    use lodestar_search::parse;

    fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
        paths
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect()
    }

    fn check(name: &str, paths: &[(&str, u64)], query: &str) -> bool {
        let f = files(paths);
        let class = classify(name, &f);
        let facts = Facts {
            class: &class,
            size: f.iter().map(|f| f.size).sum(),
            piece_len: 1 << 20,
            flags: lodestar_store::flags::PRIVATE,
            discovered_at: 1_000_000,
            now: 1_000_000 + 3600,
        };
        let item = Item {
            hashes: &[[0xab; 20]],
            name,
            files: &f,
            content_type: class.content_type,
        };
        Doc::new(&item, Some(&facts)).matches(&parse(query).unwrap(), &[])
    }

    const MOVIE: &str = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    const FILES: &[(&str, u64)] = &[
        ("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30),
        ("Sample/sample.mkv", 50 << 20),
        ("readme.txt", 100),
    ];

    #[test]
    fn text() {
        assert!(check(MOVIE, FILES, "matrix"));
        assert!(check(MOVIE, FILES, "\"the matrix\""));
        assert!(!check(MOVIE, FILES, "\"matrix the\""));
        assert!(check(MOVIE, FILES, "x264"));
        assert!(check(MOVIE, FILES, "mat*"));
        assert!(check(MOVIE, FILES, "~matirx"));
        assert!(check(MOVIE, FILES, "path:readme"));
        assert!(!check(MOVIE, FILES, "name:readme"));
        assert!(check(MOVIE, FILES, "title:matrix"));
        assert!(check(MOVIE, FILES, "matrix NEAR/3 bluray"));
        assert!(!check(MOVIE, FILES, "matrix NEAR/1 bluray"));
        assert!(check(MOVIE, FILES, "matrix -cam"));
        assert!(!check(MOVIE, FILES, "matrix cam"));
        assert!(check(MOVIE, FILES, "cam | matrix"));
    }

    #[test]
    fn fields() {
        assert!(check(MOVIE, FILES, "type:movie res:1080p year:1999"));
        assert!(!check(MOVIE, FILES, "type:tv"));
        assert!(check(MOVIE, FILES, "size:>4GB files:3 ext:mkv"));
        assert!(!check(MOVIE, FILES, "ext:exe"));
        assert!(check(MOVIE, FILES, "has:sample private:yes"));
        assert!(check(MOVIE, FILES, "bytes:video>90%"));
        assert!(check(MOVIE, FILES, "discovered:<1d"));
        assert!(!check(MOVIE, FILES, "discovered:<30m"));
        assert!(check(MOVIE, FILES, &format!("hash:{}", "ab".repeat(20))));
        assert!(check(MOVIE, FILES, "file:(ext:mkv size:>1GB)"));
        assert!(!check(MOVIE, FILES, "file:(ext:txt size:>1GB)"));
        assert!(check(MOVIE, FILES, "file:(sample ext:mkv)"));
        assert!(check(MOVIE, FILES, "junk:<20"));
    }

    #[test]
    fn neutralized_words_never_count() {
        let f = files(&[("a.mkv", 1)]);
        let item = Item {
            hashes: &[[0; 20]],
            name: "Alpha Beta",
            files: &f,
            content_type: ContentType::Video,
        };
        let doc = Doc::new(&item, None);
        let q = parse("alpha").unwrap();
        assert!(doc.matches(&q, &[]));
        assert!(!doc.matches(&q, &["alpha".to_owned()]));
        assert!(doc.matches(&parse("beta").unwrap(), &["alpha".to_owned()]));
    }

    #[test]
    fn distance() {
        assert_eq!(edit_distance("matrix", "matirx"), 2);
        assert_eq!(edit_distance("", "ab"), 2);
        assert_eq!(edit_distance("abc", "abc"), 0);
    }
}
