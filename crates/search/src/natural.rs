//! Natural phrasing (§10.4, B7): plain words like "4k movies from 2020
//! over 10gb" read as the query `res:2160p type:movie year:2020
//! size:>=10GB`, for the UI to preview as chips. The server never applies
//! it; `/parse` only proposes it.
//!
//! It's deterministic and rule-based: each rule recognises a few words
//! (a resolution, a content type, a year or range of years, a size or
//! file-count comparison, a recency, a season or episode, a language,
//! an extension, a codec, subtitles, a negation of any of those), and
//! every other word stays as text. Values of one field are merged into
//! one clause (`movies and tv shows` is `type:movie,tv`, either), and a
//! linking word (`and`, `in`, `with`, …) next to a recognised one is
//! dropped. The result is always a query the parser reads back exactly.
//!
//! **Explicit syntax is kept as written.** The input is split the way
//! the query lexer splits it, and only runs of plain words (each a word
//! the parser reads as itself, outside brackets and away from `NEAR`)
//! go through the rules. Everything else (`-x265`, `!cam`, `res:1080p`,
//! `ext:mkv,avi`, `"a phrase"`, `OR`, `(a | b)`) is copied verbatim, so
//! natural phrasing never changes what a token in query syntax means.
//! Input the strict parser rejects isn't read (`/parse` answers it with
//! the syntax error).
//!
//! Sizes: "over"/"under" are inclusive (`size:>=10GB`), since a byte
//! either way means nothing; years and counts keep the strict sense
//! ("after 2010" is `year:>=2011`).

use crate::query::{self, DateField, Node, NumField, TagKey, TextField, When};
use crate::suggest::caret::{self, Raw, RawKind};
use crate::suggest::chip_nodes;

/// Longest input read, in bytes.
pub const MAX_NATURAL_BYTES: usize = 512;
/// Most tokens read (words, operators and brackets).
pub const MAX_NATURAL_WORDS: usize = 32;

/// Why a phrase wasn't read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NaturalError {
    TooLong,
    TooManyWords,
    /// Not a query the strict parser reads.
    Syntax,
}

impl std::fmt::Display for NaturalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NaturalError::TooLong => write!(f, "longer than {MAX_NATURAL_BYTES} bytes"),
            NaturalError::TooManyWords => {
                write!(f, "more than {MAX_NATURAL_WORDS} words and operators")
            }
            NaturalError::Syntax => write!(f, "not a query the parser reads"),
        }
    }
}

impl std::error::Error for NaturalError {}

/// A recognised clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaturalChip {
    /// The clause as query text (`res:2160p`).
    pub text: String,
    /// The character range of the words it came from (the first
    /// occurrence, for merged values).
    pub from: (usize, usize),
}

/// The proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Natural {
    pub ast: Node,
    /// `ast` as canonical query text.
    pub canonical: String,
    /// The recognised clauses, in the order they appear.
    pub chips: Vec<NaturalChip>,
    /// The words kept as text.
    pub text: Vec<String>,
    /// The character ranges of every word a rule read, merged values'
    /// included (a chip's `from` is its first word's).
    pub read: Vec<(usize, usize)>,
}

/// A word of the input: as written (outer punctuation trimmed) and
/// lowercased, with its character range.
#[derive(Debug, Clone)]
struct W {
    /// The token as written, untrimmed.
    raw: String,
    orig: String,
    low: String,
    from: (usize, usize),
}

/// The word a plain token is: outer punctuation trimmed for matching
/// (`4k,` reads as `4k`), with its character range in `input`.
fn word(input: &str, t: &Raw) -> Option<W> {
    let raw = &input[t.start..t.end];
    let trimmed = raw.trim_matches(|c: char| "[]{},.;:!?'".contains(c));
    // All punctuation (`!`), or a bracket trimmed off one side only
    // (`Naruto[1080p]`): kept as written.
    let unpaired = |o: char, c: char| trimmed.matches(o).count() != trimmed.matches(c).count();
    let trimmed = if trimmed.is_empty() || unpaired('[', ']') || unpaired('{', '}') {
        raw
    } else {
        trimmed
    };
    let lead = raw.find(trimmed).unwrap_or(0);
    let from = caret::chars(input, t.start + lead);
    Some(W {
        raw: raw.to_owned(),
        orig: trimmed.to_owned(),
        low: trimmed.to_lowercase(),
        from: (from, from + trimmed.chars().count()),
    })
}

