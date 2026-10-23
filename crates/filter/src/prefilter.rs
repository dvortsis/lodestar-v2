//! Narrowing which rules can match a torrent before any of them runs
//! (§20.8: under 50 µs per torrent at 10,000 rules).
//!
//! A query names text it can't match without: a word needs one of its
//! token texts in the torrent, a phrase each of its words, `group:x` that
//! tag. That is its [`Need`]; where several texts are each needed, the
//! rarest-looking stands for them, so fewer rules are looked at.
//!
//! A torrent's [`DocKeys`] are the hashes of every token text the search
//! tokenizer would produce for its name and files. A rule whose need isn't among them can't match, so it isn't
//! looked at. Those that are get the matcher's cheap check next
//! ([`crate::matcher`]), and only then the full match.
//!
//! The narrowing must never skip a rule that would match. For ASCII text
//! (most names) the keys come from a byte scan that produces exactly the
//! token texts [`tokenize`] does (property-tested against it); other text
//! goes through the tokenizer itself. Queries that no text narrows
//! (sizes, extensions, prefixes, negations) are always looked at.

use lodestar_classify::FileInfo;
use lodestar_search::query::TextField;
use lodestar_search::tokenizer::{MAX_TOKEN_BYTES, TokKind, tokenize};
use lodestar_search::{DocEnrichment, Node};

/// A torrent's tag keys, built on first use.
pub(crate) type Tags<'a, 't> = &'a dyn Fn() -> &'t IdSet;

/// Whether a torrent passes a [`Gate`].
pub(crate) type Gates<'a> = &'a dyn Fn(Gate) -> bool;

use crate::hash::{IdMap, IdSet, fmix64, fnv_byte, fnv_start, term_hash};
use crate::matcher::{Compiled, compile, gate};
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

/// A second condition a rule can't match without, checked while its
/// rules are marked, so the commonest shapes on common words (`"mkv
/// 1080p"`, `1080p size:>3TB`, `x264 type:software`) are turned away
/// without reading the rule. Each is one of the cheap check's own
/// answers ([`crate::matcher`]) on a clause every match needs, so it
/// says *no* only where the cheap check, and so the full match, must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    None,
    /// The torrent's size (`size:`) within these, inclusive.
    Size(u64, u64),
    /// A file's size within these (`file:(… size:…)`).
    FileSize(u64, u64),
    /// This tag.
    Tag(u64),
    /// A token text that could start with this ([`start_key`]).
    Start(u64),
    /// These two token texts in a row (a phrase's first two words).
    Pair(u64, u64),
}

impl Gate {
    /// Of two gates a rule has, whether this one is the better: cheaper
    /// to check, and turning more away.
    fn rank(self) -> u8 {
        match self {
            Gate::None => 0,
            Gate::Pair(..) => 1,
            Gate::Start(_) => 2,
            Gate::Tag(_) => 3,
            Gate::Size(..) | Gate::FileSize(..) => 4,
        }
    }

