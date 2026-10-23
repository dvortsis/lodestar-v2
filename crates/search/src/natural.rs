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
/// Most words read.
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
            NaturalError::TooManyWords => write!(f, "more than {MAX_NATURAL_WORDS} words"),
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
}

/// A word of the input: as written (outer punctuation trimmed) and
/// lowercased, with its character range.
#[derive(Debug, Clone)]
struct W {
    orig: String,
    low: String,
    from: (usize, usize),
}

/// The word a plain token is: outer punctuation trimmed for matching
/// (`4k,` reads as `4k`), with its character range in `input`.
fn word(input: &str, t: &Raw) -> Option<W> {
    let raw = &input[t.start..t.end];
    let trimmed = raw.trim_matches(|c: char| "[]{},.;:!?'".contains(c));
    if trimmed.is_empty() {
        return None;
    }
    let lead = raw.find(trimmed).unwrap_or(0);
    let from = caret::chars(input, t.start + lead);
    Some(W {
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
        if let (Some(lo), true, Some(hi)) = (year(b), matches!(c, "and" | "to" | "-"), year(d)) {
            if lo <= hi {
                return node(4, years(lo, hi));
            }
        }
        if let Some((lo, hi)) = year_span(b) {
            return node(2, years(lo, hi));
        }
        if a == "between" {
            if let Some((n, sizes)) = size_between(&w[1..]) {
                return node(1 + n, sizes);
            }
        }
    }
    if let (Some(lo), true, Some(hi)) = (year(a), matches!(b, "to" | "-"), year(c)) {
        if lo <= hi {
            return node(3, years(lo, hi));
        }
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
        if let (Some(k), Some(f)) = (rest.first().and_then(|x| count(&x.low)), rest.get(1)) {
            if matches!(f.low.as_str(), "files" | "file") {
                return node(n + 2, range(NumField::Files, cmp, k, true));
            }
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
        if words.contains(&a) {
            if let Some(x) = count(b).filter(|x| *x <= 5000) {
                if let (true, Some(y)) = (matches!(c, "to" | "-" | "through"), count(d)) {
                    if x <= y && y <= 5000 {
                        return num(field, 4, x, y);
                    }
                }
                return num(field, 2, x, x);
            }
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
    if input.len() > MAX_NATURAL_BYTES {
        return Err(NaturalError::TooLong);
    }
    let toks = caret::split(input);
    if toks.len() > MAX_NATURAL_WORDS {
        return Err(NaturalError::TooManyWords);
    }
    let strict = query::parse(input).map_err(|_| NaturalError::Syntax)?;
    let mut pieces: Vec<String> = Vec::new();
    let mut chips = Vec::new();
    let mut text = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if plain(input, &toks, i) {
            let mut run = Vec::new();
            while i < toks.len() && plain(input, &toks, i) {
                run.extend(word(input, &toks[i]));
                i += 1;
            }
            let read = read_run(&run);
            chips.extend(read.chips);
            text.extend(read.text);
            if !read.nodes.is_empty() {
                pieces.push(and(read.nodes).to_query());
            }
        } else {
            // Syntax, verbatim: up to the next plain word.
            let start = toks[i].start;
            while i < toks.len() && !plain(input, &toks, i) {
                i += 1;
            }
            pieces.push(input[start..toks[i - 1].end].to_owned());
        }
    }
    // The pieces read together as the proposal; should they not (a rule
    // emptied a run between operators), the query stands as typed.
    match query::parse(&pieces.join(" ")) {
        Ok(ast) => Ok(Natural {
            canonical: ast.to_query(),
            ast,
            chips,
            text,
        }),
        Err(_) => Ok(Natural {
            canonical: strict.to_query(),
            ast: strict,
            chips: Vec::new(),
            text: Vec::new(),
        }),
    }
}

fn and(mut nodes: Vec<Node>) -> Node {
    match nodes.len() {
        0 => Node::All,
        1 => nodes.pop().expect("one"),
        _ => Node::And { of: nodes },
    }
}

/// What the rules made of a run of plain words.
struct Read {
    nodes: Vec<Node>,
    chips: Vec<NaturalChip>,
    text: Vec<String>,
}

/// The rules over one run of plain words.
fn read_run(w: &[W]) -> Read {
    // Each word: recognised (a slot in `parts`) or text.
    enum Part {
        Text(usize),
        Clause(Node, (usize, usize)),
        /// Values of a field, merged at the first one's place.
        Merged(MergeKey, Vec<String>, (usize, usize)),
    }
    #[derive(Clone, PartialEq, Eq)]
    enum MergeKey {
        Tag(TagKey),
        Ext,
    }
    let mut parts: Vec<Part> = Vec::new();
    // Whether each word was used by a rule.
    let mut used = vec![false; w.len()];
    let mut i = 0;
    while i < w.len() {
        let Some((n, made)) = rule(&w[i..]) else {
            parts.push(Part::Text(i));
            i += 1;
            continue;
        };
        let from = (w[i].from.0, w[i + n - 1].from.1);
        used[i..i + n].iter_mut().for_each(|u| *u = true);
        let (key, value) = match made {
            Made::Node(node) => {
                parts.push(Part::Clause(node, from));
                i += n;
                continue;
            }
            Made::Tag(k, v) => (MergeKey::Tag(k), v),
            Made::Ext(e) => (MergeKey::Ext, e),
        };
        match parts
            .iter_mut()
            .find(|p| matches!(p, Part::Merged(k, ..) if *k == key))
        {
            Some(Part::Merged(_, values, _)) => {
                if !values.contains(&value) {
                    values.push(value);
                }
            }
            _ => parts.push(Part::Merged(key, vec![value], from)),
        }
        i += n;
    }
    // Linking words beside a recognised word, and asks at the start, go.
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
    let mut chips = Vec::new();
    let mut text = Vec::new();
    for p in parts {
        let node = match p {
            Part::Text(j) => {
                let low = w[j].low.as_str();
                if leading && any_used && ASKS.contains(&low) {
                    continue;
                }
                leading = false;
                if LINKS.contains(&low) && near_used(j) {
                    continue;
                }
                let Some(n) = text_node(&w[j].orig) else {
                    continue;
                };
                text.push(w[j].orig.clone());
                nodes.push(n);
                continue;
            }
            Part::Clause(node, from) => (node, from),
            Part::Merged(MergeKey::Tag(key), any, from) => (Node::Tag { key, any }, from),
            Part::Merged(MergeKey::Ext, any, from) => (Node::Ext { any }, from),
        };
        leading = false;
        chips.push(NaturalChip {
            text: node.0.to_query(),
            from: node.1,
        });
        nodes.push(node.0);
    }
    Read { nodes, chips, text }
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