fn is_near(w: &str) -> bool {
    w == "NEAR" || w.starts_with("NEAR/")
}

/// Whether token `i` is a plain word the rules may read: one the parser
/// reads as itself, apart from its neighbours, outside brackets, and not
/// a `NEAR` operand.
fn plain(input: &str, toks: &[Raw], i: usize) -> bool {
    let t = &toks[i];
    let text = &input[t.start..t.end];
    let near = |j: Option<usize>| {
        j.and_then(|j| toks.get(j))
            .is_some_and(|n| n.kind == RawKind::Word && is_near(&input[n.start..n.end]))
    };
    // Touching its neighbour (`-"a b"`, `title:"x"`), it's part of it.
    let apart =
        |j: Option<usize>, gap: &dyn Fn(&Raw) -> bool| j.and_then(|j| toks.get(j)).is_none_or(gap);
    t.kind == RawKind::Word
        && t.depth == 0
        && apart(i.checked_sub(1), &|p: &Raw| p.end < t.start)
        && apart(Some(i + 1), &|n: &Raw| t.end < n.start)
        && query::is_plain_word(text)
        && !near(i.checked_sub(1))
        && !near(Some(i + 1))
}

/// Words that link recognised ones and mean nothing alone.
const LINKS: &[&str] = &[
    "and", "or", "with", "in", "from", "of", "at", "for", "the", "a", "an", "&", "+", "only",
    "released", "made", "that", "are", "is", "which", "added", "uploaded",
];

/// Articles, dropped only before a recognised word (`the 90s`): before
/// text they're part of it (`the office`).
const ARTICLES: &[&str] = &["the", "a", "an"];

/// Whether a word is one the rules may drop (a linking or request
/// word), for tests that check no other word is lost.
#[doc(hidden)]
pub fn is_filler(word: &str) -> bool {
    let w = word.to_lowercase();
    LINKS.contains(&w.as_str()) || ASKS.contains(&w.as_str())
}

/// Words that ask for a search, dropped at the start.
const ASKS: &[&str] = &["find", "search", "show", "get", "me", "all", "any", "some"];

/// Content types by the words people use, plural forms included.
const TYPES: &[(&[&str], &str)] = &[
    (&["movie", "movies", "film", "films"], "movie"),
    (
        &["tv", "series", "shows", "episodes", "tvshows", "tv-shows"],
        "tv",
    ),
    (&["music", "songs", "albums", "album"], "music"),
    (&["audiobook", "audiobooks"], "audiobook"),
    (&["ebook", "ebooks", "e-book", "e-books", "books"], "ebook"),
    (&["comic", "comics"], "comic"),
    (
        &["software", "apps", "programs", "applications"],
        "software",
    ),
    (&["game", "games"], "game"),
    (&["images", "photos", "pictures", "wallpapers"], "images"),
    (&["videos", "clips"], "video"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Above,
    Below,
}

/// A comparison word or two at `w`: how many, and which way.
fn comparison(w: &[W]) -> Option<(usize, Cmp)> {
    let a = w.first()?.low.as_str();
    let b = w.get(1).map_or("", |x| x.low.as_str());
    Some(match (a, b) {
        ("more" | "bigger" | "larger" | "greater" | "longer", "than") => (2, Cmp::Above),
        ("at", "least") => (2, Cmp::Above),
        ("less" | "smaller" | "fewer" | "shorter", "than") => (2, Cmp::Below),
        ("at", "most") | ("up", "to") => (2, Cmp::Below),
        ("over" | "above" | "exceeding" | ">" | ">=" | "min" | "minimum", _) => (1, Cmp::Above),
        ("under" | "below" | "<" | "<=" | "max" | "maximum", _) => (1, Cmp::Below),
        _ => return None,
    })
}

fn unit(s: &str) -> Option<u64> {
    Some(match s {
        "b" | "byte" | "bytes" => 1,
        "k" | "kb" | "kib" | "kilobytes" => 1 << 10,
        "m" | "mb" | "mib" | "meg" | "megs" | "megabytes" => 1 << 20,
        "g" | "gb" | "gib" | "gig" | "gigs" | "gigabytes" => 1 << 30,
        "t" | "tb" | "tib" | "terabytes" => 1 << 40,
        _ => return None,
    })
}

/// A decimal number of at most 9 digits each side.
fn number(s: &str) -> Option<f64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits = |x: &str| x.len() <= 9 && x.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) {
        return None;
    }
    s.parse().ok()
}

