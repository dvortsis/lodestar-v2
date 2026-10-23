//! Rules are saved queries (§20.2): this evaluates a parsed query against
//! one torrent in memory, with the search tokenizer's tokens, so a rule
//! matches what the same query would find in search.
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
//!
//! Speed (§20.8): a query is [`compile`]d once, with its words already
//! tokenized and hashed. Before the full match, [`Doc`] checks it
//! against what the torrent has: its token texts, which token follows
//! which, its extensions, sizes and tags. That check says *no* only when
//! the full match must say no, so most rules that can't match cost a few
//! hash lookups. It never says *yes* from a hash, so a token whose hash
//! collides with a word can't make a rule match, or stop one matching.

use std::cell::OnceCell;

use lodestar_classify::{Category, Classification, FileInfo, Record};
use lodestar_search::DocEnrichment;
use lodestar_search::query::{DateField, NumField, ShareBy, TextField, When};
use lodestar_search::tokenizer::{Tok, TokKind, tokenize};
use lodestar_search::{Node, flag_names};

use crate::core::Item;
use crate::fasttok::{self, Text};
use crate::hash::{IdSet, term_hash};
use crate::prefilter::{self, DocKeys, Gate, NearTerms, RuleList};

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

/// A token text of a query, with its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Term {
    hash: u64,
    text: Box<str>,
}

impl Term {
    fn new(text: &str) -> Term {
        Term {
            hash: term_hash(text),
            text: text.into(),
        }
    }
}

/// A query ready to evaluate: words tokenized, tags spelled out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Compiled {
    All,
    And(Vec<Compiled>),
    Or(Vec<Compiled>),
    Not(Box<Compiled>),
    Word {
        field: TextField,
        /// Whole and compound forms: any one as a token matches.
        whole: Vec<Term>,
        /// Or the parts in a row.
        parts: Vec<Term>,
        /// What CSX-5 neutralizing compares.
        fold: Box<str>,
    },
    Phrase {
        field: TextField,
        parts: Vec<Term>,
        fold: Box<str>,
    },
    Near {
        field: TextField,
        words: Vec<Operand>,
        slop: u32,
    },
    Prefix {
        field: TextField,
        prefix: Box<str>,
        /// Its first characters' key ([`prefilter::start_key`]).
        start: Option<u64>,
    },
    Fuzzy {
        field: TextField,
        word: Spelled,
        distance: u8,
    },
    Range {
        field: NumField,
        lo: Option<u64>,
        hi: Option<u64>,
    },
    Ext {
        any: Vec<String>,
    },
    Tag {
        keys: Vec<u64>,
        tags: Vec<String>,
    },
    File(Box<Compiled>),
    /// Dates, flags, hashes and shares, read as they are.
    Leaf(Box<Node>),
}

/// A word inside NEAR: its parts in a row (a phrase), or one token of
/// its whole text (`webdl` for `web-dl`), as a word matches (§16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operand {
    parts: Vec<Term>,
    whole: Vec<Term>,
}

/// A NEAR word's parts, and the compound or whole forms that cover all
/// of them (not a compound of only some, like `webdl` in `web-dl.x264`).
/// No parts, no forms: such a word matches nothing.
pub(crate) fn near_operand(word: &str) -> (Vec<String>, Vec<String>) {
    let q = tokenize(word);
    let parts: Vec<&Tok> = q.iter().filter(|t| t.kind == TokKind::Part).collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return (Vec::new(), Vec::new());
    };
    let whole = q
        .iter()
        .filter(|t| t.kind != TokKind::Part && t.from <= first.from && t.to >= last.to);
    (
        parts.iter().map(|t| t.text.clone()).collect(),
        whole.map(|t| t.text.clone()).collect(),
    )
}

/// A word for fuzzy matching, with what rules most tokens out before an
/// edit distance: its length and the characters it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spelled {
    chars: Vec<char>,
    has: u64,
}

impl Spelled {
    fn new(text: &str) -> Spelled {
        Spelled {
            chars: text.chars().collect(),
            has: prefilter::char_set(text),
        }
    }

    /// Whether `text` is within `distance` edits.
    fn near(&self, text: &str, distance: u8) -> bool {
        self.near_counted(
            text,
            text.chars().count(),
            prefilter::char_set(text),
            distance,
        )
    }

    /// [`Spelled::near`] for a text of `chars` characters with these
    /// characters present ([`prefilter::char_set`]). Each edit changes
    /// the length by at most one and the characters present by at most
    /// two (one gone, one new; folding characters onto shared bits only
    /// merges them), so texts further off on either count are ruled out
    /// without computing the distance.
    #[inline]
    fn near_counted(&self, text: &str, chars: usize, has: u64, distance: u8) -> bool {
        let d = usize::from(distance);
        chars.abs_diff(self.chars.len()) <= d
            && (has ^ self.has).count_ones() as usize <= 2 * d
            && edit_distance(text, &self.chars) <= d
    }
}

/// Every token text of a query word, joined.
fn folded(s: &str) -> String {
    tokenize(s)
        .into_iter()
        .map(|t| t.text)
        .collect::<Vec<_>>()
        .join("")
}

/// Compiles a parsed query.
pub(crate) fn compile(node: &Node) -> Compiled {
    let terms = |toks: &mut dyn Iterator<Item = String>| toks.map(|t| Term::new(&t)).collect();
    match node {
        Node::All => Compiled::All,
        Node::And { of } => {
            // Clauses that read no text first: they're cheapest to check,
            // and an AND is the same in any order.
            let mut of: Vec<Compiled> = of.iter().map(compile).collect();
            of.sort_by_key(|c| {
                !matches!(
                    c,
                    Compiled::Range { .. }
                        | Compiled::Ext { .. }
                        | Compiled::Tag { .. }
                        | Compiled::Leaf(_)
                )
            });
            Compiled::And(of)
        }
        Node::Or { of } => Compiled::Or(of.iter().map(compile).collect()),
        Node::Not { of } => Compiled::Not(Box::new(compile(of))),
        Node::Word { field, word } => {
            let q = tokenize(word);
            Compiled::Word {
                field: *field,
                whole: terms(
                    &mut q
                        .iter()
                        .filter(|t| t.kind != TokKind::Part)
                        .map(|t| t.text.clone()),
                ),
                parts: terms(
                    &mut q
                        .iter()
                        .filter(|t| t.kind == TokKind::Part)
                        .map(|t| t.text.clone()),
                ),
                fold: folded(word).into(),
            }
        }
        Node::Phrase { field, text } => Compiled::Phrase {
            field: *field,
            parts: terms(
                &mut tokenize(text)
                    .into_iter()
                    .filter(|t| t.kind == TokKind::Part)
                    .map(|t| t.text),
            ),
            fold: folded(text).into(),
        },
        Node::Near { field, words, slop } => Compiled::Near {
            field: *field,
            words: words
                .iter()
                .map(|w| {
                    let (parts, whole) = near_operand(w);
                    Operand {
                        parts: parts.iter().map(|t| Term::new(t)).collect(),
                        whole: whole.iter().map(|t| Term::new(t)).collect(),
                    }
                })
                .collect(),
            slop: *slop,
        },
        Node::Prefix { field, prefix } => {
            let prefix = folded(prefix);
            Compiled::Prefix {
                field: *field,
                start: prefilter::start_key(&prefix),
                prefix: prefix.into(),
            }
        }
        Node::Fuzzy {
            field,
            word,
            distance,
        } => Compiled::Fuzzy {
            field: *field,
            word: Spelled::new(&folded(word)),
            distance: *distance,
        },
        Node::Range { field, lo, hi } => Compiled::Range {
            field: *field,
            lo: *lo,
            hi: *hi,
        },
        Node::Ext { any } => Compiled::Ext { any: any.clone() },
        Node::Tag { key, any } => {
            let tags: Vec<String> = any
                .iter()
                .map(|v| format!("{}:{v}", key.as_str()))
                .collect();
            Compiled::Tag {
                keys: tags.iter().map(|t| prefilter::tag_key(t)).collect(),
                tags,
            }
        }
        Node::File { of } => Compiled::File(Box::new(compile(of))),
        Node::Date { .. } | Node::Flag { .. } | Node::Hash { .. } | Node::Share { .. } => {
            Compiled::Leaf(Box::new(node.clone()))
        }
    }
}

