//! Narrowing which rules can match a torrent before any of them runs
//! (§20.8: under 50 µs per torrent at 10,000 rules).
//!
//! A query names text it can't match without: a word needs one of its
//! token texts in the torrent, a phrase its first word, `group:x` that
//! tag. That is its [`Need`]. A torrent's [`DocKeys`] are the hashes of
//! every token text the search tokenizer would produce for its name and
//! files. A rule whose need isn't among them can't match, so it isn't
//! evaluated, and the torrent isn't tokenized for the full match unless
//! some rule could match.
//!
//! The narrowing must never skip a rule that would match. For ASCII text
//! (most names) the keys come from a byte scan that produces exactly the
//! token texts [`tokenize`] does (property-tested against it); other text
//! goes through the tokenizer itself. Queries that no text narrows
//! (sizes, extensions, prefixes, negations) are always evaluated.

use std::cell::OnceCell;

use lodestar_classify::FileInfo;
use lodestar_search::query::TextField;
use lodestar_search::tokenizer::{MAX_TOKEN_BYTES, TokKind, tokenize};
use lodestar_search::{DocEnrichment, Node};

/// Builds a torrent's tag keys, on first use.
pub(crate) type Tags<'a> = &'a dyn Fn() -> IdSet;

use crate::hash::{IdMap, IdSet, fmix64, fnv_byte, fnv_start, term_hash};
use crate::rules::UserRule;

/// What a query can't match without.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Need {
    /// Nothing narrows it: always evaluated.
    Always,
    /// It can't match anything.
    Never,
    /// One of these keys must be in the torrent.
    Any(Vec<u64>),
}

/// Words longer than this (in chars) aren't split (the tokenizer's
/// `MAX_SPLIT_CHARS`).
const MAX_SPLIT_CHARS: usize = 24;

/// A tag's key, apart from text keys.
fn tag_key(tag: &str) -> u64 {
    let mut h = fnv_start();
    for b in b"\x01tag\x01".iter().chain(tag.as_bytes()) {
        h = fnv_byte(h, *b);
    }
    fmix64(h)
}

/// Every token text of a query word, joined (the matcher's `folded`).
fn folded(word: &str) -> String {
    tokenize(word)
        .into_iter()
        .map(|t| t.text)
        .collect::<Vec<_>>()
        .join("")
}

/// The need of a query. Inside `file:(…)`, every text field reads that
/// file's name or path, so titles narrow there too.
pub(crate) fn need(node: &Node) -> Need {
    need_in(node, false)
}

fn need_in(node: &Node, in_file: bool) -> Need {
    let text = |field: &TextField| in_file || *field != TextField::Title;
    match node {
        Node::And { of } => {
            let mut best: Option<Vec<u64>> = None;
            for n in of {
                match need_in(n, in_file) {
                    Need::Never => return Need::Never,
                    Need::Always => {}
                    Need::Any(k) => {
                        if best.as_ref().is_none_or(|b| k.len() < b.len()) {
                            best = Some(k);
                        }
                    }
                }
            }
            best.map_or(Need::Always, Need::Any)
        }
        Node::Or { of } => {
            let mut keys = Vec::new();
            for n in of {
                match need_in(n, in_file) {
                    Need::Never => {}
                    Need::Always => return Need::Always,
                    Need::Any(k) => keys.extend(k),
                }
            }
            if keys.is_empty() {
                Need::Never
            } else {
                Need::Any(keys)
            }
        }
        // A whole-token match, or the parts in a row (starting with the
        // first part).
        Node::Word { field, word } if text(field) => {
            let q = tokenize(word);
            let mut keys: Vec<u64> = q
                .iter()
                .filter(|t| t.kind != TokKind::Part)
                .map(|t| term_hash(&t.text))
                .collect();
            if let Some(p) = q.iter().find(|t| t.kind == TokKind::Part) {
                keys.push(term_hash(&p.text));
            }
            if keys.is_empty() {
                Need::Never
            } else {
                Need::Any(keys)
            }
        }
        Node::Phrase { field, text: t } if text(field) => {
            match tokenize(t).into_iter().find(|t| t.kind == TokKind::Part) {
                Some(p) => Need::Any(vec![term_hash(&p.text)]),
                None => Need::Never,
            }
        }
        Node::Near { field, words, .. } if text(field) => match words.first().map(|w| folded(w)) {
            Some(f) if f.is_empty() => Need::Never,
            Some(f) => Need::Any(vec![term_hash(&f)]),
            None => Need::Always,
        },
        Node::File { of } => need_in(of, true),
        Node::Tag { key, any } => {
            if any.is_empty() {
                Need::Never
            } else {
                Need::Any(
                    any.iter()
                        .map(|v| tag_key(&format!("{}:{v}", key.as_str())))
                        .collect(),
                )
            }
        }
        _ => Need::Always,
    }
}