/// A size at `w` (`10gb` or `10 gb`): words used and bytes.
fn size(w: &[W]) -> Option<(usize, u64)> {
    let a = &w.first()?.low;
    let bytes = |n: f64, m: u64| {
        let v = n * m as f64;
        (0.0..1e18).contains(&v).then_some(v as u64)
    };
    let split = a
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(a.len());
    let (n, u) = a.split_at(split);
    let n = number(n)?;
    if !u.is_empty() {
        return Some((1, bytes(n, unit(u)?)?));
    }
    let m = unit(&w.get(1)?.low)?;
    Some((2, bytes(n, m)?))
}

/// A plain count (`10`).
fn count(s: &str) -> Option<u64> {
    (s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// A year 1900–2099.
fn year(s: &str) -> Option<u64> {
    let y = (s.len() == 4).then(|| count(s)).flatten()?;
    (1900..=2099).contains(&y).then_some(y)
}

/// `80s`, `'80s`, `1980s`: the decade's first year.
fn decade(s: &str) -> Option<u64> {
    let d = s.strip_prefix('\'').unwrap_or(s).strip_suffix("s")?;
    match d.len() {
        2 => {
            let n = count(d)?;
            (n % 10 == 0).then_some(if n >= 30 { 1900 + n } else { 2000 + n })
        }
        4 => year(d).filter(|y| y % 10 == 0),
        _ => None,
    }
}

/// `2000-2010` as one word.
fn year_span(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.split_once(['-', '–'])?;
    let (a, b) = (year(a)?, year(b)?);
    (a <= b).then_some((a, b))
}

fn years(lo: u64, hi: u64) -> Node {
    Node::Range {
        field: NumField::Year,
        lo: Some(lo),
        hi: Some(hi),
    }
}

fn range(field: NumField, c: Cmp, v: u64, strict: bool) -> Node {
    let (lo, hi) = match (c, strict) {
        (Cmp::Above, false) => (Some(v), None),
        (Cmp::Above, true) => (Some(v.saturating_add(1)), None),
        (Cmp::Below, false) => (None, Some(v)),
        (Cmp::Below, true) => (None, Some(v.saturating_sub(1))),
    };
    Node::Range { field, lo, hi }
}

fn ago(secs: i64) -> Node {
    Node::Date {
        field: DateField::Discovered,
        from: Some(When::Ago(secs)),
        to: None,
    }
}

const DAY: i64 = 86_400;

/// A time unit word, in seconds.
fn period(s: &str) -> Option<i64> {
    Some(match s {
        "hour" | "hours" => 3600,
        "day" | "days" => DAY,
        "week" | "weeks" => 7 * DAY,
        "month" | "months" => 30 * DAY,
        "year" | "years" => 365 * DAY,
        _ => return None,
    })
}

/// What a rule made of some words.
enum Made {
    Node(Node),
    /// A keyword value, merged with the others of its field.
    Tag(TagKey, String),
    Ext(String),
}

/// The rules, in order, at `w`: words used and the clause.
fn rule(w: &[W]) -> Option<(usize, Made)> {
    let a = w.first()?.low.as_str();
    let b = w.get(1).map_or("", |x| x.low.as_str());
    let c = w.get(2).map_or("", |x| x.low.as_str());
    let d = w.get(3).map_or("", |x| x.low.as_str());
    let node = |n, node| Some((n, Made::Node(node)));
    // Negation of the next clause.
    if matches!(a, "without" | "no" | "not" | "except" | "excluding") {
        if let Some((n, made)) = rule(&w[1..]) {
            let inner = match made {
                Made::Node(n) => n,
                Made::Tag(key, v) => Node::Tag { key, any: vec![v] },
                Made::Ext(e) => Node::Ext { any: vec![e] },
            };
            if !matches!(inner, Node::Not { .. }) {
                return node(
                    n + 1,
                    Node::Not {
                        of: Box::new(inner),
                    },
                );
            }
        }
        return None;
    }
    // Year ranges: "between 2000 and 2010", "from 2000 to 2010",
    // "2000 to 2010", "2000-2010".
    if matches!(a, "between" | "from") {
        if let (Some(lo), true, Some(hi)) = (year(b), matches!(c, "and" | "to" | "-"), year(d))
            && lo <= hi
        {
            return node(4, years(lo, hi));
        }
        if let Some((lo, hi)) = year_span(b) {
            return node(2, years(lo, hi));
        }
        if a == "between"
            && let Some((n, sizes)) = size_between(&w[1..])
        {
            return node(1 + n, sizes);
        }
    }
    if let (Some(lo), true, Some(hi)) = (year(a), matches!(b, "to" | "-"), year(c))
        && lo <= hi
    {
        return node(3, years(lo, hi));
    }
    if let Some((lo, hi)) = year_span(a) {
        return node(1, years(lo, hi));
    }
    match (a, year(b)) {
        ("after", Some(y)) => return node(2, range(NumField::Year, Cmp::Above, y, true)),
        ("since", Some(y)) => return node(2, range(NumField::Year, Cmp::Above, y, false)),
        ("before", Some(y)) => return node(2, range(NumField::Year, Cmp::Below, y, true)),
        ("until" | "till", Some(y)) => {
            return node(2, range(NumField::Year, Cmp::Below, y, false));
        }
        _ => {}
    }
    if let Some(y) = year(a) {
        return node(1, years(y, y));
    }
    if let Some(y) = decade(a) {
        return node(1, years(y, y + 9));
    }
    // Sizes and file counts after a comparison.
    if let Some((n, cmp)) = comparison(w) {
        let rest = &w[n..];
        if let Some((m, bytes)) = size(rest) {
            return node(n + m, range(NumField::Size, cmp, bytes, false));
        }
        if let (Some(k), Some(f)) = (rest.first().and_then(|x| count(&x.low)), rest.get(1))
            && matches!(f.low.as_str(), "files" | "file")
        {
            return node(n + 2, range(NumField::Files, cmp, k, true));
        }
        return None;
    }
    // Recency.
    match (a, b) {
        ("today", _) => return node(1, ago(DAY)),
        ("yesterday", _) => return node(1, ago(2 * DAY)),
        ("this" | "last" | "past", p) if period(p).is_some() => {
            return node(2, ago(period(p)?));
        }
        ("last" | "past", n) if count(n).is_some_and(|n| (1..=1000).contains(&n)) => {
            if let Some(p) = period(c) {
                let n = count(n)? as i64;
                return node(3, ago(n * p));
            }
        }
        ("recent" | "recently" | "latest" | "newest", _) => {
            let n = if b == "added" { 2 } else { 1 };
            return node(n, ago(7 * DAY));
        }
        _ => {}
    }
    // Seasons and episodes.
    let num = |field, n: usize, lo: u64, hi: u64| {
        node(
            n,
            Node::Range {
                field,
                lo: Some(lo),
                hi: Some(hi),
            },
        )
    };
    for (words, field) in [
        (["season", "seasons"], NumField::Season),
        (["episode", "episodes"], NumField::Episode),
    ] {
        if words.contains(&a)
            && let Some(x) = count(b).filter(|x| *x <= 5000)
        {
            if let (true, Some(y)) = (matches!(c, "to" | "-" | "through"), count(d))
                && x <= y
                && y <= 5000
            {
                return num(field, 4, x, y);
            }
            return num(field, 2, x, x);
        }
    }
    // Subtitles.
    if matches!(a, "subtitles" | "subtitle" | "subs" | "subtitled") {
        return Some((1, Made::Tag(TagKey::Has, "subs".into())));
    }
    // Content types.
    if a == "tv" && matches!(b, "shows" | "show" | "series") {
        return Some((2, Made::Tag(TagKey::Type, "tv".into())));
    }
    if let Some((_, t)) = TYPES.iter().find(|(ws, _)| ws.contains(&a)) {
        return Some((1, Made::Tag(TagKey::Type, (*t).into())));
    }
    // Languages: "in french", "french audio".
    let lang = |s: &W| {
        chip_nodes(&s.orig).into_iter().find_map(|n| match n {
            Node::Tag {
                key: TagKey::Lang,
                mut any,
            } => any.pop(),
            _ => None,
        })
    };
    if let Some(l) = lang(&w[0]) {
        let n = if matches!(b, "audio" | "dub" | "dubbed" | "language") {
            2
        } else {
            1
        };
        return Some((n, Made::Tag(TagKey::Lang, l)));
    }
    let Some(made) = recognised(&w[0].orig) else {
        // Two-word terms neither word makes alone (`dolby vision`).
        let second = w.get(1)?;
        if recognised(&second.orig).is_some() {
            return None;
        }
        return Some((2, recognised(&format!("{} {}", w[0].orig, second.orig))?));
    };
    // `flac files`, `mkv format`.
    let n = if matches!(made, Made::Ext(_)) && matches!(b, "files" | "file" | "format") {
        2
    } else {
        1
    };
    Some((n, made))
}

/// `1gb and 4gb` or `1 and 4 gb` (after "between"): words used and the
/// size range.
fn size_between(w: &[W]) -> Option<(usize, Node)> {
    let sizes = |lo: u64, hi: u64| {
        (lo <= hi).then_some(Node::Range {
            field: NumField::Size,
            lo: Some(lo),
            hi: Some(hi),
        })
    };
    if let Some((n1, lo)) = size(w) {
        if w.get(n1)?.low != "and" {
            return None;
        }
        let (n2, hi) = size(&w[n1 + 1..])?;
        return Some((n1 + 1 + n2, sizes(lo, hi)?));
    }
    let lo = count(&w.first()?.low)?;
    if w.get(1)?.low != "and" {
        return None;
    }
    let (n2, hi) = size(&w[2..])?;
    let lo = lo.saturating_mul(unit_of(&w[2..])?);
    Some((2 + n2, sizes(lo, hi)?))
}

/// The unit a size at `w` was given in.
fn unit_of(w: &[W]) -> Option<u64> {
    let a = &w.first()?.low;
    let split = a
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(a.len());
    if split < a.len() {
        unit(&a[split..])
    } else {
        unit(&w.get(1)?.low)
    }
}

/// A token the smart chips recognise, preferring the extension (`flac`
/// files) and leaving years and types to the rules above.
fn recognised(word: &str) -> Option<Made> {
    let nodes = chip_nodes(word);
    if let Some(e) = nodes.iter().find_map(|n| match n {
        Node::Ext { any } => any.first().cloned(),
        _ => None,
    }) {
        return Some(Made::Ext(e));
    }
    nodes.into_iter().find_map(|n| match n {
        Node::Tag { key, mut any } if key != TagKey::Type && key != TagKey::Lang => {
            Some(Made::Tag(key, any.pop()?))
        }
        _ => None,
    })
}

/// Reads `input` as natural phrasing.
pub fn natural(input: &str) -> Result<Natural, NaturalError> {
    let (pieces, chips, text, read, strict) = build(input)?;
    // The pieces read together as the proposal; should they not (a rule
    // emptied a run between operators), the query stands as typed.
    match query::parse(&join(&pieces, None)) {
        Ok(ast) => Ok(Natural {
            canonical: ast.to_query(),
            ast,
            chips,
            text,
            read,
        }),
        Err(_) => Ok(Natural {
            canonical: strict.to_query(),
            ast: strict,
            chips: Vec::new(),
            text: Vec::new(),
            read: Vec::new(),
        }),
    }
}

/// A piece of the proposal: a run of plain words as the rules read it
/// (empty when they dropped every word), or syntax as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Run {
        /// The words as written.
        text: String,
        /// The rules' reading of them; empty when they dropped them all.
        read: String,
    },
    Syntax(String),
}