/// Each NEAR in `q`, as the hashes of each word's tokens and its span
/// (words plus slop), whatever field it reads.
pub(crate) fn near_words(q: &Compiled, f: &mut dyn FnMut(Vec<Vec<u64>>, u32)) {
    match q {
        Compiled::And(of) | Compiled::Or(of) => of.iter().for_each(|c| near_words(c, f)),
        Compiled::Not(of) | Compiled::File(of) => near_words(of, f),
        Compiled::Near { words, slop, .. } => {
            let hashes = words
                .iter()
                .map(|w| w.parts.iter().chain(&w.whole).map(|t| t.hash).collect())
                .collect();
            f(hashes, words.len() as u32 + slop);
        }
        _ => {}
    }
}

/// A [`Gate`] of a compiled query: a clause every match needs, which the
/// cheap check answers *no* for when the gate isn't passed
/// ([`Doc::passes`]). Inside `file:(…)` (`in_file`) every text field is
/// keyed, and a size is a file's.
pub(crate) fn gate(q: &Compiled, clauses: &mut prefilter::Clauses) -> Gate {
    gate_in(q, false, clauses)
}

fn gate_in(q: &Compiled, in_file: bool, clauses: &mut prefilter::Clauses) -> Gate {
    let keyed = |field: &TextField| in_file || *field != TextField::Title;
    let bounds = |lo: &Option<u64>, hi: &Option<u64>| (lo.unwrap_or(0), hi.unwrap_or(u64::MAX));
    let mut clause = |c: &Compiled| clauses.add(c);
    match q {
        // Every clause of an AND is needed.
        Compiled::And(of) => of
            .iter()
            .fold(Gate::None, |g, c| g.better(gate_in(c, in_file, clauses))),
        Compiled::File(of) => gate_in(of, true, clauses),
        Compiled::Range {
            field: NumField::Size,
            lo,
            hi,
        } => {
            let (lo, hi) = bounds(lo, hi);
            if in_file {
                Gate::FileSize(lo, hi)
            } else {
                Gate::Size(lo, hi)
            }
        }
        Compiled::Tag { keys, .. } if keys.len() == 1 => Gate::Tag(keys[0]),
        Compiled::Phrase { field, parts, .. } if keyed(field) && parts.len() >= 2 => {
            Gate::Pair(parts[0].hash, parts[1].hash)
        }
        // A negated word anywhere, read as [`Doc::word_anywhere`] reads it.
        Compiled::Not(of)
            if !in_file
                && matches!(
                    **of,
                    Compiled::Word {
                        field: TextField::Any,
                        ..
                    }
                ) =>
        {
            Gate::Lacks(clause(of))
        }
        Compiled::Prefix {
            field,
            start: Some(_),
            ..
        } if keyed(field) => Gate::Prefix(clause(q)),
        Compiled::Fuzzy { field, word, .. } if keyed(field) && !word.chars.is_empty() => {
            Gate::Fuzzy(clause(q))
        }
        Compiled::Near { field, .. } if keyed(field) => Gate::Near(clause(q)),
        Compiled::Word {
            field: TextField::Title,
            ..
        } if !in_file => Gate::Title(clause(q)),
        _ => Gate::None,
    }
}

/// [`compile`], remembered: *Always allow*, CSX-5 scopes and exceptions
/// are evaluated for every torrent, and compiling tokenizes. Remembered
/// by the query's address, and used only while the query there is still
/// equal to the one compiled, so an address reused for another query
/// compiles afresh.
fn compiled(node: &Node) -> std::rc::Rc<Compiled> {
    use std::collections::HashMap;
    use std::rc::Rc;
    thread_local! {
        static SEEN: std::cell::RefCell<HashMap<usize, (Node, Rc<Compiled>)>> =
            Default::default();
    }
    let at = node as *const Node as usize;
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if let Some((n, c)) = seen.get(&at) {
            if n == node {
                return c.clone();
            }
        }
        if seen.len() >= 4096 {
            seen.clear();
        }
        let c = Rc::new(compile(node));
        seen.insert(at, (node.clone(), c.clone()));
        c
    })
}

/// What the cheap check knows: *no* and *yes* are certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    No,
    Maybe,
    Yes,
}

impl Tri {
    fn of(b: bool) -> Tri {
        if b { Tri::Yes } else { Tri::No }
    }
}

/// A torrent prepared for many rule evaluations: tokens and enrichment
/// are computed once, on first use.
pub struct Doc<'a> {
    pub item: &'a Item<'a>,
    pub facts: Option<&'a Facts<'a>>,
    name: OnceCell<Text>,
    /// Each file's whole path.
    files: OnceCell<Vec<Text>>,
    /// Each file's own name.
    file_names: OnceCell<Vec<Text>>,
    titles: OnceCell<Vec<Text>>,
    /// The name and each path in the core's tokens, for keyword entries.
    normalized: OnceCell<Vec<Vec<crate::normalize::Token>>>,
    enrichment: OnceCell<Option<DocEnrichment>>,
    keys: OnceCell<DocKeys>,
    /// The rule set's NEAR terms, whose nearness the keys record.
    near: Option<&'a std::sync::Arc<NearTerms>>,
    tag_keys: OnceCell<IdSet>,
    exts: OnceCell<Vec<lodestar_classify::ext::Ext>>,
    content: OnceCell<Content>,
}