/// The keys of one torrent: its token texts, and its tags once a tag is
/// asked about.
#[derive(Debug, Default)]
pub(crate) struct DocKeys {
    text: IdSet,
    tags: OnceCell<IdSet>,
}

impl DocKeys {
    pub(crate) fn new(name: &str, files: &[FileInfo]) -> DocKeys {
        let mut scan = Scan {
            out: IdSet::with_capacity_and_hasher(256, Default::default()),
            seen: [None; SEEN],
        };
        scan.text(name);
        // A path's segments are joined by '/', which never joins tokens,
        // so each segment is its own text, and folders shared with the
        // previous file add nothing new.
        let mut folder: &[String] = &[];
        for f in files.iter().take(crate::MAX_FILES) {
            let Some((file, dirs)) = f.path.split_last() else {
                continue;
            };
            if dirs != folder {
                for seg in dirs {
                    scan.text(seg);
                }
                folder = dirs;
            }
            scan.text(file);
        }
        DocKeys {
            text: scan.out,
            tags: OnceCell::new(),
        }
    }

    fn tags(&self, tags: Tags<'_>) -> &IdSet {
        self.tags.get_or_init(tags)
    }

    fn has(&self, key: u64, tags: Tags<'_>) -> bool {
        self.text.contains(&key) || self.tags(tags).contains(&key)
    }

    /// Whether a query with this need could match.
    pub(crate) fn admits(&self, need: &Need, tags: Tags<'_>) -> bool {
        match need {
            Need::Always => true,
            Need::Never => false,
            Need::Any(keys) => keys.iter().any(|k| self.has(*k, tags)),
        }
    }
}

/// The tag keys of an enrichment.
pub(crate) fn tag_keys(e: Option<&DocEnrichment>) -> IdSet {
    e.map(|e| e.tags.iter().map(|t| tag_key(t)).collect())
        .unwrap_or_default()
}

impl DocKeys {}

/// Slots of the atom cache.
const SEEN: usize = 64;

/// Collects a torrent's keys. Words repeat across file names, so a small
/// cache of words already split (compared byte for byte, so it can't be
/// fooled) skips splitting them again.
struct Scan<'a> {
    out: IdSet,
    seen: [Option<&'a [u8]>; SEEN],
}