    pub(crate) fn better(self, other: Gate) -> Gate {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// How rare a need looks: the shortest of its texts, or 0 for words
/// most release names have (`1080p`, `mkv`, `web`, numbers, one or two
/// letters). Of an AND's needs, the index keys a rule on the rarest, so
/// `1080p cam` is looked at only for torrents that have `cam`. Any need
/// is correct; this only decides how many rules a torrent meets.
fn rarity(text: &str) -> usize {
    const COMMON: &[&str] = &[
        "mkv", "mp4", "avi", "m4v", "ts", "srt", "sub", "subs", "idx", "ass", "nfo", "txt", "jpg",
        "png", "sfv", "sample", "web", "webdl", "webrip", "bluray", "bdrip", "brrip", "hdtv",
        "dvdrip", "remux", "x264", "x265", "h264", "h265", "hevc", "avc", "av1", "xvid", "aac",
        "ac3", "eac3", "dts", "ddp", "dd+", "atmos", "truehd", "flac", "mp3", "hdr", "hdr10", "dv",
        "uhd", "proper", "repack", "internal", "complete", "season", "episode", "the", "and", "of",
        "eng", "multi", "subbed", "dubbed",
    ];
    let digits = text.bytes().all(|b| b.is_ascii_digit());
    // Resolutions, seasons and episodes: 1080p, s01, e02.
    let numbered = |p: &[u8]| {
        text.len() > 1
            && text.bytes().take(1).all(|b| p.contains(&b))
            && text.bytes().skip(1).all(|b| b.is_ascii_digit())
    };
    let resolution =
        text.ends_with('p') && text[..text.len() - 1].bytes().all(|b| b.is_ascii_digit());
    if text.len() <= 2 || digits || resolution || numbered(b"se") || COMMON.contains(&text) {
        0
    } else {
        text.chars().count()
    }
}

/// [`Need::Any`] with how rare its keys look: the commonest of them,
/// since any one present admits the rule.
type Keyed = (Vec<u64>, usize);

/// Words longer than this (in chars) aren't split (the tokenizer's
/// `MAX_SPLIT_CHARS`).
const MAX_SPLIT_CHARS: usize = 24;

/// A tag's key, apart from text keys.
pub(crate) fn tag_key(tag: &str) -> u64 {
    let mut h = fnv_start();
    for b in b"\x01tag\x01".iter().chain(tag.as_bytes()) {
        h = fnv_byte(h, *b);
    }
    fmix64(h)
}

/// The need of a query. Inside `file:(…)`, every text field reads that
/// file's name or path, so titles narrow there too.
pub(crate) fn need(node: &Node) -> Need {
    match need_in(node, false) {
        Ok(Some((keys, _))) => Need::Any(keys),
        Ok(None) => Need::Always,
        Err(()) => Need::Never,
    }
}

/// Of keys that are all needed, whether `k` narrows better than `b`:
/// rarer, then fewer.
fn better(k: &Keyed, b: &Keyed) -> bool {
    (k.1, std::cmp::Reverse(k.0.len())) > (b.1, std::cmp::Reverse(b.0.len()))
}

/// `Ok(None)` for nothing narrowing it, `Err` for never matching.
fn need_in(node: &Node, in_file: bool) -> Result<Option<Keyed>, ()> {
    let text = |field: &TextField| in_file || *field != TextField::Title;
    let any = |texts: Vec<String>| -> Result<Option<Keyed>, ()> {
        let rare = texts.iter().map(|t| rarity(t)).min().ok_or(())?;
        Ok(Some((texts.iter().map(|t| term_hash(t)).collect(), rare)))
    };
    // The rarest of several keys, all of which are needed.
    let rarest = |texts: Vec<String>| -> Result<Option<Keyed>, ()> {
        let t = texts.into_iter().max_by_key(|t| rarity(t)).ok_or(())?;
        any(vec![t])
    };
    match node {
        Node::And { of } => {
            let mut best: Option<Keyed> = None;
            for n in of {
                if let Some(k) = need_in(n, in_file)? {
                    if best.as_ref().is_none_or(|b| better(&k, b)) {
                        best = Some(k);
                    }
                }
            }
            Ok(best)
        }
        Node::Or { of } => {
            let mut keys = Vec::new();
            let mut rare = usize::MAX;
            for n in of {
                match need_in(n, in_file) {
                    Err(()) => {}
                    Ok(None) => return Ok(None),
                    Ok(Some((k, r))) => {
                        keys.extend(k);
                        rare = rare.min(r);
                    }
                }
            }
            if keys.is_empty() {
                Err(())
            } else {
                Ok(Some((keys, rare)))
            }
        }
        // A whole or compound token, or the parts in a row: every part
        // is needed then, so the rarest of them stands for them.
        Node::Word { field, word } if text(field) => {
            let q = tokenize(word);
            let mut texts: Vec<String> = q
                .iter()
                .filter(|t| t.kind != TokKind::Part)
                .map(|t| t.text.clone())
                .collect();
            if let Some(p) = q
                .iter()
                .filter(|t| t.kind == TokKind::Part)
                .max_by_key(|t| rarity(&t.text))
            {
                texts.push(p.text.clone());
            }
            any(texts)
        }
        Node::Phrase { field, text: t } if text(field) => rarest(
            tokenize(t)
                .into_iter()
                .filter(|t| t.kind == TokKind::Part)
                .map(|t| t.text)
                .collect(),
        ),
        // Every word is needed, each as a word is: a covering form, or
        // its parts (the rarest standing for them). The rarest word's
        // keys stand for the NEAR, as in an AND.
        Node::Near { field, words, .. } if text(field) => {
            let mut best: Option<Keyed> = None;
            for w in words {
                let (parts, mut texts) = crate::matcher::near_operand(w);
                texts.extend(parts.into_iter().max_by_key(|t| rarity(t)));
                let Some(k) = any(texts)? else { continue };
                if best.as_ref().is_none_or(|b| better(&k, b)) {
                    best = Some(k);
                }
            }
            Ok(best)
        }
        Node::File { of } => need_in(of, true),
        Node::Tag { key, any } => {
            if any.is_empty() {
                return Err(());
            }
            // Release groups are rare; types, resolutions and codecs aren't.
            let rare = match key {
                lodestar_search::query::TagKey::Group => {
                    any.iter().map(|v| v.chars().count()).min().unwrap_or(0)
                }
                _ => 0,
            };
            Ok(Some((
                any.iter()
                    .map(|v| tag_key(&format!("{}:{v}", key.as_str())))
                    .collect(),
                rare,
            )))
        }
        _ => Ok(None),
    }
}

/// The keys of one torrent: its token texts, and which could follow
/// which.
#[derive(Debug, Default)]
pub(crate) struct DocKeys {
    /// Each token text's hash, with the vocabulary entry of its text.
    text: IdMap<u32>,
    pairs: Bloom,
    /// The first two, three and four characters of every token text.
    starts: Bloom,
    /// Each distinct token text, for prefixes and fuzzy words.
    vocabulary: Vocabulary,
}

impl DocKeys {
    pub(crate) fn new(name: &str, files: &[FileInfo]) -> DocKeys {
        let bytes = name.len()
            + files
                .iter()
                .take(crate::MAX_FILES)
                .flat_map(|f| &f.path)
                .map(|s| s.len() + 1)
                .sum::<usize>();
        let mut scan = Scan {
            out: IdMap::with_capacity_and_hasher(256, Default::default()),
            pairs: Bloom::for_text(bytes),
            starts: Bloom::for_text(bytes),
            vocabulary: Vocabulary::default(),
            seen: [const { None }; SEEN],
            at: At::default(),
        };
        scan.text(name);
        // A path's segments are joined by '/', which never joins tokens,
        // so each segment is scanned on its own, continuing the path's
        // positions. Folders already scanned add nothing new, so the scan
        // picks up where the longest of them ended (a season's episodes
        // and their subtitles alternate between two folders).
        let mut folders = Folders::default();
        for f in files.iter().take(crate::MAX_FILES) {
            let Some((file, dirs)) = f.path.split_last() else {
                continue;
            };
            let (known, at) = folders.longest(dirs);
            scan.at = at;
            for k in known..dirs.len() {
                scan.text(&dirs[k]);
                folders.add(&dirs[..=k], scan.at);
            }
            scan.text(file);
        }
        DocKeys {
            text: scan.out,
            pairs: scan.pairs,
            starts: scan.starts,
            vocabulary: scan.vocabulary,
        }
    }

    /// Each distinct token text of the name and the paths.
    pub(crate) fn vocabulary(&self) -> impl Iterator<Item = &str> {
        let v = &self.vocabulary;
        v.words.iter().map(|w| v.text(w))
    }

    /// The distinct token texts within `distance` characters of `chars`
    /// long and within `2 × distance` bits of [`char_set`] `has`: those
    /// an edit distance could put within `distance`.
    pub(crate) fn vocabulary_near(
        &self,
        chars: usize,
        has: u64,
        distance: usize,
    ) -> impl Iterator<Item = &str> {
        let v = &self.vocabulary;
        v.words
            .iter()
            .filter(move |w| {
                (w.chars as usize).abs_diff(chars) <= distance
                    && ((w.has ^ has).count_ones() as usize) <= 2 * distance
            })
            .map(|w| v.text(w))
    }

    /// Whether some token text could start with the text [`start_key`]
    /// was given.
    pub(crate) fn could_start(&self, key: u64) -> bool {
        self.starts.contains(key)
    }

    /// Whether token `b` could follow token `a` in the name or a path.
    pub(crate) fn could_follow(&self, a: u64, b: u64) -> bool {
        self.pairs.contains(pair(a, b))
    }

    #[cfg(test)]
    fn has(&self, key: u64, tags: &IdSet) -> bool {
        self.text.contains_key(&key) || tags.contains(&key)
    }

    /// Whether a token text with this hash is in the name or a path.
    pub(crate) fn has_text(&self, key: u64) -> bool {
        self.text.contains_key(&key)
    }

    /// Whether a query with this need could match.
    #[cfg(test)]
    pub(crate) fn admits(&self, need: &Need, tags: &IdSet) -> bool {
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

/// Folders a scan remembers, and how deep: deeper ones continue from
/// their remembered start, so a path costs a comparison of at most its
/// first [`FOLDER_DEPTH`] segments against each, whatever its depth.
const FOLDERS: usize = 8;
const FOLDER_DEPTH: usize = 4;

/// Folders already scanned, with where the scan stood after each.
#[derive(Default)]
struct Folders<'a> {
    seen: [(&'a [String], At); FOLDERS],
    next: usize,
}

impl<'a> Folders<'a> {
    /// How many of `dirs`' first segments were scanned as a folder, and
    /// where the scan stood after them (the start of a path for none).
    fn longest(&self, dirs: &[String]) -> (usize, At) {
        for k in (1..=dirs.len().min(FOLDER_DEPTH)).rev() {
            if let Some((_, at)) = self.seen.iter().find(|(f, _)| *f == &dirs[..k]) {
                return (k, *at);
            }
        }
        (0, At::default())
    }

    fn add(&mut self, folder: &'a [String], at: At) {
        if folder.len() <= FOLDER_DEPTH {
            self.seen[self.next] = (folder, at);
            self.next = (self.next + 1) % FOLDERS;
        }
    }
}

/// Slots of the word cache.
const SEEN: usize = 64;

/// A set of pair keys that may answer *yes* for a key never added
/// (about 2% of the time at its sizing), never *no* for one that was:
/// a *no* is all the cheap check relies on.
#[derive(Debug, Default)]
struct Bloom {
    bits: Vec<u64>,
}

impl Bloom {
    /// Sized for a text of `bytes`: eight bits per byte, a little more
    /// than enough for its pairs, at least 8,192.
    fn for_text(bytes: usize) -> Bloom {
        let bits = (bytes * 8).next_power_of_two().clamp(1 << 13, 1 << 23);
        Bloom {
            bits: vec![0; bits / 64],
        }
    }

    /// The two bits of a key.
    fn probes(&self, key: u64) -> [(usize, u64); 2] {
        let mask = self.bits.len() * 64 - 1;
        let bit = |h: u64| {
            let i = h as usize & mask;
            (i / 64, 1u64 << (i % 64))
        };
        [bit(key), bit(key.rotate_right(32))]
    }

    fn insert(&mut self, key: u64) {
        for (w, b) in self.probes(key) {
            self.bits[w] |= b;
        }
    }

    fn contains(&self, key: u64) -> bool {
        self.bits.is_empty() || self.probes(key).iter().all(|&(w, b)| self.bits[w] & b != 0)
    }
}

/// The key of a text's first two, three or four characters, as
/// [`DocKeys::could_start`] asks it: the text's own first `min(4, len)`
/// characters, or `None` for a text of fewer than two.
pub(crate) fn start_key(text: &str) -> Option<u64> {
    let n = text.chars().count().min(4);
    (n >= 2).then(|| {
        let end = text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
        starts_hash(text[..end].bytes())
    })
}

fn starts_hash(bytes: impl Iterator<Item = u8>) -> u64 {
    fmix64(bytes.fold(fnv_start() ^ 0x5354_4152_5453, fnv_byte))
}

/// The characters a text has, as bits: one per letter and digit, and
/// others folded onto the rest.
pub(crate) fn char_set(text: &str) -> u64 {
    text.chars().fold(0, |m, c| m | char_bit(c))
}

fn char_bit(c: char) -> u64 {
    let bit = match c {
        'a'..='z' => c as u32 - 'a' as u32,
        '0'..='9' => 26 + c as u32 - '0' as u32,
        _ => 36 + c as u32 % 28,
    };
    1 << bit
}

/// Distinct token texts, compared byte for byte, so two texts whose hashes
/// collide are both kept.
#[derive(Debug, Default)]
struct Vocabulary {
    buf: String,
    words: Vec<Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    start: u32,
    end: u32,
    chars: u32,
    has: u64,
    /// The next entry with the same hash, or [`NONE`]: texts whose hashes
    /// collide are chained, so a crafted collision costs its own chain,
    /// not a scan of every text.
    next: u32,
}

const NONE: u32 = u32::MAX;

impl Vocabulary {
    fn text(&self, w: &Entry) -> &str {
        &self.buf[w.start as usize..w.end as usize]
    }

    /// Adds a text, lowercased from ASCII `pieces` or as it is, after
    /// entry `after` of the same hash, and its starts to `starts`.
    fn push(&mut self, text: Piece<'_>, after: u32, starts: &mut Bloom) -> u32 {
        let start = self.buf.len();
        match text {
            Piece::Ascii(pieces) => {
                for p in pieces {
                    self.buf
                        .extend(p.iter().map(|c| char::from(c.to_ascii_lowercase())));
                }
            }
            Piece::Str(t) => self.buf.push_str(t),
        }
        let text = &self.buf[start..];
        if text.is_ascii() {
            add_starts(starts, text.bytes());
        } else {
            add_starts_str(starts, text);
        }
        let i = self.words.len() as u32;
        self.words.push(Entry {
            start: start as u32,
            end: self.buf.len() as u32,
            chars: text.chars().count() as u32,
            has: char_set(text),
            next: NONE,
        });
        if after != NONE {
            self.words[after as usize].next = i;
        }
        i
    }

    /// Whether `text` is in the chain of same-hash entries from `i`, or
    /// else the chain's last entry, to add it after.
    fn find(&self, mut i: u32, text: Piece<'_>) -> Result<(), u32> {
        loop {
            let w = &self.words[i as usize];
            if text.is(self.text(w)) {
                return Ok(());
            }
            if w.next == NONE {
                return Err(i);
            }
            i = w.next;
        }
    }
}

/// A token text: lowercase ASCII from pieces in any case, or a text
/// already folded.
#[derive(Clone, Copy)]
enum Piece<'a> {
    Ascii(&'a [&'a [u8]]),
    Str(&'a str),
}

impl Piece<'_> {
    fn is(self, text: &str) -> bool {
        match self {
            Piece::Str(t) => t == text,
            Piece::Ascii(pieces) => {
                let mut rest = text.as_bytes();
                pieces.iter().all(|p| {
                    let Some((head, tail)) = rest.split_at_checked(p.len()) else {
                        return false;
                    };
                    rest = tail;
                    head.eq_ignore_ascii_case(p)
                }) && rest.is_empty()
            }
        }
    }
}

/// Adds a token's first two, three and four characters, from its
/// lowercase ASCII bytes.
fn add_starts(out: &mut Bloom, text: impl Iterator<Item = u8>) {
    let mut h = fnv_start() ^ 0x5354_4152_5453;
    for (n, b) in text.take(4).enumerate() {
        h = fnv_byte(h, b);
        if n >= 1 {
            out.insert(fmix64(h));
        }
    }
}

/// [`add_starts`] for any token text, by characters.
fn add_starts_str(out: &mut Bloom, text: &str) {
    let chars = text.chars().count();
    for n in 2..=chars.min(4) {
        let end = text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
        out.insert(starts_hash(text[..end].bytes()));
    }
}

/// The key of token `b` at the position after token `a`.
pub(crate) fn pair(a: u64, b: u64) -> u64 {
    fmix64(a.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ b)
}

/// A word's token hashes: the whole word, and its first, second and
/// last parts when it splits.
#[derive(Debug, Default, Clone, Copy)]
struct Word {
    full: u64,
    split: Option<(u64, u64, u64)>,
}

/// The tokens at one position: at most the word or its first part, its
/// whole form, a trailing plus and a join.
#[derive(Debug, Default, Clone, Copy)]
struct Few {
    n: usize,
    v: [u64; 4],
}

impl Few {
    fn one(h: u64) -> Few {
        let mut f = Few::default();
        f.push(h);
        f
    }

    fn push(&mut self, h: u64) {
        self.v[self.n] = h;
        self.n += 1;
    }

    fn get(&self) -> &[u64] {
        &self.v[..self.n]
    }
}

/// Where the scan is in one text (a name, or a path across its
/// segments): the tokens at the last position, at the one before the
/// previous word's last, and the previous word's second part.
#[derive(Debug, Default, Clone, Copy)]
struct At {
    last: Few,
    before: Few,
    second: Option<u64>,
}

/// Collects a torrent's keys, and every pair of tokens at neighbouring
/// positions (with some pairs that aren't, which only costs a full match
/// now and then). Words repeat across file names, so a small cache of
/// words already split (compared byte for byte, so it can't be fooled)
/// skips splitting them again.
struct Scan<'a> {
    out: IdMap<u32>,
    pairs: Bloom,
    starts: Bloom,
    vocabulary: Vocabulary,
    seen: [Option<(&'a [u8], Word)>; SEEN],
    at: At,
}

fn pairs(out: &mut Bloom, a: &[u64], b: &[u64]) {
    for x in a {
        for y in b {
            out.insert(pair(*x, *y));
        }
    }
}

impl<'a> Scan<'a> {
    /// Adds one token of ASCII text, at most [`MAX_TOKEN_BYTES`] long, from
    /// its pieces: its key, and its text and starts when it's new.
    fn token(&mut self, hash: u64, pieces: &[&[u8]]) {
        self.add_text(hash, Piece::Ascii(pieces));
    }

    /// A token text's key, and its text and starts when it's new.
    fn add_text(&mut self, hash: u64, text: Piece<'_>) {
        use std::collections::hash_map::Entry as Slot;
        match self.out.entry(hash) {
            Slot::Vacant(v) => {
                v.insert(self.vocabulary.push(text, NONE, &mut self.starts));
            }
            // Almost always the same text again; a different one whose
            // hash collides is kept as well.
            Slot::Occupied(o) => {
                if let Err(last) = self.vocabulary.find(*o.get(), text) {
                    self.vocabulary.push(text, last, &mut self.starts);
                }
            }
        }
    }

    /// Adds the token texts [`tokenize`] produces for `text`, which
    /// continues the text before it at the next position (a path's next
    /// segment) unless [`Scan::at`] was reset.
    fn text(&mut self, text: &'a str) {
        if !text.is_ascii() {
            self.other(text);
            return;
        }
        let b = text.as_bytes();
        let hash = |h: u64, bytes: &[u8]| {
            bytes
                .iter()
                .fold(h, |h, c| fnv_byte(h, c.to_ascii_lowercase()))
        };
        let fits = |len: usize| len > 0 && len <= MAX_TOKEN_BYTES;
        let alnum = |c: u8| ALNUM[c as usize];
        // The previous word, for joins.
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
            let slot = slot(word);
            let w = match &self.seen[slot] {
                Some((seen, w)) if same(seen, word) => *w,
                _ => {
                    // A word is emitted whole when it splits, and as its
                    // one part when it doesn't: its text is a key either
                    // way. Its own pairs are added once.
                    let full = fmix64(hash(fnv_start(), word));
                    if fits(word.len()) {
                        self.token(full, &[word]);
                    }
                    let mut split = None;
                    if word.len() <= MAX_SPLIT_CHARS && cuts(word).next().is_some() {
                        let (mut first, mut second, mut last) = (0, 0, 0);
                        let mut from = 0;
                        for (n, cut) in cuts(word).chain(std::iter::once(word.len())).enumerate() {
                            let h = fmix64(hash(fnv_start(), &word[from..cut]));
                            if fits(cut - from) {
                                self.token(h, &[&word[from..cut]]);
                            }
                            match n {
                                0 => first = h,
                                1 => {
                                    second = h;
                                    self.pairs.insert(pair(full, h));
                                }
                                _ => {}
                            }
                            if n > 0 {
                                self.pairs.insert(pair(last, h));
                            }
                            last = h;
                            from = cut;
                        }
                        split = Some((first, second, last));
                    }
                    let w = Word { full, split };
                    self.seen[slot] = Some((word, w));
                    w
                }
            };
            // The word's tokens at its first position: the word or its
            // first part, its whole form, a trailing plus.
            let mut first = Few::one(w.full);
            if let Some((p0, ..)) = w.split {
                first.push(p0);
            }
            // A trailing plus: DD+, C++.
            if b.get(e) == Some(&b'+') {
                let pluses = b[e..].iter().take_while(|&&c| c == b'+').count();
                if pluses <= 2 {
                    let plus = fmix64(hash(hash(fnv_start(), word), &b[e..e + pluses]));
                    if fits(word.len() + pluses) {
                        self.token(plus, &[word, &b[e..e + pluses]]);
                    }
                    first.push(plus);
                    if let Some((_, p1, _)) = w.split {
                        self.pairs.insert(pair(plus, p1));
                    }
                }
            }
            // The previous word's last position, then this word's first.
            let mut last = self.at.last;
            pairs(&mut self.pairs, last.get(), first.get());
            // Joined to the previous word by a single '.' or '-': the
            // compound sits at the previous word's first position.
            if let Some((ps, pe)) = prev {
                if s == pe + 1
                    && matches!(b[pe], b'.' | b'-')
                    && pe - ps <= 4
                    && word.len() <= 4
                    && joins(&b[ps..pe], b[pe], word)
                {
                    let joined = fmix64(hash(hash(fnv_start(), &b[ps..pe]), word));
                    if fits(pe - ps + word.len()) {
                        self.token(joined, &[&b[ps..pe], word]);
                    }
                    pairs(&mut self.pairs, self.at.before.get(), &[joined]);
                    match self.at.second {
                        // The previous word split: its second part follows.
                        Some(p1) => {
                            self.pairs.insert(pair(joined, p1));
                        }
                        // It didn't: its first position was its last.
                        None => {
                            pairs(&mut self.pairs, &[joined], first.get());
                            last.push(joined);
                        }
                    }
                }
            }
            prev = Some((s, e));
            self.at = At {
                before: last,
                second: w.split.map(|(_, p1, _)| p1),
                last: match w.split {
                    Some((.., pl)) => Few::one(pl),
                    None => first,
                },
            };
        }
    }

    /// Text with other than ASCII, through the tokenizer.
    fn other(&mut self, text: &str) {
        let toks = tokenize(text);
        for t in &toks {
            self.add_text(term_hash(&t.text), Piece::Str(&t.text));
        }
        let mut by_pos: Vec<(usize, u64)> = toks
            .iter()
            .map(|t| (t.position, term_hash(&t.text)))
            .collect();
        by_pos.sort_unstable();
        let Some(&(lo, _)) = by_pos.first() else {
            return;
        };
        let at_pos =
            |p: usize| -> Vec<u64> { by_pos.iter().filter(|t| t.0 == p).map(|t| t.1).collect() };
        let hi = by_pos[by_pos.len() - 1].0;
        pairs(&mut self.pairs, self.at.last.get(), &at_pos(lo));
        for p in lo..hi {
            pairs(&mut self.pairs, &at_pos(p), &at_pos(p + 1));
        }
        // No join reaches across segments, so only the last position
        // carries over; it holds at most a part, its word and a plus.
        let mut last = Few::default();
        for h in at_pos(hi).into_iter().take(4) {
            last.push(h);
        }
        self.at = At {
            last,
            before: self.at.last,
            second: None,
        };
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
    /// Rules nothing narrows, by position, with their gates.
    always: Vec<(u32, Gate)>,
    by_key: IdMap<Vec<(u32, Gate)>>,
    /// Each rule's query and `unless`, compiled.
    compiled: Vec<(Compiled, Option<Compiled>)>,
    /// Each rule's switch, read without touching the rule.
    enabled: Vec<bool>,
}

impl RuleList {
    fn index(&self) -> &RuleIndex {
        self.index.get_or_init(|| {
            let mut ix = RuleIndex::default();
            for (i, r) in self.rules.iter().enumerate() {
                let query = compile(&r.query);
                let g = gate(&query);
                ix.compiled.push((query, r.unless.as_ref().map(compile)));
                ix.enabled.push(r.enabled);
                let i = i as u32;
                match need(&r.query) {
                    Need::Always => ix.always.push((i, g)),
                    Need::Never => {}
                    Need::Any(keys) => {
                        for k in keys {
                            let v = ix.by_key.entry(k).or_default();
                            if v.last().is_none_or(|l| l.0 != i) {
                                v.push((i, g));
                            }
                        }
                    }
                }
            }
            ix
        })
    }

    /// Rule `i`'s query and `unless`, compiled.
    pub(crate) fn compiled(&self, i: u32) -> (&Compiled, Option<&Compiled>) {
        let (q, u) = &self.index().compiled[i as usize];
        (q, u.as_ref())
    }

    /// The positions of the switched-on rules that could match a torrent
    /// with these keys and passing `gates`, in order.
    pub(crate) fn candidates(
        &self,
        keys: &DocKeys,
        tags: Tags<'_, '_>,
        gates: Gates<'_>,
    ) -> Vec<u32> {
        let ix = self.index();
        // Marked by position, then read in order.
        let mut marks = vec![0u64; self.rules.len().div_ceil(64)];
        let mut mark = |&(i, g): &(u32, Gate)| {
            if ix.enabled[i as usize] && (g == Gate::None || gates(g)) {
                marks[i as usize / 64] |= 1 << (i % 64);
            }
        };
        ix.always.iter().for_each(&mut mark);
        if !ix.by_key.is_empty() {
            for k in keys.text.keys().chain(tags()) {
                if let Some(rules) = ix.by_key.get(k) {
                    rules.iter().for_each(&mut mark);
                }
            }
        }
        let mut at = Vec::new();
        for (w, mut bits) in marks.into_iter().enumerate() {
            while bits != 0 {
                at.push(w as u32 * 64 + bits.trailing_zeros());
                bits &= bits - 1;
            }
        }
        at
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
        DocKeys::new(text, &[]).text.keys().copied().collect()
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

        /// Every pair of tokens at neighbouring positions, in the name and
        /// in each whole path, is among the scanned pairs, and every
        /// token's start among the starts, whatever the text and however
        /// files share folders.
        #[test]
        fn pairs_cover_the_tokenizers(
            name in "\\PC{0,60}|[ -~]{0,80}",
            segs in prop::collection::vec(
                "[A-Za-z]{1,6}([._\\- +]{1,2}[A-Za-z0-9+]{1,6}){0,5}|[0-9]{1,3}[.\\-][0-9]{1,3}|\\PC{0,12}|[a-zA-Z0-9.+-]{0,30}",
                1..6,
            ),
            layout in prop::collection::vec((0usize..6, 0usize..6, 0usize..7), 0..10),
        ) {
            // Files built from the segments, often sharing folders.
            let files: Vec<FileInfo> = layout
                .iter()
                .map(|&(a, b, depth)| {
                    let mut path: Vec<String> =
                        (0..depth).map(|d| segs[(a + d) % segs.len()].clone()).collect();
                    path.push(segs[b % segs.len()].clone());
                    FileInfo::new(path, 1)
                })
                .collect();
            let keys = DocKeys::new(&name, &files);
            let texts = std::iter::once(name.clone())
                .chain(files.iter().map(|f| f.path.join("/")));
            // The vocabulary is exactly the token texts, each once.
            let all: std::collections::BTreeSet<String> = texts
                .clone()
                .flat_map(|t| tokenize(&t).into_iter().map(|t| t.text))
                .collect();
            let vocabulary: Vec<&str> = keys.vocabulary().collect();
            prop_assert_eq!(vocabulary.len(), all.len());
            prop_assert!(vocabulary.iter().all(|t| all.contains(*t)));
            for w in &keys.vocabulary.words {
                let t = keys.vocabulary.text(w);
                prop_assert_eq!(w.chars as usize, t.chars().count());
                prop_assert_eq!(w.has, char_set(t));
            }
            for text in texts {
                let toks = tokenize(&text);
                // Every token's start, for prefixes.
                for t in &toks {
                    for n in 2..=t.text.chars().count() {
                        let start: String = t.text.chars().take(n).collect();
                        if let Some(k) = start_key(&start) {
                            prop_assert!(keys.could_start(k), "{:?} of {:?} in {:?}", start, t.text, text);
                        }
                    }
                }
                for a in &toks {
                    for b in toks.iter().filter(|b| b.position == a.position + 1) {
                        prop_assert!(
                            keys.could_follow(term_hash(&a.text), term_hash(&b.text)),
                            "{:?} then {:?} in {:?}", a.text, b.text, text
                        );
                    }
                }
            }
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
                    prop_assert!(keys.admits(&need(&node), &IdSet::default()), "{q:?} on {name:?}");
                }
            }
        }
    }

    #[test]
    fn needs() {
        let n = |q: &str| need(&parse(q).unwrap());
        let k = |t: &str| term_hash(t);
        assert_eq!(n("matrix"), Need::Any(vec![k("matrix")]));
        // A compound query word: its whole form, or one of its parts.
        assert_eq!(n("x264"), Need::Any(vec![k("x264"), k("264")]));
        // A phrase or AND by its rarest word.
        assert_eq!(n("\"the matrix\""), Need::Any(vec![k("matrix")]));
        assert_eq!(n("1080p cam"), Need::Any(vec![k("cam")]));
        assert_eq!(n("cam 1080p"), Need::Any(vec![k("cam")]));
        assert_eq!(n("mkv NEAR/2 matrix"), Need::Any(vec![k("matrix")]));
        // A NEAR word of several tokens is needed as a word is.
        assert_eq!(n("1080p NEAR/2 matrix"), Need::Any(vec![k("matrix")]));
        assert_eq!(n("bluray NEAR/2 web-dl"), Need::Any(vec![k("bluray")]));
        assert_eq!(n("1080p NEAR/2 x264"), Need::Any(vec![k("1080p"), k("p")]));
        assert_eq!(n("1080p NEAR/2 \"++\""), Need::Never);
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

    /// Texts whose hashes collide are chained, each kept and found by
    /// its bytes, in any case.
    #[test]
    fn colliding_texts_are_chained() {
        let mut v = Vocabulary::default();
        let mut starts = Bloom::for_text(0);
        let a = v.push(Piece::Str("alpha"), NONE, &mut starts);
        assert_eq!(v.find(a, Piece::Ascii(&[b"ALPHA"])), Ok(()));
        assert_eq!(v.find(a, Piece::Ascii(&[b"AL", b"pha"])), Ok(()));
        assert_eq!(v.find(a, Piece::Ascii(&[b"alph"])), Err(a));
        assert_eq!(v.find(a, Piece::Ascii(&[b"alphas"])), Err(a));
        // As if "beta" had alpha's hash.
        let b = v.push(Piece::Ascii(&[b"BETA"]), a, &mut starts);
        assert_eq!(v.find(a, Piece::Str("beta")), Ok(()));
        assert_eq!(v.find(a, Piece::Str("gamma")), Err(b));
        assert_eq!(v.text(&v.words[b as usize]), "beta");
        assert!(starts.contains(start_key("be").unwrap()));
        assert!(starts.contains(start_key("alph").unwrap()));
    }

    #[test]
    fn segments_match_the_joined_path() {
        let f = FileInfo::new(
            vec!["Show.Name.S01".into(), "Show.Name.S01E01.WEB-DL.mkv".into()],
            1,
        );
        let keys = DocKeys::new("", std::slice::from_ref(&f));
        let keys: IdSet = keys.text.keys().copied().collect();
        assert_eq!(keys, tokenizer_keys(&f.path.join("/")));
    }

    #[test]
    fn gates() {
        let g = |q: &str| crate::matcher::gate(&compile(&parse(q).unwrap()));
        let k = |t: &str| term_hash(t);
        const GB: u64 = 1 << 30;
        assert_eq!(g("x264 size:>3GB"), Gate::Size(3 * GB + 1, u64::MAX));
        assert_eq!(g("file:(x264 size:<1GB)"), Gate::FileSize(0, GB - 1));
        assert_eq!(g("x264 type:software"), Gate::Tag(tag_key("type:software")));
        assert_eq!(g("\"mkv sample\""), Gate::Pair(k("mkv"), k("sample")));
        // A phrase's first two tokens: `1080p` is `1080` and `p`.
        assert_eq!(g("\"1080p mkv\""), Gate::Pair(k("1080"), k("p")));
        assert_eq!(
            g("mkv \"1080p x264\" size:>1GB"),
            Gate::Size(GB + 1, u64::MAX)
        );
        assert_eq!(
            g("webdz*"),
            Gate::Start(starts_hash(b"webd".iter().copied()))
        );
        // Nothing every match needs, or nothing the gate can read.
        assert_eq!(g("x264 | size:>3GB"), Gate::None);
        assert_eq!(g("-(size:>3GB)"), Gate::None);
        assert_eq!(g("title:\"mkv 1080p\""), Gate::None);
        assert_eq!(g("x264 (type:software | type:movie)"), Gate::None);
        assert_eq!(g("x264"), Gate::None);
    }

    /// Rules are turned away by their gates as the cheap check would.
    #[test]
    fn gated_rules_are_met_only_when_they_pass() {
        let rule = |id: u32, q: &str| UserRule {
            id,
            query: parse(q).unwrap(),
            action: crate::Action::Hide,
            enabled: true,
            shadow_until: None,
            unless: None,
        };
        let list = RuleList::from(vec![
            rule(1, "x264 size:>3GB"),
            rule(2, "\"1080p x264\""),
            rule(3, "\"x264 1080p\""),
            rule(4, "file:(x264 size:<1MB)"),
            rule(5, "size:<1GB"),
            rule(6, "sho*"),
            rule(7, "shx*"),
        ]);
        let ids = |size: u64| {
            let files = [FileInfo::new(vec!["Show.1080p.x264.mkv".into()], size)];
            let item = crate::core::Item {
                hashes: &[[0; 20]],
                name: "Show.1080p.x264",
                files: &files,
                content_type: lodestar_classify::ContentType::Video,
            };
            let doc = crate::matcher::Doc::new(&item, None);
            doc.candidates(&list)
                .iter()
                .map(|&i| list[i as usize].id)
                .collect::<Vec<_>>()
        };
        // `"x264 1080p"` passes its gate (`x` then `264`), and the cheap
        // check turns it away.
        assert_eq!(ids(4 << 30), [1, 2, 3, 6]);
        assert_eq!(ids(1 << 19), [2, 3, 4, 5, 6]);
    }

    #[test]
    fn the_index_follows_changes() {
        static NO_TAGS: std::sync::LazyLock<IdSet> = std::sync::LazyLock::new(IdSet::default);
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
            l.candidates(&keys, &|| &NO_TAGS, &|_| true)
                .iter()
                .map(|&i| l[i as usize].id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&list), [1, 2]);
        let shared = list.clone();
        list[2].query = parse("gamma").unwrap();
        assert_eq!(ids(&list), [1, 2, 3]);
        assert_eq!(ids(&shared), [1, 2], "a clone keeps its own index");
        list.push(rule(4, "alpha | delta"));
        assert_eq!(ids(&list), [1, 2, 3, 4]);
        // A rule switched off isn't looked at, until it's switched on.
        list[0].enabled = false;
        assert_eq!(ids(&list), [2, 3, 4]);
        list[0].enabled = true;
        assert_eq!(ids(&list), [1, 2, 3, 4]);
    }
}