/// The content files' count, total size and largest size, and the
/// smallest and largest of the sizes `file:(…)` reads.
#[derive(Debug, Default)]
struct Content {
    count: u64,
    bytes: u64,
    largest: Option<u64>,
    file_sizes: (u64, u64),
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
            file_names: OnceCell::new(),
            titles: OnceCell::new(),
            normalized: OnceCell::new(),
            enrichment: OnceCell::new(),
            keys: OnceCell::new(),
            near: None,
            tag_keys: OnceCell::new(),
            exts: OnceCell::new(),
            content: OnceCell::new(),
        }
    }

    fn keys(&self) -> &DocKeys {
        self.keys
            .get_or_init(|| DocKeys::with_near(self.item.name, self.item.files, self.near))
    }

    fn tag_keys(&self) -> &IdSet {
        self.tag_keys
            .get_or_init(|| prefilter::tag_keys(self.enrichment()))
    }

    fn has_key(&self, t: &Term) -> bool {
        self.keys().has_text(t.hash)
    }

    /// The keys record how close `near`'s terms come (a rule set's NEAR
    /// words, [`RuleList::near_terms`]). Before the keys are built.
    pub(crate) fn with_near(mut self, near: Option<&'a std::sync::Arc<NearTerms>>) -> Doc<'a> {
        self.near = near;
        self
    }

    /// The positions of your rules that could match, in order.
    /// `neutral` words never count (CSX-5 *Neutralize term*).
    pub(crate) fn candidates(&self, rules: &RuleList, neutral: &[String]) -> Vec<u32> {
        rules.candidates(self.keys(), &|| self.tag_keys(), |g, clauses| {
            self.passes(g, clauses, neutral)
        })
    }

    /// Whether the cheap check would let a clause with this [`gate`]
    /// through.
    pub(crate) fn passes(&self, g: Gate, clauses: &[Compiled], neutral: &[String]) -> bool {
        match g {
            Gate::None => true,
            Gate::Size(lo, hi) => (lo..=hi).contains(&self.total_size()),
            // The sizes `file:(…)` reads lie between these: the cheap
            // check's first test.
            Gate::FileSize(lo, hi) => {
                let (min, max) = self.content().file_sizes;
                lo <= max && hi >= min
            }
            Gate::Tag(k) => self.tag_keys().contains(&k),
            Gate::Pair(a, b) => self.keys().could_follow(a, b),
            Gate::Lacks(i) => match &clauses[i as usize] {
                Compiled::Word {
                    whole, parts, fold, ..
                } => {
                    neutral.iter().any(|n| **n == **fold)
                        || self.word_anywhere(whole, parts) != Some(true)
                }
                _ => true,
            },
            // Every text a prefix or fuzzy word reads is among the
            // distinct token texts, except titles, which aren't gated.
            Gate::Prefix(i) => match &clauses[i as usize] {
                Compiled::Prefix {
                    prefix,
                    start: Some(k),
                    ..
                } => {
                    let keys = self.keys();
                    !prefix.is_empty() && keys.could_start(*k) && keys.has_prefix(prefix, *k)
                }
                _ => true,
            },
            Gate::Near(i) => match &clauses[i as usize] {
                Compiled::Near { words, slop, .. } => {
                    words.iter().all(|w| self.could_have(&w.whole, &w.parts))
                        && !self.never_near(words, words.len() as u32 + slop)
                }
                _ => true,
            },
            Gate::Title(i) => match &clauses[i as usize] {
                Compiled::Word {
                    whole, parts, fold, ..
                } => {
                    !neutral.iter().any(|n| **n == **fold)
                        && self.titles().iter().any(|t| has_word(t, whole, parts))
                }
                _ => true,
            },
            Gate::Fuzzy(i) => match &clauses[i as usize] {
                Compiled::Fuzzy { word, distance, .. } => {
                    self.keys()
                        .could_be_near(word.chars.len(), word.has, usize::from(*distance))
                }
                _ => true,
            },
        }
    }

    fn name(&self) -> &Text {
        self.name.get_or_init(|| fasttok::tokens(self.item.name))
    }

    fn files(&self) -> &[Text] {
        self.files.get_or_init(|| {
            self.item
                .files
                .iter()
                .take(crate::MAX_FILES)
                .map(|f| fasttok::tokens(&f.path.join("/")))
                .collect()
        })
    }

    fn file_names(&self) -> &[Text] {
        self.file_names.get_or_init(|| {
            self.item
                .files
                .iter()
                .take(crate::MAX_FILES)
                .map(|f| fasttok::tokens(f.name()))
                .collect()
        })
    }

    /// The name, then each path, in the core's tokens.
    pub(crate) fn normalized(&self) -> &[Vec<crate::normalize::Token>] {
        self.normalized.get_or_init(|| {
            let text = |t: &str| {
                let mut out = Vec::new();
                crate::normalize::tokens(t, &mut out);
                out
            };
            std::iter::once(text(self.item.name))
                .chain(
                    self.item
                        .files
                        .iter()
                        .take(crate::MAX_FILES)
                        .map(|f| text(&f.path.join("/"))),
                )
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

    fn titles(&self) -> &[Text] {
        self.titles.get_or_init(|| {
            self.enrichment()
                .map(|e| e.titles.iter().map(|t| fasttok::tokens(t)).collect())
                .unwrap_or_default()
        })
    }

    /// Whether a word could be here: one of its whole or compound forms,
    /// or all its parts, each able to follow the one before.
    fn could_have(&self, whole: &[Term], parts: &[Term]) -> bool {
        whole.iter().any(|t| self.has_key(t))
            || (!parts.is_empty()
                && parts.iter().all(|t| self.has_key(t))
                && self.could_follow(parts))
    }

    /// Whether some word never comes within `span - 1` positions of the
    /// first in any text, by the nearness the keys recorded: a NEAR's
    /// places within its window have a token of each that close (two
    /// ends `gap` apart, or a shared position). Only for words all of
    /// whose tokens were recorded, within the recorded window.
    fn never_near(&self, words: &[Operand], span: u32) -> bool {
        let Some(near) = self.keys().nearness() else {
            return false;
        };
        fn terms(w: &Operand) -> impl Iterator<Item = u64> + '_ {
            w.parts.iter().chain(&w.whole).map(|t| t.hash)
        }
        let Some((first, rest)) = words.split_first() else {
            return false;
        };
        if span == 0
            || span - 1 > near.window()
            || !words.iter().all(|w| terms(w).all(|h| near.covers(h)))
        {
            return false;
        }
        rest.iter()
            .any(|w| !terms(first).any(|x| terms(w).any(|y| near.within(x, y, span - 1))))
    }

    /// Whether these parts could follow one another somewhere.
    fn could_follow(&self, parts: &[Term]) -> bool {
        parts.len() < 2
            || parts
                .windows(2)
                .all(|w| self.keys().could_follow(w[0].hash, w[1].hash))
    }

    fn exts(&self) -> &[lodestar_classify::ext::Ext] {
        self.exts.get_or_init(|| {
            let mut v = Vec::new();
            for f in self.item.files {
                if let Some(e) = lodestar_classify::ext::extension(f.name()) {
                    if !v.contains(&e) {
                        v.push(e);
                    }
                }
            }
            v
        })
    }

    /// Whether the query matches the torrent. `neutral` words never count
    /// (CSX-5 *Neutralize term*, §20.5.5).
    pub fn matches(&self, node: &Node, neutral: &[String]) -> bool {
        self.matches_compiled(&compiled(node), neutral)
    }

    /// [`Doc::matches`] for a compiled query.
    pub(crate) fn matches_compiled(&self, q: &Compiled, neutral: &[String]) -> bool {
        self.could_match(q) && self.eval(q, None, neutral)
    }

    /// Whether the cheap check leaves the full match to decide.
    pub(crate) fn could_match(&self, q: &Compiled) -> bool {
        self.check(q, false) != Tri::No
    }

    /// The full match, without the cheap check first.
    #[cfg(test)]
    pub(crate) fn eval_full(&self, node: &Node) -> bool {
        self.eval(&compile(node), None, &[])
    }

    /// The cheap check. Inside `file:(…)` (`in_file`), *yes* and *no*
    /// must hold for every file alike.
    fn check(&self, q: &Compiled, in_file: bool) -> Tri {
        // Titles aren't among the keys, except inside `file:(…)`, where
        // every text field reads that file's name or path.
        let keyed = |field: &TextField| in_file || *field != TextField::Title;
        match q {
            Compiled::All => Tri::Yes,
            Compiled::And(of) => {
                let mut all = Tri::Yes;
                for c in of {
                    match self.check(c, in_file) {
                        Tri::No => return Tri::No,
                        Tri::Maybe => all = Tri::Maybe,
                        Tri::Yes => {}
                    }
                }
                all
            }
            Compiled::Or(of) => {
                let mut any = Tri::No;
                for c in of {
                    match self.check(c, in_file) {
                        Tri::Yes => return Tri::Yes,
                        Tri::Maybe => any = Tri::Maybe,
                        Tri::No => {}
                    }
                }
                any
            }
            Compiled::Not(of) => match self.check(of, in_file) {
                Tri::Yes => Tri::No,
                Tri::No => Tri::Yes,
                Tri::Maybe => Tri::Maybe,
            },
            Compiled::Word {
                field,
                whole,
                parts,
                ..
            } if keyed(field) => {
                if self.could_have(whole, parts) {
                    Tri::Maybe
                } else {
                    Tri::No
                }
            }
            Compiled::Phrase { field, parts, .. } if keyed(field) => {
                if parts.is_empty()
                    || !parts.iter().all(|t| self.has_key(t))
                    || !self.could_follow(parts)
                {
                    Tri::No
                } else {
                    Tri::Maybe
                }
            }
            Compiled::Prefix {
                field,
                start: Some(k),
                ..
            } if keyed(field) && !self.keys().could_start(*k) => Tri::No,
            Compiled::Near { field, words, slop } if keyed(field) => {
                if words.iter().all(|w| self.could_have(&w.whole, &w.parts))
                    && !self.never_near(words, words.len() as u32 + slop)
                {
                    Tri::Maybe
                } else {
                    Tri::No
                }
            }
            Compiled::Range { field, lo, hi } => self.check_range(*field, *lo, *hi, in_file),
            Compiled::Ext { any } => {
                let has = self
                    .exts()
                    .iter()
                    .any(|e| any.iter().any(|a| a == e.as_str()));
                match (has, in_file) {
                    (false, _) => Tri::No,
                    (true, true) => Tri::Maybe,
                    (true, false) => Tri::Yes,
                }
            }
            Compiled::Tag { keys, .. } => {
                if keys.iter().any(|k| self.tag_keys().contains(k)) {
                    Tri::Maybe
                } else {
                    Tri::No
                }
            }
            Compiled::File(of) => match self.check(of, true) {
                Tri::No => Tri::No,
                _ => Tri::Maybe,
            },
            Compiled::Leaf(node) => Tri::of(self.leaf(node)),
            _ => Tri::Maybe,
        }
    }

    /// The cheap check of a numeric range.
    fn check_range(&self, field: NumField, lo: Option<u64>, hi: Option<u64>, in_file: bool) -> Tri {
        let inside = |v: u64| lo.is_none_or(|l| v >= l) && hi.is_none_or(|h| v <= h);
        if in_file && field == NumField::Size {
            // The sizes `file:(…)` reads lie between these, so a
            // range that misses them misses every file.
            let (min, max) = self.content().file_sizes;
            let reachable = lo.is_none_or(|l| l <= max) && hi.is_none_or(|h| h >= min);
            let any = reachable
                && self
                    .item
                    .files
                    .iter()
                    .take(crate::MAX_FILES)
                    .any(|f| !f.padding && inside(f.size));
            if any { Tri::Maybe } else { Tri::No }
        } else {
            Tri::of(self.any_number(field, None, inside))
        }
    }

    /// Whether `f` holds for any text of `field`: inside `file:(…)`, that
    /// file's name or path.
    fn any_text(&self, field: TextField, file: Option<usize>, f: &dyn Fn(&Text) -> bool) -> bool {
        match file {
            Some(i) => match field {
                TextField::Name => f(&self.file_names()[i]),
                _ => f(&self.files()[i]),
            },
            None => match field {
                TextField::Name => f(self.name()),
                TextField::Path => self.files().iter().any(f),
                TextField::Title => self.titles().iter().any(f),
                TextField::Any => f(self.name()) || self.files().iter().any(f),
            },
        }
    }

    fn eval(&self, q: &Compiled, file: Option<usize>, neutral: &[String]) -> bool {
        // Inside `file:(…)`, text and size clauses read that one file.
        let texts = |field: TextField, f: &dyn Fn(&Text) -> bool| self.any_text(field, file, f);
        let neutral_word = |fold: &str| !neutral.is_empty() && neutral.iter().any(|n| **n == *fold);
        match q {
            Compiled::All => true,
            Compiled::And(of) => of.iter().all(|n| self.eval(n, file, neutral)),
            Compiled::Or(of) => of.iter().any(|n| self.eval(n, file, neutral)),
            Compiled::Not(of) => !self.eval(of, file, neutral),
            Compiled::Word {
                field,
                whole,
                parts,
                fold,
            } => {
                !neutral_word(fold)
                    && match (field, file) {
                        (TextField::Any, None) => self.word_anywhere(whole, parts),
                        _ => None,
                    }
                    .unwrap_or_else(|| texts(*field, &|t| has_word(t, whole, parts)))
            }
            Compiled::Phrase { field, parts, fold } => {
                !neutral_word(fold) && texts(*field, &|t| sequence(t, parts))
            }
            Compiled::Near { field, words, slop } => texts(*field, &|t| has_near(t, words, *slop)),
            // Prefixes and fuzzy words read each distinct token once.
            Compiled::Prefix {
                field,
                prefix,
                start,
            } => {
                let starts = |t: &str| t.starts_with(&**prefix);
                !prefix.is_empty()
                    && match (field, file) {
                        (TextField::Any, None) => {
                            self.keys().vocabulary_starting(prefix, *start).any(starts)
                        }
                        _ => texts(*field, &|t| t.toks.iter().any(|x| starts(t.text(x)))),
                    }
            }
            Compiled::Fuzzy {
                field,
                word,
                distance,
            } => {
                let near = |t: &str| word.near(t, *distance);
                match (field, file) {
                    (TextField::Any, None) => self
                        .keys()
                        .vocabulary_near(word.chars.len(), word.has, usize::from(*distance))
                        .any(|t| edit_distance(t, &word.chars) <= usize::from(*distance)),
                    _ => texts(*field, &|t| t.toks.iter().any(|x| near(t.text(x)))),
                }
            }
            Compiled::Range { field, lo, hi } => {
                let inside = |v: u64| lo.is_none_or(|l| v >= l) && hi.is_none_or(|h| v <= h);
                self.any_number(*field, file, inside)
            }
            Compiled::Ext { any } => {
                let has = |f: &FileInfo| {
                    lodestar_classify::ext::extension(f.name())
                        .is_some_and(|e| any.iter().any(|a| a == e.as_str()))
                };
                match file {
                    Some(i) => has(&self.item.files[i]),
                    None => self.item.files.iter().any(has),
                }
            }
            Compiled::Tag { tags, .. } => self
                .enrichment()
                .is_some_and(|e| tags.iter().any(|t| e.tags.contains(t))),
            Compiled::File(of) => {
                file.is_none()
                    && (0..self.item.files.len().min(crate::MAX_FILES))
                        .any(|i| !self.item.files[i].padding && self.eval(of, Some(i), neutral))
            }
            Compiled::Leaf(node) => self.leaf(node),
        }
    }

    /// A word anywhere in the name or paths, from the torrent's distinct
    /// token texts where they decide it: one of its whole forms is there,
    /// or its one part, or one of its parts isn't. Several parts all
    /// there need their positions, so `None` leaves it to the texts.
    fn word_anywhere(&self, whole: &[Term], parts: &[Term]) -> Option<bool> {
        let keys = self.keys();
        let has = |t: &Term| keys.has_exact(t.hash, &t.text);
        if whole.iter().any(has) {
            return Some(true);
        }
        match parts {
            [] => Some(false),
            [one] => Some(has(one)),
            _ if !parts.iter().all(has) => Some(false),
            _ => None,
        }
    }

    /// The clauses that read neither text nor files.
    fn leaf(&self, node: &Node) -> bool {
        match node {
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
            Node::Flag { name, value } => self.flag(name) == *value,
            Node::Hash { hash } => self.item.hashes.iter().any(|h| hex(h) == *hash),
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
            _ => unreachable!("compile keeps only these as leaves"),
        }
    }

    /// Whether any value of a numeric field satisfies `f`. Enrichment is
    /// only built for the fields that need it.
    fn any_number(&self, field: NumField, file: Option<usize>, f: impl Fn(u64) -> bool) -> bool {
        if let (Some(i), NumField::Size) = (file, field) {
            return f(self.item.files[i].size);
        }
        let content = self.content();
        let list = |pick: fn(&DocEnrichment) -> &[u64]| {
            self.enrichment()
                .is_some_and(|e| pick(e).iter().any(|v| f(*v)))
        };
        match field {
            NumField::Size => f(self.total_size()),
            NumField::Files => f(content.count),
            NumField::Largest => content.largest.is_some_and(&f),
            NumField::Piece => self.facts.is_some_and(|x| f(x.piece_len)),
            NumField::Year => list(|e| &e.years),
            NumField::Season => list(|e| &e.seasons),
            NumField::Episode => list(|e| &e.episodes),
            NumField::Junk => self.enrichment().is_some_and(|e| f(e.junk)),
        }
    }

    /// The torrent's size, as `size:` reads it.
    fn total_size(&self) -> u64 {
        self.facts.map_or_else(|| self.content().bytes, |x| x.size)
    }

    fn content(&self) -> &Content {
        self.content.get_or_init(|| {
            let mut c = Content {
                file_sizes: (u64::MAX, 0),
                ..Content::default()
            };
            for file in self.item.files.iter().filter(|f| !f.padding) {
                c.count += 1;
                c.bytes += file.size;
                c.largest = Some(c.largest.map_or(file.size, |l: u64| l.max(file.size)));
            }
            for f in self
                .item
                .files
                .iter()
                .take(crate::MAX_FILES)
                .filter(|f| !f.padding)
            {
                c.file_sizes = (c.file_sizes.0.min(f.size), c.file_sizes.1.max(f.size));
            }
            c
        })
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

/// A word: its whole or compound form as one token, or its parts in a row.
fn has_word(field: &Text, whole: &[Term], parts: &[Term]) -> bool {
    whole
        .iter()
        .any(|w| field.toks.iter().any(|t| field.is(t, w.hash, &w.text)))
        || sequence(field, parts)
}

/// The terms at consecutive positions. Each place of the first term
/// looks the rest up by position, so the cost grows with the text, not
/// its square.
fn sequence(field: &Text, words: &[Term]) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    crate::work::tick(field.toks.len());
    field
        .toks
        .iter()
        .filter(|t| field.is(t, first.hash, &first.text))
        .any(|start| {
            rest.iter().zip(1..).all(|(w, i)| {
                field
                    .at(start.position + i)
                    .any(|t| field.is(t, w.hash, &w.text))
            })
        })
}

/// Every word within a window of `slop` extra positions, in any order:
/// for some place of the first word, every word has a place within the
/// window. A word of several parts takes up their span, and distances
/// run between the nearest ends of two spans (§16), so `1080p NEAR/1 x`
/// allows one token between `p` and `x`, as a single word would.
///
/// Names are untrusted and can repeat a word thousands of times. Each
/// word's places are listed once, in order, and each place of the first
/// word finds the nearest of every other word's by binary search, so the
/// cost grows with the text times its logarithm, whatever the slop.
fn has_near(field: &Text, words: &[Operand], slop: u32) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    let span = words.len() as u32 + slop;
    // Words of one token each, the usual case: each place of the first
    // word against every token, as cheap as it gets for an ordinary name.
    // Past a few places, the listed search below takes over.
    if words
        .iter()
        .all(|w| w.whole.is_empty() && w.parts.len() == 1)
    {
        fn places<'a>(field: &'a Text, w: &'a Operand) -> impl Iterator<Item = u32> + 'a {
            let w = &w.parts[0];
            crate::work::tick(field.toks.len());
            field
                .toks
                .iter()
                .filter(move |t| field.is(t, w.hash, &w.text))
                .map(|t| t.position)
        }
        let mut found = 0;
        for p0 in places(field, first) {
            found += 1;
            if found > SCANS {
                break;
            }
            if rest
                .iter()
                .all(|w| places(field, w).any(|p| p.abs_diff(p0) < span))
            {
                return true;
            }
        }
        if found <= SCANS {
            return false;
        }
    }
    let first = Places::of(field, first);
    let rest: Vec<Places> = rest.iter().map(|w| Places::of(field, w)).collect();
    first.all().any(|a| rest.iter().all(|w| w.within(a, span)))
}

/// Plain scans of a text's tokens for the first word's places before
/// NEAR lists every word's: as cheap for an ordinary name, and bounded
/// by the text's length times this.
const SCANS: u32 = 8;

/// A NEAR word's places in one text, each list by first position: its
/// parts in a row (`width` positions after the first), and its whole
/// forms (one position each).
struct Places {
    parts: Vec<u32>,
    width: u32,
    whole: Vec<u32>,
}

impl Places {
    fn of(field: &Text, w: &Operand) -> Places {
        crate::work::tick(field.toks.len());
        let mut p = Places {
            parts: Vec::new(),
            width: w.parts.len().saturating_sub(1) as u32,
            whole: Vec::new(),
        };
        for t in &field.toks {
            if let Some((head, tail)) = w.parts.split_first() {
                if field.is(t, head.hash, &head.text)
                    && tail.iter().zip(1..).all(|(x, i)| {
                        field
                            .at(t.position + i)
                            .any(|t| field.is(t, x.hash, &x.text))
                    })
                {
                    p.parts.push(t.position);
                    continue;
                }
            }
            if w.whole.iter().any(|x| field.is(t, x.hash, &x.text)) {
                p.whole.push(t.position);
            }
        }
        p.parts.sort_unstable();
        p.whole.sort_unstable();
        p
    }

    /// Every place, as first and last positions.
    fn all(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let parts = self.parts.iter().map(|&p| (p, p + self.width));
        parts.chain(self.whole.iter().map(|&p| (p, p)))
    }

    /// Whether a place is fewer than `span` positions from `(a0, a1)`,
    /// between nearest ends. For a place `(b, b + w)` that is
    /// `a0 - (span - 1) - w <= b <= a1 + span - 1`; the first place at or
    /// after the low end, found by binary search, answers it.
    fn within(&self, (a0, a1): (u32, u32), span: u32) -> bool {
        let near = |list: &[u32], w: u32| {
            crate::work::tick(list.len().max(1).ilog2() as usize + 1);
            let lo = i64::from(a0) - i64::from(span) + 1 - i64::from(w);
            let hi = i64::from(a1) + i64::from(span) - 1;
            let i = list.partition_point(|&b| i64::from(b) < lo);
            list.get(i).is_some_and(|&b| i64::from(b) <= hi)
        };
        near(&self.parts, self.width) || near(&self.whole, 0)
    }
}

fn edit_distance(a: &str, b: &[char]) -> usize {
    // Token texts are at most 64 bytes, so a row fits on the stack.
    let mut stack = [0usize; 66];
    let mut heap = Vec::new();
    let row: &mut [usize] = if b.len() < stack.len() {
        &mut stack[..=b.len()]
    } else {
        heap.resize(b.len() + 1, 0);
        &mut heap
    };
    for (j, r) in row.iter_mut().enumerate() {
        *r = j;
    }
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
#[path = "matcher_reference.rs"]
mod reference;

#[cfg(test)]
#[path = "matcher_old.rs"]
mod old;

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::{ContentType, classify};
    use lodestar_search::parse;
    use proptest::prelude::*;

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
        let doc = Doc::new(&item, Some(&facts));
        let q = parse(query).unwrap();
        let ours = doc.matches(&q, &[]);
        assert_eq!(
            ours,
            reference::matches(&item, Some(&facts), &q, &[]),
            "{query}"
        );
        ours
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
        // A phrase across a folder and its file.
        assert!(check(MOVIE, FILES, "\"sample sample\""));
        assert!(!check(MOVIE, FILES, "\"mkv sample\""));
    }

    #[test]
    fn near_words_of_several_tokens() {
        // `1080p`, `BluRay` and `x264` split into parts: the at 0, matrix
        // 1, 1999 2, 1080 p 3–4, blu ray 5–6, x 264 7–8, grp 9. Each
        // matches as its parts in a row, and distances run from the
        // nearest end.
        assert!(check(MOVIE, FILES, "1080p NEAR/1 bluray"));
        assert!(check(MOVIE, FILES, "bluray NEAR/1 1080p"));
        assert!(check(MOVIE, FILES, "1080p NEAR/2 x264"));
        assert!(!check(MOVIE, FILES, "1080p NEAR/1 x264"));
        assert!(check(MOVIE, FILES, "x264 NEAR/1 grp"));
        assert!(check(MOVIE, FILES, "1999 NEAR/2 1080p NEAR/2 bluray"));
        assert!(!check(MOVIE, FILES, "1999 NEAR/2 1080p NEAR/2 x264"));
        assert!(!check(MOVIE, FILES, "1080p NEAR/3 grp"));
        assert!(check(MOVIE, FILES, "1080p NEAR/4 grp"));
        assert!(check(MOVIE, FILES, "matrix NEAR/1 1080p"));
        assert!(!check(MOVIE, FILES, "matrix NEAR/2 x264"));
        assert!(check(MOVIE, FILES, "-(1080p NEAR/1 grp)"));
        assert!(!check(MOVIE, FILES, "-(1080p NEAR/2 x264)"));
        // The same word twice may be the same place, as before.
        assert!(check(MOVIE, FILES, "1080p NEAR/1 1080p"));
        // A part of a compound token is a word of its own.
        assert!(check(MOVIE, FILES, "264 NEAR/1 grp"));
        // Both sides of a compound: `web-dl` matches `WEBDL`, and the parts.
        let tv = [("Show.S01E02.WEBDL.x265.mkv", 1 << 30)];
        assert!(check("Show.S01E02.WEBDL.x265", &tv, "web-dl NEAR/1 x265"));
        assert!(check("Show.S01E02.WEB-DL.x265", &tv, "webdl NEAR/1 s01e02"));
        assert!(check(
            "Show.S01E02.WEB-DL.x265",
            &tv,
            "web-dl NEAR/1 s01e02"
        ));
        assert!(check("Show.S01E02.DD+5.1.x265", &tv, "dd+ NEAR/1 5.1"));
        // A form of only some of the parts doesn't stand for the word.
        assert!(!check(
            "Show WEBDL",
            &tv,
            "name:web-dl.x265 NEAR/1 name:show"
        ));
        // Parts out of order aren't the word.
        assert!(!check("p 1080 Show", &tv, "name:1080p NEAR/5 name:show"));
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
        assert!(!check(MOVIE, FILES, "file:(readme ext:mkv)"));
        assert!(check(MOVIE, FILES, "file:(-sample ext:mkv)"));
        assert!(check(MOVIE, FILES, "file:(name:sample -name:matrix)"));
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
        // Neutralizing makes a word false, so its negation true.
        assert!(doc.matches(&parse("-alpha").unwrap(), &["alpha".to_owned()]));
    }

    #[test]
    fn a_remembered_query_follows_its_address() {
        let f = files(&[("a.mkv", 1)]);
        let item = Item {
            hashes: &[[0; 20]],
            name: "Alpha Beta",
            files: &f,
            content_type: ContentType::Video,
        };
        let doc = Doc::new(&item, None);
        // The same place in memory, holding another query.
        let mut q = parse("alpha").unwrap();
        assert!(doc.matches(&q, &[]));
        q = parse("gamma").unwrap();
        assert!(!doc.matches(&q, &[]));
        q = parse("beta").unwrap();
        assert!(doc.matches(&q, &[]));
    }

    fn near_of(q: &str) -> (Vec<Operand>, u32) {
        match compile(&parse(q).unwrap()) {
            Compiled::Near { words, slop, .. } => (words, slop),
            c => panic!("{q:?} compiled to {c:?}"),
        }
    }

    fn phrase_of(q: &str) -> Vec<Term> {
        match compile(&parse(q).unwrap()) {
            Compiled::Phrase { parts, .. } => parts,
            c => panic!("{q:?} compiled to {c:?}"),
        }
    }

    /// Hostile names (§6) repeat a word thousands of times. Phrases and
    /// NEAR cost in proportion to the text (times its logarithm), held
    /// to a count of steps rather than a time: four times the text takes
    /// well under the sixteen times a square would.
    #[test]
    fn hostile_names_cost_in_proportion() {
        let far = format!("{}b", "x ".repeat(60));
        let cases: &[(&str, &str, &str)] = &[
            // Every place of the first word starts a phrase that fails late.
            ("a ", "b", "\"a a a a b c\""),
            ("1080p ", "x264", "\"1080p 1080p 1080p x265\""),
            // More places than the plain scans take, single tokens.
            ("a ", "b", "a NEAR/2 c"),
            ("a b ", "c", "a NEAR/50 d"),
            // The other word there, near only the last place, or near none.
            ("a ", "b", "a NEAR/1 b"),
            ("a ", &far, "a NEAR/50 b"),
            // Words of several tokens, near and far, and a wide slop.
            ("1080p.x264 ", "WEB-DL", "1080p NEAR/3 web-dl"),
            ("1080p.x264 ", "WEB-DL", "1080p NEAR/50 web-dl NEAR/50 dd+"),
            ("web dl x ", "z", "web-dl NEAR/5 web-dl NEAR/5 z"),
        ];
        for &(unit, tail, q) in cases {
            let text = |n: usize| fasttok::tokens(&format!("{}{tail}", unit.repeat(n)));
            let run = |t: &Text| -> (bool, u64) {
                crate::work::counted(|| {
                    if q.contains("NEAR") {
                        let (words, slop) = near_of(q);
                        has_near(t, &words, slop)
                    } else {
                        sequence(t, &phrase_of(q))
                    }
                })
            };
            let (small, big) = (text(1_000), text(4_000));
            let ((m1, w1), (m2, w2)) = (run(&small), run(&big));
            assert_eq!(m1, m2, "{q:?}");
            let n = big.toks.len() as u64;
            assert!(
                w2 <= 6 * w1,
                "{q:?}: {w1} steps, then {w2} on four times the text"
            );
            assert!(
                w2 <= 40 * n * u64::from(n.ilog2()),
                "{q:?}: {w2} steps on {n} tokens"
            );
            // As the search before it answered.
            let old = if q.contains("NEAR") {
                let (words, slop) = near_of(q);
                old::has_near(&small, &words, slop)
            } else {
                old::sequence(&small, &phrase_of(q))
            };
            assert_eq!(m1, old, "{q:?}");
        }
    }

    /// Names and paths of the words NEAR tests repeat, files sharing
    /// folders.
    fn near_text() -> impl Strategy<Value = String> {
        (
            prop::collection::vec(near_word(), 0..14),
            prop::collection::vec(prop::sample::select(vec![" ", ".", "-", "_", "+ "]), 14),
        )
            .prop_map(|(w, s)| {
                w.iter()
                    .zip(s)
                    .map(|(w, s)| format!("{w}{s}"))
                    .collect::<String>()
            })
    }

    fn near_torrent() -> impl Strategy<Value = (String, Vec<FileInfo>)> {
        let segs = prop::collection::vec(near_text(), 1..5);
        (
            near_text(),
            segs,
            prop::collection::vec((0usize..5, 0usize..5, 0usize..3), 0..6),
        )
            .prop_map(|(name, segs, layout)| {
                let files = layout
                    .iter()
                    .map(|&(a, b, depth)| {
                        let mut path: Vec<String> = (0..depth)
                            .map(|d| segs[(a + d) % segs.len()].clone())
                            .collect();
                        path.push(segs[b % segs.len()].clone());
                        FileInfo::new(path, 1)
                    })
                    .collect();
                (name, files)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3000))]
        /// The nearness the scan records turns a NEAR away only when the
        /// full match says no, inside `file:(…)` too, whatever the words,
        /// slop and folders.
        #[test]
        fn nearness_never_turns_a_match_away(
            (name, files) in near_torrent(),
            q in prop::collection::vec(near_word(), 2..4),
            slop in 1u32..9,
            other in prop::collection::vec(near_word(), 2..3),
            in_file in any::<bool>(),
        ) {
            let near = q.join(&format!(" NEAR/{slop} "));
            let query = if in_file { format!("file:({near})") } else { near.clone() };
            let compiled = compile(&parse(&query).unwrap());
            // The rule set's terms: this NEAR's, and another's.
            let another = compile(&parse(&other.join(" NEAR/2 ")).unwrap());
            let terms = prefilter::NearTerms::of([&compiled, &another].into_iter()).map(std::sync::Arc::new);
            let item = Item { hashes: &[[1; 20]], name: &name, files: &files, content_type: ContentType::Video };
            let full = Doc::new(&item, None).eval(&compiled, None, &[]);
            let doc = Doc::new(&item, None).with_near(terms.as_ref());
            if doc.check(&compiled, false) == Tri::No {
                prop_assert!(!full, "{} on {:?} {:?}", query, name, files);
            }
            prop_assert_eq!(doc.eval(&compiled, None, &[]), full);
        }
    }

    /// A negated word gates a rule away only where the torrent has it and
    /// it counts: a neutralized word never does (CSX-5 *Neutralize term*).
    #[test]
    fn lacks_gates_follow_neutralized_words() {
        let item = Item {
            hashes: &[[1; 20]],
            name: "Show.S01.WEB.x264-GRP",
            files: &[],
            content_type: ContentType::Video,
        };
        let doc = Doc::new(&item, None);
        for (q, neutral, matches) in [
            ("x264 -web", &[][..], false),
            ("x264 -web", &["web".to_owned()][..], true),
            ("x264 -webrip", &[][..], true),
            ("x264 -web-dl", &[][..], true),
            ("x264 -grp", &[][..], false),
        ] {
            let compiled = compile(&parse(q).unwrap());
            let mut clauses = prefilter::Clauses::default();
            let g = gate(&compiled, &mut clauses);
            assert!(matches!(g, Gate::Lacks(_)), "{q}: {g:?}");
            assert_eq!(
                doc.eval(&compiled, None, neutral),
                matches,
                "{q} {neutral:?}"
            );
            assert_eq!(
                doc.passes(g, &clauses.list, neutral),
                matches,
                "{q} {neutral:?}"
            );
        }
    }

    /// A join's compound is found after the word it sits at (`webdl`, at
    /// `web`, comes with `dl`): the nearness kept for it reaches back far
    /// enough, so the NEAR isn't turned away.
    #[test]
    fn nearness_reaches_back_for_compounds() {
        for (name, q) in [
            ("x a WEB-DL", "x NEAR/1 webdl"),
            ("x a b c d e f g WEB-DL", "x NEAR/7 webdl"),
            ("x a h.264", "x NEAR/1 h264"),
            ("x DD+", "x NEAR/1 \"dd+\""),
        ] {
            let compiled = compile(&parse(q).unwrap());
            let terms =
                prefilter::NearTerms::of(std::iter::once(&compiled)).map(std::sync::Arc::new);
            let item = Item {
                hashes: &[[1; 20]],
                name,
                files: &[],
                content_type: ContentType::Video,
            };
            let doc = Doc::new(&item, None).with_near(terms.as_ref());
            assert!(doc.eval(&compiled, None, &[]), "{q} on {name}");
            assert_ne!(doc.check(&compiled, false), Tri::No, "{q} on {name}");
        }
    }

    /// A NEAR outside the rule set (an *Always allow* query, say) whose
    /// words are among the rule set's NEAR terms, in a pair no rule asks
    /// about, isn't turned away by the nearness.
    #[test]
    fn nearness_tells_only_of_pairs_asked() {
        let rules = compile(&parse("a NEAR/1 b").unwrap());
        let other = compile(&parse("b NEAR/1 c").unwrap());
        let terms =
            prefilter::NearTerms::of([&rules, &compile(&parse("c NEAR/1 d").unwrap())].into_iter())
                .map(std::sync::Arc::new);
        let item = Item {
            hashes: &[[1; 20]],
            name: "a x x x b c",
            files: &[],
            content_type: ContentType::Video,
        };
        let doc = Doc::new(&item, None).with_near(terms.as_ref());
        assert_eq!(doc.check(&rules, false), Tri::No);
        assert!(doc.eval(&other, None, &[]));
        assert_ne!(doc.check(&other, false), Tri::No);
    }

    /// Recording nearness stays in proportion to the name (§6): a name
    /// repeating NEAR terms four times as long takes about four times the
    /// steps.
    #[test]
    fn hostile_names_record_nearness_in_proportion() {
        let compiled = compile(&parse("1080p NEAR/8 x264 NEAR/8 web-dl").unwrap());
        let terms = prefilter::NearTerms::of(std::iter::once(&compiled)).map(std::sync::Arc::new);
        let steps = |n: usize| {
            let name = "1080p.x264.WEB-DL.1080p ".repeat(n);
            crate::work::counted(|| DocKeys::with_near(&name, &[], terms.as_ref())).1
        };
        let (small, big) = (steps(500), steps(2_000));
        assert!(
            big <= 5 * small,
            "{small} steps, then {big} on four times the name"
        );
    }

    /// Words a phrase or NEAR test text repeats: single tokens, words of
    /// several, and their compound forms.
    fn near_word() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec![
            "a", "b", "x", "1080p", "1080", "p", "x264", "264", "web-dl", "webdl", "web", "dl",
            "dd+", "dd", "h.264", "5.1",
        ])
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        /// The indexed phrase and NEAR search answer as the scanning one
        /// before it did, on texts that repeat their words.
        #[test]
        fn indexed_search_agrees_with_the_scans(
            words in prop::collection::vec(near_word(), 0..60),
            seps in prop::collection::vec(prop::sample::select(vec![" ", ".", "-", "_", "  "]), 60),
            q in prop::collection::vec(near_word(), 2..4),
            slop in 1u32..7,
        ) {
            let text: String = words.iter().zip(&seps).map(|(w, s)| format!("{w}{s}")).collect();
            let t = fasttok::tokens(&text);
            let near = q.join(&format!(" NEAR/{slop} "));
            let (ws, sl) = near_of(&near);
            prop_assert_eq!(has_near(&t, &ws, sl), old::has_near(&t, &ws, sl), "{} on {:?}", near, text);
            let phrase = format!("\"{}\"", q.join(" "));
            let parts = phrase_of(&phrase);
            prop_assert_eq!(sequence(&t, &parts), old::sequence(&t, &parts), "{} on {:?}", phrase, text);
        }
    }

    #[test]
    fn distance() {
        let d = |a: &str, b: &str| edit_distance(a, &b.chars().collect::<Vec<_>>());
        assert_eq!(d("matrix", "matirx"), 2);
        assert_eq!(d("", "ab"), 2);
        assert_eq!(d("abc", "abc"), 0);
        assert_eq!(
            d(&"a".repeat(70), &"a".repeat(68)),
            2,
            "longer than the stack row"
        );
    }

    proptest! {
        /// The length and character bounds never rule out a word within
        /// the distance.
        #[test]
        fn fuzzy_bounds_are_sound(
            a in "[a-c0-1]{0,8}|\\PC{0,6}",
            b in "[a-c0-1]{0,8}|\\PC{0,6}",
            distance in 0u8..4,
        ) {
            let exact = edit_distance(&a, &b.chars().collect::<Vec<_>>()) <= usize::from(distance);
            prop_assert_eq!(Spelled::new(&b).near(&a, distance), exact);
        }
    }

    /// Words a query is built from: the names' own words and others.
    fn word() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::sample::select(vec![
                "show",
                "name",
                "s01",
                "s01e02",
                "e02",
                "1080p",
                "web",
                "dl",
                "web-dl",
                "webdl",
                "x265",
                "x",
                "265",
                "h.264",
                "h264",
                "264",
                "dd+",
                "dd",
                "5.1",
                "51",
                "group",
                "sample",
                "mkv",
                "srt",
                "en",
                "matrix",
                "the",
                "ddp5",
                "title",
                "mat",
                "showname",
                "ünïcode",
                "日本語",
            ])
            .prop_map(str::to_owned),
            "[a-z0-9]{1,6}",
        ]
    }

    fn field() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["", "", "", "name:", "path:", "title:"])
    }

    /// Random queries over the text, size, extension and file clauses.
    fn query() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            (field(), word()).prop_map(|(f, w)| format!("{f}{w}")),
            (field(), word(), word()).prop_map(|(f, a, b)| format!("{f}\"{a} {b}\"")),
            (field(), word(), word(), word())
                .prop_map(|(f, a, b, c)| format!("{f}\"{a} {b} {c}\"")),
            (word(), word(), 1u32..4).prop_map(|(a, b, n)| format!("{a} NEAR/{n} {b}")),
            (field(), word(), word(), word(), 1u32..4)
                .prop_map(|(f, a, b, c, n)| format!("{f}{a} NEAR/{n} {f}{b} NEAR/{n} {f}{c}")),
            (field(), "[a-z0-9]{2,4}").prop_map(|(f, p)| format!("{f}{p}*")),
            "[a-z0-9]{3,7}".prop_map(|w| format!("~{w}")),
            (0u64..12).prop_map(|n| format!("size:>{n}GB")),
            (1u64..40).prop_map(|n| format!("files:<{n}")),
            prop::sample::select(vec!["ext:mkv", "ext:srt", "ext:exe", "ext:nfo"])
                .prop_map(str::to_owned),
            prop::sample::select(vec!["type:tv", "type:movie", "res:1080p", "group:group"])
                .prop_map(str::to_owned),
        ];
        leaf.prop_recursive(3, 12, 3, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 2..4).prop_map(|v| v.join(" ")),
                prop::collection::vec(inner.clone(), 2..4)
                    .prop_map(|v| format!("({})", v.join(" | "))),
                inner.clone().prop_map(|q| format!("-({q})")),
                prop::collection::vec(inner, 1..3).prop_map(|v| format!("file:({})", v.join(" "))),
            ]
        })
    }

    fn torrent() -> impl Strategy<Value = (String, Vec<(String, u64)>)> {
        let words = prop::collection::vec(
            prop_oneof![
                prop::sample::select(vec![
                    "Show",
                    "Name",
                    "S01",
                    "S01E02",
                    "1080p",
                    "WEB-DL",
                    "DD+5.1",
                    "x265",
                    "H.264",
                    "GROUP",
                    "Sample",
                    "The",
                    "Matrix",
                    "ShowName",
                    "Ünïcode",
                    "日本語",
                ])
                .prop_map(str::to_owned),
                "[A-Za-z0-9]{1,5}",
            ],
            1..8,
        );
        let sep = prop::sample::select(vec![".", " ", "-", "_", "."]);
        let text = (words, prop::collection::vec(sep, 8)).prop_map(|(w, s)| {
            w.iter()
                .zip(s)
                .map(|(w, s)| format!("{w}{s}"))
                .collect::<String>()
        });
        let ext = prop::sample::select(vec!["mkv", "srt", "nfo", "exe", "mkv"]);
        let file = (
            prop::collection::vec(text.clone(), 1..3),
            ext,
            0u64..(16u64 << 30),
        )
            .prop_map(|(segs, ext, size)| (format!("{}.{ext}", segs.join("/")), size));
        (text, prop::collection::vec(file, 0..6))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4000))]
        /// The compiled match, with its cheap check, agrees with the
        /// previous matcher on any query and torrent, the cheap check
        /// alone never contradicts the full match, and the query's gate
        /// turns it away only where the cheap check says no.
        #[test]
        fn agrees_with_the_reference(q in query(), (name, paths) in torrent(), neutral in any::<bool>()) {
            let Ok(node) = parse(&q) else { return Ok(()) };
            let f: Vec<FileInfo> = paths
                .iter()
                .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
                .collect();
            let class = classify(&name, &f);
            let facts = Facts {
                class: &class,
                size: f.iter().map(|f| f.size).sum(),
                piece_len: 1 << 20,
                flags: 0,
                discovered_at: 0,
                now: 0,
            };
            let item = Item { hashes: &[[1; 20]], name: &name, files: &f, content_type: class.content_type };
            let neutral: Vec<String> = if neutral { vec!["web".into(), "1080p".into()] } else { vec![] };
            let doc = Doc::new(&item, Some(&facts));
            let compiled = compile(&node);
            let full = doc.eval(&compiled, None, &neutral);
            prop_assert_eq!(full, reference::matches(&item, Some(&facts), &node, &neutral), "{} on {:?} {:?}", q, name, paths);
            match doc.check(&compiled, false) {
                Tri::No => prop_assert!(!full, "check said no: {} on {:?} {:?}", q, name, paths),
                Tri::Yes => prop_assert!(full, "check said yes: {} on {:?} {:?}", q, name, paths),
                Tri::Maybe => {}
            }
            prop_assert_eq!(doc.matches_compiled(&compiled, &neutral), full);
            // A gate turns a rule away only where the full match must.
            let mut clauses = prefilter::Clauses::default();
            let g = gate(&compiled, &mut clauses);
            if !doc.passes(g, &clauses.list, &neutral) {
                prop_assert!(!full, "gate {:?}: {} on {:?} {:?}", g, q, name, paths);
            }
        }
    }
}