impl<'a> Scan<'a> {
    /// Adds the token texts [`tokenize`] produces for `text`.
    fn text(&mut self, text: &'a str) {
        if !text.is_ascii() {
            for t in tokenize(text) {
                self.out.insert(term_hash(&t.text));
            }
            return;
        }
        let b = text.as_bytes();
        let hash = |h: u64, bytes: &[u8]| {
            bytes
                .iter()
                .fold(h, |h, c| fnv_byte(h, c.to_ascii_lowercase()))
        };
        let emit = |out: &mut IdSet, h: u64, len: usize| {
            if len > 0 && len <= MAX_TOKEN_BYTES {
                out.insert(fmix64(h));
            }
        };
        let alnum = |c: u8| ALNUM[c as usize];
        // The previous atom (a run of letters and digits), for joins.
        let mut prev: Option<(usize, usize)> = None;
        let mut i = 0;
        while i < b.len() {
            if !alnum(b[i]) {
                i += 1;
                continue;
            }
            let s = i;
            while i < b.len() && alnum(b[i]) {
                i += 1;
            }
            let e = i;
            let word = &b[s..e];
            // Joined to the previous word by a single '.' or '-'.
            if let Some((ps, pe)) = prev {
                if s == pe + 1
                    && matches!(b[pe], b'.' | b'-')
                    && pe - ps <= 4
                    && word.len() <= 4
                    && joins(&b[ps..pe], b[pe], word)
                {
                    let joined = hash(hash(fnv_start(), &b[ps..pe]), word);
                    emit(&mut self.out, joined, pe - ps + word.len());
                }
            }
            prev = Some((s, e));
            // A trailing plus: DD+, C++.
            if b.get(e) == Some(&b'+') {
                let pluses = b[e..].iter().take_while(|&&c| c == b'+').count();
                if pluses <= 2 {
                    let plus = hash(hash(fnv_start(), word), &b[e..e + pluses]);
                    emit(&mut self.out, plus, word.len() + pluses);
                }
            }
            // A word already split adds nothing new.
            let slot = &mut self.seen[slot(word)];
            if slot.is_some_and(|w| same(w, word)) {
                continue;
            }
            *slot = Some(word);
            // A word is emitted whole when it splits, and as its one part
            // when it doesn't: its text is a key either way.
            emit(&mut self.out, hash(fnv_start(), word), word.len());
            if word.len() <= MAX_SPLIT_CHARS {
                let mut from = 0;
                for cut in cuts(word) {
                    emit(
                        &mut self.out,
                        hash(fnv_start(), &word[from..cut]),
                        cut - from,
                    );
                    from = cut;
                }
                if from > 0 {
                    emit(
                        &mut self.out,
                        hash(fnv_start(), &word[from..]),
                        word.len() - from,
                    );
                }
            }
        }
    }
}

/// ASCII letters and digits.
const ALNUM: [bool; 256] = {
    let mut t = [false; 256];
    let mut c = 0;
    while c < 256 {
        t[c] = (c as u8).is_ascii_alphanumeric();
        c += 1;
    }
    t
};

/// Short words compared without a library call.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

/// A word's slot in the cache: cheap, since a hit compares the bytes.
fn slot(word: &[u8]) -> usize {
    let n = word.len();
    let mix = (n as u64)
        ^ (u64::from(word[0]) << 8)
        ^ (u64::from(word[n / 2]) << 16)
        ^ (u64::from(word[n - 1]) << 24)
        ^ (u64::from(word[n.saturating_sub(2)]) << 32);
    (fmix64(mix) as usize) % SEEN
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Upper,
    Lower,
    Digit,
}

fn class(c: u8) -> Class {
    if c.is_ascii_digit() {
        Class::Digit
    } else if c.is_ascii_uppercase() {
        Class::Upper
    } else {
        Class::Lower
    }
}

/// The tokenizer's part boundaries in an ASCII word: letter/digit edges,
/// camelCase, and before the last capital of a capital run.
fn cuts(word: &[u8]) -> impl Iterator<Item = usize> + '_ {
    (1..word.len()).filter(move |&i| {
        let (a, b) = (class(word[i - 1]), class(word[i]));
        let digit_edge = (a == Class::Digit) != (b == Class::Digit);
        let lower_upper = a == Class::Lower && b == Class::Upper;
        let acronym_end = a == Class::Upper
            && b == Class::Upper
            && word.get(i + 1).is_some_and(|&n| class(n) == Class::Lower);
        digit_edge || lower_upper || acronym_end
    })
}