/// The proposal's pieces, in order, with each run's text replaced by
/// `placeholder(i)` when one is given: what the tests compare the
/// proposal's structure with.
#[doc(hidden)]
pub fn pieces(input: &str) -> Result<Vec<Piece>, NaturalError> {
    Ok(build(input)?.0)
}

/// The pieces as query text, each run `i` as `placeholder(i)` if given.
#[doc(hidden)]
pub fn join(pieces: &[Piece], placeholder: Option<&dyn Fn(usize) -> String>) -> String {
    let mut run = 0;
    let mut out: Vec<String> = Vec::new();
    for p in pieces {
        match p {
            Piece::Syntax(s) => out.push(s.clone()),
            Piece::Run { read, .. } => {
                match placeholder {
                    Some(f) => out.push(f(run)),
                    None if !read.is_empty() => out.push(read.clone()),
                    None => {}
                }
                run += 1;
            }
        }
    }
    out.join(" ")
}

/// The field a clause is on, for merging values and for leaving a
/// field the query already names alone.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Tag(TagKey),
    Ext,
    Num(NumField),
    Date,
    Flag(String),
}

fn key_of(node: &Node) -> Option<Key> {
    Some(match node {
        Node::Tag { key, .. } => Key::Tag(*key),
        Node::Ext { .. } => Key::Ext,
        Node::Range { field, .. } => Key::Num(*field),
        Node::Date { .. } => Key::Date,
        Node::Flag { name, .. } => Key::Flag(name.clone()),
        Node::Not { of } => return key_of(of),
        _ => return None,
    })
}