/// The tokenizer's rule for two words joined by `sep` (5.1, H.264,
/// WEB-DL, 24-96).
fn joins(a: &[u8], sep: u8, b: &[u8]) -> bool {
    let digits = |w: &[u8]| w.len() <= 3 && w.iter().all(u8::is_ascii_digit);
    let letters = |w: &[u8]| w.iter().all(u8::is_ascii_alphabetic);
    match sep {
        b'.' => (digits(a) && digits(b)) || (letters(a) && a.len() <= 2 && digits(b)),
        b'-' => {
            (digits(a) && digits(b))
                || (letters(a) && a.len() <= 4 && (letters(b) || digits(b)) && b.len() <= 4)
        }
        _ => false,
    }
}

/// Your rules, indexed by what they need. Rebuilt whenever the list is
/// changed (any mutable access), so it never goes stale.
#[derive(Debug, Clone, Default)]
pub struct RuleList {
    rules: Vec<UserRule>,
    index: std::sync::Arc<std::sync::OnceLock<RuleIndex>>,
}

#[derive(Debug, Default)]
struct RuleIndex {
    /// Rules nothing narrows, by position.
    always: Vec<u32>,
    by_key: IdMap<Vec<u32>>,
}

impl RuleList {
    fn index(&self) -> &RuleIndex {
        self.index.get_or_init(|| {
            let mut ix = RuleIndex::default();
            for (i, r) in self.rules.iter().enumerate() {
                match need(&r.query) {
                    Need::Always => ix.always.push(i as u32),
                    Need::Never => {}
                    Need::Any(keys) => {
                        for k in keys {
                            let v = ix.by_key.entry(k).or_default();
                            if v.last() != Some(&(i as u32)) {
                                v.push(i as u32);
                            }
                        }
                    }
                }
            }
            ix
        })
    }

    /// The rules that could match a torrent with these keys, in order.
    pub(crate) fn candidates(&self, keys: &DocKeys, tags: Tags<'_>) -> Vec<&UserRule> {
        let ix = self.index();
        let mut at: Vec<u32> = ix.always.clone();
        if !ix.by_key.is_empty() {
            for k in keys.text.iter().chain(keys.tags(tags)) {
                if let Some(rules) = ix.by_key.get(k) {
                    at.extend(rules);
                }
            }
        }
        at.sort_unstable();
        at.dedup();
        at.into_iter().map(|i| &self.rules[i as usize]).collect()
    }
}

impl From<Vec<UserRule>> for RuleList {
    fn from(rules: Vec<UserRule>) -> RuleList {
        RuleList {
            rules,
            index: Default::default(),
        }
    }
}

impl std::ops::Deref for RuleList {
    type Target = Vec<UserRule>;

    fn deref(&self) -> &Vec<UserRule> {
        &self.rules
    }
}

impl std::ops::DerefMut for RuleList {
    fn deref_mut(&mut self) -> &mut Vec<UserRule> {
        self.index = Default::default();
        &mut self.rules
    }
}

impl FromIterator<UserRule> for RuleList {
    fn from_iter<I: IntoIterator<Item = UserRule>>(iter: I) -> RuleList {
        RuleList::from(iter.into_iter().collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_search::parse;
    use proptest::prelude::*;

    fn tokenizer_keys(text: &str) -> IdSet {
        tokenize(text)
            .into_iter()
            .map(|t| term_hash(&t.text))
            .collect()
    }

    fn scan_keys(text: &str) -> IdSet {
        DocKeys::new(text, &[]).text
    }

    proptest! {
        /// The byte scan produces exactly the tokenizer's token texts.
        #[test]
        fn ascii_keys_are_the_tokenizers(text in "[ -~]{0,120}") {
            prop_assert_eq!(scan_keys(&text), tokenizer_keys(&text));
        }

        #[test]
        fn release_like_keys_are_the_tokenizers(
            parts in prop::collection::vec("[A-Za-z]{1,6}|[0-9]{1,4}|[A-Z][a-z]{1,5}|x26[45]|DD\\+|5\\.1|WEB-DL|[a-z]{1,3}[A-Z]{2,4}[a-z]{1,3}", 1..12),
            seps in prop::collection::vec("[ ._\\-+()\\[\\]]{1,2}", 12),
        ) {
            let mut text = String::new();
            for (i, p) in parts.iter().enumerate() {
                text.push_str(p);
                text.push_str(&seps[i]);
            }
            prop_assert_eq!(scan_keys(&text), tokenizer_keys(&text));
        }

        /// Any text: a rule the full match accepts is never skipped.
        #[test]
        fn needs_are_necessary(
            name in "\\PC{0,60}",
            word in "[a-zA-Z0-9]{1,8}|\\PC{1,6}",
        ) {
            let item = crate::core::Item {
                hashes: &[[0; 20]],
                name: &name,
                files: &[],
                content_type: lodestar_classify::ContentType::Other,
            };
            let keys = DocKeys::new(&name, &[]);
            for q in [word.clone(), format!("\"{word} x\""), format!("{word} NEAR/2 x")] {
                let Ok(node) = parse(&q) else { continue };
                let full = crate::matcher::Doc::new(&item, None).eval_full(&node);
                if full {
                    prop_assert!(keys.admits(&need(&node), &IdSet::default), "{q:?} on {name:?}");
                }
            }
        }
    }

    #[test]
    fn needs() {
        let n = |q: &str| need(&parse(q).unwrap());
        let k = |t: &str| term_hash(t);
        assert_eq!(n("matrix"), Need::Any(vec![k("matrix")]));
        // A compound query word: its whole form, or its first part.
        assert_eq!(n("x264"), Need::Any(vec![k("x264"), k("x")]));
        assert_eq!(n("\"the matrix\""), Need::Any(vec![k("the")]));
        // The narrower side of AND; both sides of OR.
        assert_eq!(n("matrix size:>1GB"), Need::Any(vec![k("matrix")]));
        assert_eq!(n("cam | ts"), Need::Any(vec![k("cam"), k("ts")]));
        assert_eq!(n("cam | size:>1GB"), Need::Always);
        assert_eq!(n("-cam"), Need::Always);
        assert_eq!(n("mat*"), Need::Always);
        assert_eq!(n("title:matrix"), Need::Always);
        assert_eq!(
            n("file:(path:matrix ext:mkv)"),
            Need::Any(vec![k("matrix")])
        );
        assert_eq!(n("group:grp"), Need::Any(vec![tag_key("group:grp")]));
        assert_eq!(n("ext:exe"), Need::Always);
    }

    #[test]
    fn segments_match_the_joined_path() {
        let f = FileInfo::new(
            vec!["Show.Name.S01".into(), "Show.Name.S01E01.WEB-DL.mkv".into()],
            1,
        );
        let keys = DocKeys::new("", std::slice::from_ref(&f));
        assert_eq!(keys.text, tokenizer_keys(&f.path.join("/")));
    }

    #[test]
    fn the_index_follows_changes() {
        let rule = |id: u32, q: &str| UserRule {
            id,
            query: parse(q).unwrap(),
            action: crate::Action::Hide,
            enabled: true,
            shadow_until: None,
            unless: None,
        };
        let mut list = RuleList::from(vec![
            rule(1, "alpha"),
            rule(2, "size:>1GB"),
            rule(3, "beta"),
        ]);
        let keys = DocKeys::new("Alpha.Gamma", &[]);
        let ids = |l: &RuleList| {
            l.candidates(&keys, &IdSet::default)
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&list), [1, 2]);
        let shared = list.clone();
        list[2].query = parse("gamma").unwrap();
        assert_eq!(ids(&list), [1, 2, 3]);
        assert_eq!(ids(&shared), [1, 2], "a clone keeps its own index");
        list.push(rule(4, "alpha | delta"));
        assert_eq!(ids(&list), [1, 2, 3, 4]);
    }
}