type Built = (
    Vec<Piece>,
    Vec<NaturalChip>,
    Vec<String>,
    Vec<(usize, usize)>,
    Node,
);

fn build(input: &str) -> Result<Built, NaturalError> {
    if input.len() > MAX_NATURAL_BYTES {
        return Err(NaturalError::TooLong);
    }
    let toks = caret::split(input);
    if toks.len() > MAX_NATURAL_WORDS {
        return Err(NaturalError::TooManyWords);
    }
    let strict = query::parse(input).map_err(|_| NaturalError::Syntax)?;
    // Segments in order: runs of plain words (with the top-level AND
    // group they're in, between `OR`s) and syntax as written. Each group
    // also notes the fields its own top-level syntax names.
    enum Seg {
        /// The words, their group, the text they span, and their merge
        /// scope (an explicit `AND` starts a new one).
        Run(Vec<W>, usize, String, usize),
        Syntax(String),
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut taken: Vec<Vec<Key>> = vec![Vec::new()];
    let mut scope = 0;
    let mut i = 0;
    while i < toks.len() {
        let group = taken.len() - 1;
        if plain(input, &toks, i) {
            let mut run = Vec::new();
            let start = toks[i].start;
            while i < toks.len() && plain(input, &toks, i) {
                run.extend(word(input, &toks[i]));
                i += 1;
            }
            let span = input[start..toks[i - 1].end].to_owned();
            segs.push(Seg::Run(run, group, span, scope));
            continue;
        }
        // Syntax, verbatim: up to the next plain word.
        let start = toks[i].start;
        while i < toks.len() && !plain(input, &toks, i) {
            let t = &toks[i];
            let text = &input[t.start..t.end];
            let top = t.depth == 0;
            if top && (t.kind == RawKind::Bar || (t.kind == RawKind::Word && text == "OR")) {
                taken.push(Vec::new());
                scope += 1;
            } else if top && t.kind == RawKind::Word && text == "AND" {
                // "All of" these: values either side aren't merged into
                // "any of".
                scope += 1;
            } else if t.kind == RawKind::Word {
                // A field named anywhere in the group, in brackets too
                // (`-(type:tv)`).
                if let Some(k) = query::parse(text).ok().as_ref().and_then(key_of) {
                    taken.last_mut().expect("a group").push(k);
                }
            }
            i += 1;
        }
        segs.push(Seg::Syntax(input[start..toks[i - 1].end].to_owned()));
    }
    // The rules, one AND group at a time, over all its runs together.
    let mut rendered: Vec<String> = vec![String::new(); segs.len()];
    let mut chips = Vec::new();
    let mut text = Vec::new();
    let mut read_words = Vec::new();
    for (g, keys) in taken.iter().enumerate() {
        let idx: Vec<usize> = (0..segs.len())
            .filter(|&j| matches!(&segs[j], Seg::Run(_, gr, ..) if *gr == g))
            .collect();
        let runs: Vec<(&[W], usize)> = idx
            .iter()
            .map(|&j| match &segs[j] {
                Seg::Run(w, _, _, scope) => (w.as_slice(), *scope),
                Seg::Syntax(_) => unreachable!("runs only"),
            })
            .collect();
        let read = read_group(&runs, keys);
        chips.extend(read.chips);
        text.extend(read.text);
        read_words.extend(read.read);
        for (j, nodes) in idx.into_iter().zip(read.nodes) {
            if !nodes.is_empty() {
                rendered[j] = and(nodes).to_query();
            }
        }
    }
    let pieces = segs
        .into_iter()
        .zip(rendered)
        .map(|(seg, r)| match seg {
            Seg::Run(_, _, text, _) => Piece::Run { text, read: r },
            Seg::Syntax(s) => Piece::Syntax(s),
        })
        .collect();
    Ok((pieces, chips, text, read_words, strict))
}

fn and(mut nodes: Vec<Node>) -> Node {
    match nodes.len() {
        0 => Node::All,
        1 => nodes.pop().expect("one"),
        _ => Node::And { of: nodes },
    }
}

/// What the rules made of the runs of one AND group.
struct Read {
    /// Each run's clauses and kept words, in order.
    nodes: Vec<Vec<Node>>,
    chips: Vec<NaturalChip>,
    text: Vec<String>,
    /// The words the rules read.
    read: Vec<(usize, usize)>,
}

/// Values of one field, merged across the group at the first one's
/// place.
struct Merged {
    key: Key,
    /// The runs it merges across: none past an explicit `AND`.
    scope: usize,
    values: Vec<String>,
    from: (usize, usize),
}

/// The rules over the runs of one AND group. A rule on a field the
/// group's own syntax already names (`-type:tv tv shows`) is left as
/// text; keyword values of one field merge across runs (`movies -x tv
/// shows` is `type:movie,tv -x`), but never across an explicit `AND`.
fn read_group(runs: &[(&[W], usize)], taken: &[Key]) -> Read {
    // Each word: recognised (a clause, or a slot in `merged`) or text.
    enum Part {
        Text(usize),
        Clause(Node, (usize, usize)),
        Merged(usize),
    }
    let mut merged: Vec<Merged> = Vec::new();
    let mut read = Vec::new();
    let mut all_parts: Vec<(Vec<Part>, Vec<bool>)> = Vec::new();
    for &(w, scope) in runs {
        let mut parts: Vec<Part> = Vec::new();
        let mut used = vec![false; w.len()];
        let mut i = 0;
        while i < w.len() {
            let Some((n, made)) = rule(&w[i..]) else {
                parts.push(Part::Text(i));
                i += 1;
                continue;
            };
            let key = match &made {
                Made::Node(node) => key_of(node),
                Made::Tag(k, _) => Some(Key::Tag(*k)),
                Made::Ext(_) => Some(Key::Ext),
            };
            if key.as_ref().is_some_and(|k| taken.contains(k)) {
                // The query names this field itself: these words stay
                // text rather than contradict or widen it.
                parts.extend((i..i + n).map(Part::Text));
                i += n;
                continue;
            }
            let from = (w[i].from.0, w[i + n - 1].from.1);
            used[i..i + n].iter_mut().for_each(|u| *u = true);
            read.extend(w[i..i + n].iter().map(|x| x.from));
            let value = match made {
                Made::Node(node) => {
                    parts.push(Part::Clause(node, from));
                    i += n;
                    continue;
                }
                Made::Tag(_, v) | Made::Ext(v) => v,
            };
            let key = key.expect("a keyword field");
            match merged.iter_mut().find(|m| m.key == key && m.scope == scope) {
                Some(m) => {
                    if !m.values.contains(&value) {
                        m.values.push(value);
                    }
                }
                None => {
                    merged.push(Merged {
                        key,
                        scope,
                        values: vec![value],
                        from,
                    });
                    parts.push(Part::Merged(merged.len() - 1));
                }
            }
            i += n;
        }
        all_parts.push((parts, used));
    }
    let mut out = Read {
        nodes: Vec::new(),
        chips: Vec::new(),
        text: Vec::new(),
        read,
    };
    for (&(w, _), (parts, used)) in runs.iter().zip(all_parts) {
        // Linking words beside a recognised word, and asks at the start,
        // go.
        let near_used = |j: usize| {
            let mut k = j;
            let before = loop {
                if k == 0 {
                    break false;
                }
                k -= 1;
                if used[k] {
                    break true;
                }
                if !LINKS.contains(&w[k].low.as_str()) {
                    break false;
                }
            };
            let after = (j + 1..w.len())
                .find(|&k| used[k] || !LINKS.contains(&w[k].low.as_str()))
                .is_some_and(|k| used[k]);
            after || (before && !ARTICLES.contains(&w[j].low.as_str()))
        };
        let any_used = used.iter().any(|u| *u);
        let mut leading = true;
        let mut nodes = Vec::new();
        for p in parts {
            let (node, from) = match p {
                Part::Text(j) => {
                    let low = w[j].low.as_str();
                    if leading && any_used && ASKS.contains(&low) {
                        continue;
                    }
                    leading = false;
                    if LINKS.contains(&low) && near_used(j) {
                        continue;
                    }
                    // Trimmed, or as written when trimming made it syntax
                    // (`NEAR,`): a plain token always stays.
                    let (kept, n) = match text_node(&w[j].orig) {
                        Some(n) => (&w[j].orig, n),
                        None => match text_node(&w[j].raw) {
                            Some(n) => (&w[j].raw, n),
                            None => continue,
                        },
                    };
                    out.text.push(kept.clone());
                    nodes.push(n);
                    continue;
                }
                Part::Clause(node, from) => (node, from),
                Part::Merged(m) => {
                    let m = &merged[m];
                    let node = match &m.key {
                        Key::Tag(key) => Node::Tag {
                            key: *key,
                            any: m.values.clone(),
                        },
                        _ => Node::Ext {
                            any: m.values.clone(),
                        },
                    };
                    (node, m.from)
                }
            };
            leading = false;
            out.chips.push(NaturalChip {
                text: node.to_query(),
                from,
            });
            nodes.push(node);
        }
        out.nodes.push(nodes);
    }
    out
}

/// A word kept as text, if the parser reads it as itself once trimmed
/// (it did before: it's from a plain token).
fn text_node(word: &str) -> Option<Node> {
    query::is_plain_word(word).then(|| Node::Word {
        field: TextField::Any,
        word: word.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_char_ranges() {
        let q = "  Amélie, [2001]  4k.";
        let toks = caret::split(q);
        let got: Vec<(String, (usize, usize))> = toks
            .iter()
            .filter_map(|t| word(q, t))
            .map(|w| (w.orig, w.from))
            .collect();
        assert_eq!(
            got,
            [
                ("Amélie".to_string(), (2, 8)),
                ("2001".to_string(), (11, 15)),
                ("4k".to_string(), (18, 20))
            ]
        );
    }

    #[test]
    fn syntax_is_not_plain() {
        let q = "a -b c:d res:1080p \"e\" OR (f) g NEAR h x* -\"i j\" k";
        let toks = caret::split(q);
        let plain: Vec<&str> = (0..toks.len())
            .filter(|&i| plain(q, &toks, i))
            .map(|i| &q[toks[i].start..toks[i].end])
            .collect();
        assert_eq!(plain, ["a", "c:d", "k"]);
    }

    #[test]
    fn numbers_sizes_and_years() {
        assert_eq!(number("1.5"), Some(1.5));
        assert_eq!(number(".5"), None);
        assert_eq!(number("1.2.3"), None);
        assert_eq!(decade("80s"), Some(1980));
        assert_eq!(decade("'90s"), Some(1990));
        assert_eq!(decade("2010s"), Some(2010));
        assert_eq!(decade("85s"), None);
        assert_eq!(decade("20s"), Some(2020));
        assert_eq!(year("1899"), None);
        assert_eq!(year_span("2010-2000"), None);
    }

    #[test]
    fn limits() {
        assert_eq!(natural(&"a ".repeat(40)), Err(NaturalError::TooManyWords));
        assert_eq!(natural("a (b"), Err(NaturalError::Syntax));
        assert_eq!(natural(&"x".repeat(600)), Err(NaturalError::TooLong));
        assert_eq!(natural("").unwrap().ast, Node::All);
    }
}
