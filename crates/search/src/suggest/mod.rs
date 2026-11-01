//! Typeahead (§8, §10.4, B7): what to offer for the token being typed.
//!
//! - **Field names** for a partial word, from the query language's own
//!   field table.
//! - **Field values** after `field:`, from that field's term dictionary,
//!   with the number of torrents the request may see.
//! - **Titles** from the parsed-title FST ([`titles`]), by prefix.
//! - **Smart chips:** a bare token the release parser or the extension
//!   table recognises (`2160p`, `x265`, `mkv`, `2020`), as its field form.
//! - **Did you mean:** a near field name for an unknown one, and, when
//!   the whole query finds fewer than [`FEW`] torrents, fuzzy
//!   alternatives for its rare words from the term dictionaries.
//!
//! **Visibility.** Nothing is offered that only torrents the base query
//! hides carry (filter-hidden ones unless asked, Adult unless shown):
//! value and chip counts and every existence check run the request's
//! base query ([`Request::restrict`], the one `/search` runs), and
//! titles are counted per visibility class in the FST, then checked
//! against the index before they're offered, so a title hidden since
//! the last build is never offered. Quarantined (sealed) names are never
//! stored or indexed, so they can't be offered.
//!
//! Ranges are character offsets into the query, so the client never
//! tokenises it again.

pub(crate) mod caret;
mod fuzzy;
pub mod titles;

use std::sync::atomic::{AtomicUsize, Ordering};

use lodestar_classify::{Category, ContentType};
use tantivy::schema::Field;
use tantivy::{Searcher, Term};

use crate::cancel::CancelToken;
use crate::compile::ABSENT;
use crate::count::{self, Matcher};
use crate::index::{Compiled, Error, Result, STEM_FALLBACK_BELOW, SearchIndex};
use crate::query::{self, FieldKind, MAX_QUERY_BYTES, Node, NumField, TagKey, TextField};
use crate::tokenizer;
use caret::{Context, Raw, RawKind};
pub use titles::{Shown, Title, TitleSet, Titles, fold};

/// Items per response by default, and at most.
pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 20;
/// "Few hits": a query finding fewer torrents gets "did you mean" (the
/// same line as the stem fallback, §8).
pub const FEW: usize = STEM_FALLBACK_BELOW;
/// Words of a title being typed, at most.
const TITLE_WORDS: usize = 6;
/// Titles need this many folded characters typed.
const TITLE_MIN_CHARS: usize = 2;
/// Field names, chips and "did you mean" items offered, at most.
const MAX_FIELDS: usize = 4;
const MAX_CHIPS: usize = 2;
const MAX_DID_YOU_MEAN: usize = 3;
/// Values whose count is taken, at most, before giving up on filling
/// the list (values only hidden torrents carry count 0 and are dropped).
const MAX_COUNTED: usize = 48;
/// Terms read per segment for a value prefix or a fuzzy word.
const MAX_TERMS: usize = 4096;
/// Words "did you mean" looks for corrections of, nearest the caret.
const FUZZY_WORDS: usize = 2;
/// Fuzzy candidates checked against the index per word.
const FUZZY_CHECKED: usize = 4;
/// A title's live count is checked up to this many torrents per offered
/// title: a keystroke never walks a huge title's whole match list.
const LIVE_CAP: usize = 1000;
/// Spare titles looked up beyond the limit, for those the existence
/// check drops.
const TITLE_SPARE: usize = 4;

/// What an item offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    DidYouMean,
    Chip,
    Field,
    Value,
    Title,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Field => "field",
            Kind::Value => "value",
            Kind::Title => "title",
            Kind::Chip => "chip",
            Kind::DidYouMean => "did_you_mean",
        }
    }
}

/// One suggestion: put `insert` in place of the query's characters
/// `replace.0..replace.1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: Kind,
    /// What to show: the value, title or field, or for "did you mean"
    /// the whole corrected query.
    pub text: String,
    pub insert: String,
    /// Character offsets in the query, end exclusive.
    pub replace: (usize, usize),
    /// Torrents it would find, among those the request may see.
    pub count: Option<u64>,
}

/// A suggest request.
pub struct Request<'a> {
    pub q: &'a str,
    /// The caret, in characters; the end when `None` or past it.
    pub pos: Option<usize>,
    pub limit: usize,
    /// What the base query shows, for the title counts.
    pub shown: Shown,
    /// The base query of a clause (`/search`'s, with the hidden-torrent
    /// and Adult clauses): every count and existence check runs it.
    pub restrict: &'a (dyn Fn(Node) -> Node + Sync),
    /// Unix seconds, for relative dates in the query.
    pub now: i64,
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("q", &self.q)
            .field("pos", &self.pos)
            .field("limit", &self.limit)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

/// Everything one request reads: one snapshot of the index.
struct Cx<'a> {
    index: &'a SearchIndex,
    searcher: Searcher,
    req: &'a Request<'a>,
    cancel: &'a CancelToken,
}

/// The suggestions for `req`, at most `req.limit` (1–[`MAX_LIMIT`]):
/// "did you mean" first, then chips, field names, values and titles.
pub fn suggest(
    index: &SearchIndex,
    req: &Request<'_>,
    cancel: &CancelToken,
) -> Result<Vec<Suggestion>> {
    let q = req.q;
    let limit = req.limit.clamp(1, MAX_LIMIT);
    if q.len() > MAX_QUERY_BYTES || q.trim().is_empty() {
        return Ok(Vec::new());
    }
    let caret = req
        .pos
        .and_then(|p| caret::byte_at(q, p))
        .unwrap_or(q.len());
    let toks = caret::split(q);
    let at = caret::at(q, &toks, caret);
    let cx = Cx {
        index,
        searcher: index.searcher(),
        req,
        cancel,
    };
    let mut out = Vec::new();
    // The word being typed, when it starts a title: not a misspelling.
    let mut typing_title = None;
    let span = |r: (usize, usize)| (caret::chars(q, r.0), caret::chars(q, r.1));
    match &at.context {
        Context::None => {}
        Context::Word {
            replace,
            partial,
            word,
        } => {
            out.extend(chips(&cx, word, at.in_file, span(*replace))?);
            out.extend(field_names(
                partial,
                at.in_file,
                req.shown.adult,
                span(*replace),
            ));
            if !at.in_file
                && let Some(i) = at.token
            {
                for start in caret::title_runs(q, &toks, i, TITLE_WORDS) {
                    let found = titles(&cx, &q[start..caret], span((start, replace.1)), limit)?;
                    if !found.is_empty() {
                        out.extend(found);
                        typing_title = Some(i);
                        break;
                    }
                }
            }
        }
        Context::Field {
            replace,
            name,
            kind,
            partial,
            name_only,
        } => {
            if *name_only {
                let colon = replace.0 + name.len() + 1;
                out.extend(field_names(
                    partial,
                    at.in_file,
                    req.shown.adult,
                    span((replace.0, colon)),
                ));
            } else if *kind == FieldKind::Text(TextField::Title) && !at.in_file {
                out.extend(titles(&cx, partial, span(*replace), limit)?);
            } else {
                out.extend(values(
                    &cx,
                    name,
                    *kind,
                    partial,
                    at.in_file,
                    span(*replace),
                    limit,
                )?);
            }
        }
        Context::Phrase { replace, partial } => {
            if !at.in_file {
                out.extend(titles(&cx, partial, span(*replace), limit)?);
            }
        }
    }
    // The word the caret ends is still being typed: a prefix, not a
    // misspelling (a title it starts is offered instead).
    let typing = typing_title.or(at.token.filter(|&i| toks[i].end == caret));
    let mut dym = did_you_mean(&cx, &toks, caret, typing)?;
    dym.append(&mut out);
    let mut out = dym;
    // One item per insertion, the first kept.
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert((s.replace, s.insert.clone())));
    out.truncate(limit);
    Ok(out)
}

// ---- counting ---------------------------------------------------------------

/// Docs per step while counting up to a bound.
const CHUNK: u32 = 8192;

/// Matches of `compiled`, counted up to `n` (segments stop soon after
/// it's reached).
fn count_up_to(
    searcher: &Searcher,
    compiled: &Compiled,
    n: usize,
    cancel: &CancelToken,
) -> Result<usize> {
    let found = AtomicUsize::new(0);
    count::per_segment(searcher, compiled, cancel, |w, r| {
        let mut m = Matcher::new(w, r, compiled, cancel)?;
        let max = r.max_doc();
        let mut end = 0;
        while end < max && found.load(Ordering::Relaxed) < n {
            end = end.saturating_add(CHUNK).min(max);
            let got = m.visit_until(end, &mut |_| {})?;
            found.fetch_add(got, Ordering::Relaxed);
        }
        Ok(())
    })?;
    Ok(found.load(Ordering::Relaxed).min(n))
}

impl Cx<'_> {
    /// `node` as the base query runs it, compiled. Its `file:(…)`
    /// clauses run on the file index under the request's token.
    fn compiled(&self, node: Node) -> Result<Compiled> {
        self.index
            .query_cancelled(&(self.req.restrict)(node), self.req.now, 0, self.cancel)
    }

    /// Torrents the request may see that `node` matches.
    fn visible(&self, node: Node) -> Result<u64> {
        let c = self.compiled(node)?;
        Ok(count::exact(&self.searcher, &c, self.cancel)? as u64)
    }

    /// Whether the request may see any torrent `node` matches.
    fn exists(&self, node: Node) -> Result<bool> {
        Ok(self.count_up_to(node, 1)? > 0)
    }

    /// Torrents the request may see that `node` matches, up to `n`.
    fn count_up_to(&self, node: Node, n: usize) -> Result<usize> {
        let c = self.compiled(node)?;
        count_up_to(&self.searcher, &c, n, self.cancel)
    }

    /// `(term text, doc freq)` for terms of `field` starting with
    /// `prefix`, summed over segments (deleted documents included: it's
    /// only for ordering candidates).
    fn prefixed(&self, field: Field, prefix: &str) -> Result<Vec<(String, u64)>> {
        let mut by: std::collections::HashMap<String, u64> = Default::default();
        if field == ABSENT {
            return Ok(Vec::new());
        }
        for seg in self.searcher.segment_readers() {
            self.cancel.check()?;
            let inv = seg.inverted_index(field)?;
            let mut s = inv
                .terms()
                .range()
                .ge(prefix.as_bytes())
                .into_stream()
                .map_err(Error::Io)?;
            let mut read = 0;
            while let Some((k, info)) = s.next() {
                if !k.starts_with(prefix.as_bytes()) || read == MAX_TERMS {
                    break;
                }
                read += 1;
                if let Ok(t) = std::str::from_utf8(k) {
                    *by.entry(t.to_owned()).or_default() += u64::from(info.doc_freq);
                }
            }
        }
        Ok(by.into_iter().collect())
    }
}

// ---- field names ------------------------------------------------------------

/// Fields a `file:(…)` group takes (`files::file_clause_ok`).
fn file_field(kind: FieldKind) -> bool {
    matches!(
        kind,
        FieldKind::Text(TextField::Name | TextField::Path)
            | FieldKind::Ext
            | FieldKind::Num(NumField::Size)
    )
}

/// Whether `node` has an Adult clause (`adult:…`, `type:adult`), either
/// way round: while Adult is hidden nothing offers one (§20.9).
fn names_adult(node: &Node) -> bool {
    match node {
        Node::Tag {
            key: TagKey::Adult, ..
        } => true,
        Node::Tag {
            key: TagKey::Type,
            any,
        } => any.iter().any(|t| t == ContentType::Adult.as_str()),
        Node::And { of } | Node::Or { of } => of.iter().any(names_adult),
        Node::Not { of } => names_adult(of),
        _ => false,
    }
}

/// The field names `partial` starts; `adult:` only while Adult is shown.
fn field_names(
    partial: &str,
    in_file: bool,
    adult: bool,
    replace: (usize, usize),
) -> Vec<Suggestion> {
    let p = partial.to_ascii_lowercase();
    if p.is_empty() || !query::is_field_name(&p) {
        return Vec::new();
    }
    let mut found: Vec<(usize, &str)> = query::FIELDS
        .iter()
        .enumerate()
        .filter(|(_, (name, kind))| {
            name.starts_with(&p)
                && !matches!(kind, FieldKind::Later(_))
                && (adult || !matches!(kind, FieldKind::Tag(TagKey::Adult)))
                && (!in_file || file_field(*kind))
        })
        .map(|(i, (name, _))| (i, *name))
        .collect();
    // The exact name first, then shorter names, then the table's order.
    found.sort_by_key(|(i, name)| (*name != p, name.len(), *i));
    found
        .into_iter()
        .take(MAX_FIELDS)
        .map(|(_, name)| Suggestion {
            kind: Kind::Field,
            text: format!("{name}:"),
            insert: format!("{name}:"),
            replace,
            count: None,
        })
        .collect()
}

// ---- values -----------------------------------------------------------------

/// Whether `value` can be written after `field:{head}` and reads back
/// as itself (the last of the list, after a head).
fn writable(name: &str, head: &str, value: &str, alone: &Node) -> bool {
    if value.is_empty() || value.contains(|c: char| c.is_whitespace() || "()\"|,".contains(c)) {
        return false;
    }
    match query::parse(&format!("{name}:{head}{value}")) {
        Ok(Node::Ext { any } | Node::Tag { any, .. }) if !head.is_empty() => {
            any.last().is_some_and(|v| v == value)
        }
        Ok(n) => n == *alone,
        Err(_) => false,
    }
}

fn values(
    cx: &Cx<'_>,
    name: &str,
    kind: FieldKind,
    partial: &str,
    in_file: bool,
    replace: (usize, usize),
    limit: usize,
) -> Result<Vec<Suggestion>> {
    // `ext:mkv,mp`: complete the last of the list.
    let (head, last) = match partial.rfind(',') {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let last = last.to_lowercase();
    let item = |value: &str, count: Option<u64>| Suggestion {
        kind: Kind::Value,
        text: value.to_owned(),
        insert: format!("{name}:{head}{value}"),
        replace,
        count,
    };
    let f = cx.index.fields();
    let (field, prefix, of) = match kind {
        FieldKind::Tag(key) if !in_file => (f.tag, format!("{}:{last}", key.as_str()), Some(key)),
        FieldKind::Ext => (f.ext, last.clone(), None),
        FieldKind::Flag(flag) if !in_file => {
            let mut out = Vec::new();
            for (word, value) in [("yes", true), ("no", false)] {
                if word.starts_with(&last) {
                    let n = cx.visible(Node::Flag {
                        name: flag.to_owned(),
                        value,
                    })?;
                    out.push(item(word, Some(n)));
                }
            }
            return Ok(out);
        }
        FieldKind::Share(_) if !in_file => {
            // A fixed list: nothing about the index to leak, and no count
            // (each would scan a column).
            return Ok(Category::ALL
                .iter()
                .map(|c| c.as_str())
                .filter(|c| c.starts_with(&last))
                .take(limit)
                .map(|c| item(c, None))
                .collect());
        }
        _ => return Ok(Vec::new()),
    };
    // A tag value (`res:1080p`), or an extension.
    let node = |v: &str| match of {
        Some(key) => Node::Tag {
            key,
            any: vec![v.to_owned()],
        },
        None => Node::Ext {
            any: vec![v.to_owned()],
        },
    };
    let strip = prefix.len() - last.len();
    let mut terms = cx.prefixed(field, &prefix)?;
    terms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out: Vec<(u64, String)> = Vec::new();
    for (term, _) in terms.into_iter().take(MAX_COUNTED) {
        if out.len() >= limit {
            break;
        }
        let value = &term[strip..];
        if !writable(name, head, value, &node(value)) {
            continue;
        }
        let n = cx.visible(node(value))?;
        if n > 0 {
            out.push((n, value.to_owned()));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(out.into_iter().map(|(n, v)| item(&v, Some(n))).collect())
}

// ---- titles -----------------------------------------------------------------

/// The titles starting with `typed`, as `title:"…"` phrases.
fn titles(
    cx: &Cx<'_>,
    typed: &str,
    replace: (usize, usize),
    limit: usize,
) -> Result<Vec<Suggestion>> {
    let prefix = fold(typed, true);
    if prefix.trim_end().chars().count() < TITLE_MIN_CHARS {
        return Ok(Vec::new());
    }
    let Some(set) = cx.index.titles().current() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for t in set.lookup(&prefix, cx.req.shown, limit + TITLE_SPARE) {
        if out.len() >= limit {
            break;
        }
        cx.cancel.check()?;
        let text = t
            .display
            .replace('"', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            continue;
        }
        let insert = format!("title:\"{text}\"");
        let node = Node::Phrase {
            field: TextField::Title,
            text: text.clone(),
        };
        if query::parse(&insert).ok().as_ref() != Some(&node) {
            continue;
        }
        // The FST may be a build behind: a title hidden or purged since
        // isn't offered, and its count is never more than the torrents
        // the request may see now. That's counted up to the FST's count
        // or [`LIVE_CAP`]: past the cap the FST's count stands.
        let cap = (t.count as usize).min(LIVE_CAP);
        let live = cx.count_up_to(node, cap)?;
        if live == 0 {
            continue;
        }
        let count = if live < cap {
            live as u64
        } else {
            u64::from(t.count)
        };
        out.push(Suggestion {
            kind: Kind::Title,
            text: t.display,
            insert,
            replace,
            count: Some(count),
        });
    }
    Ok(out)
}

// ---- smart chips ------------------------------------------------------------

/// The field forms of a bare token (`2160p` → `res:2160p`), from the
/// release parser's recognisers (read as the query parser reads a tag
/// value), the extension table and the content types.
pub fn chip_nodes(word: &str) -> Vec<Node> {
    let lower = word.to_lowercase();
    let mut out = Vec::new();
    if lower.is_empty() || lower.len() > 32 || lower.contains(':') {
        return out;
    }
    if lower.len() == 4 && lower.bytes().all(|b| b.is_ascii_digit()) {
        let y: u64 = lower.parse().unwrap_or(0);
        if (1900..=2099).contains(&y) {
            out.push(Node::Range {
                field: NumField::Year,
                lo: Some(y),
                hi: Some(y),
            });
        }
    }
    if let Some(node) = release_chip(word, &lower) {
        out.push(node);
    }
    // Extensions, with a letter in them: `300`, `007`, `720` are numbers
    // (and split-archive parts), not extensions anyone types.
    if lower.len() <= 10
        && lower.bytes().all(|b| b.is_ascii_alphanumeric())
        && lower.bytes().any(|b| b.is_ascii_alphabetic())
        && lodestar_classify::ext::category_of(&lower) != Category::Other
    {
        out.push(Node::Ext {
            any: vec![lower.clone()],
        });
    }
    if let Some(t) = ContentType::from_name(&lower)
        && !matches!(t, ContentType::Other | ContentType::Adult)
    {
        out.push(Node::Tag {
            key: TagKey::Type,
            any: vec![t.as_str().to_owned()],
        });
    }
    // Only forms the parser reads back as themselves.
    out.retain(|n| query::parse(&n.to_query()).ok().as_ref() == Some(n));
    out
}

/// The keys a release-parser chip may be, as the parser names them.
const CHIP_KEYS: [(TagKey, &str); 5] = [
    (TagKey::Res, "res"),
    (TagKey::Codec, "vcodec"),
    (TagKey::Hdr, "hdr"),
    (TagKey::Audio, "acodec"),
    (TagKey::Lang, "lang"),
];

/// `word` as one keyword value, when it's that value and nothing else:
/// read as the release parser reads `A [word]`, the title must stay `A`
/// and the only field it sets must be one of [`CHIP_KEYS`] (with the
/// channels an audio codec carries, `dd5.1`), with one value. So a token
/// with more in it (`Naruto.S01E02.1080p`, `x265,naruto`, `ita.eng`,
/// `x264.x265`, `naruto\u{3000}1080p`) is no chip: it would lose the rest
/// of its text.
fn release_chip(word: &str, lower: &str) -> Option<Node> {
    if word.chars().any(|c| c.is_whitespace() && !c.is_ascii()) {
        return None;
    }
    // Two values joined (`x264.x265`, `flac-mp3`) aren't one: each piece
    // would chip on its own. `h.264`, `dts-hd.ma` and `dd5.1` have one
    // such piece at most.
    let pieces: Vec<&str> = word
        .split(['.', '-', '_', '+'])
        .filter(|p| !p.is_empty())
        .collect();
    if pieces.len() > 1
        && pieces
            .iter()
            .filter(|p| release_chip(p, &p.to_lowercase()).is_some())
            .count()
            > 1
    {
        return None;
    }
    let parsed = lodestar_release_parser::parse(&format!("A [{word}]"));
    if parsed.title != "A" {
        return None;
    }
    let fields = parsed.fields();
    let mut found = None;
    for (k, v) in fields.iter().filter(|(k, _)| *k != "title") {
        // A list (`ita.eng` is `en, it`) is more than one value.
        if v.contains(", ") {
            return None;
        }
        match CHIP_KEYS.iter().find(|(_, rk)| rk == k) {
            Some((key, _)) if found.is_none() => found = Some(*key),
            None if *k == "channels" => {}
            _ => return None,
        }
    }
    let key = found?;
    if fields.iter().any(|(k, _)| *k == "channels") && key != TagKey::Audio {
        return None;
    }
    // Two-letter language codes are mostly ordinary words (`it`).
    if key == TagKey::Lang && lower.chars().count() < 3 {
        return None;
    }
    query::tag_value(key, word).map(|v| Node::Tag { key, any: vec![v] })
}

fn chips(
    cx: &Cx<'_>,
    word: &str,
    in_file: bool,
    replace: (usize, usize),
) -> Result<Vec<Suggestion>> {
    let mut out = Vec::new();
    for node in chip_nodes(word).into_iter().take(MAX_CHIPS) {
        if in_file && !matches!(node, Node::Ext { .. }) {
            continue;
        }
        let insert = node.to_query();
        let count = if in_file {
            None
        } else {
            Some(cx.visible(node)?)
        };
        out.push(Suggestion {
            kind: Kind::Chip,
            text: insert.clone(),
            insert,
            replace,
            count,
        });
    }
    Ok(out)
}

// ---- did you mean -----------------------------------------------------------

/// "Did you mean" items; the token `typing` (the word being typed) isn't
/// corrected.
fn did_you_mean(
    cx: &Cx<'_>,
    toks: &[Raw],
    caret: usize,
    typing: Option<usize>,
) -> Result<Vec<Suggestion>> {
    let q = cx.req.q;
    let span = |a: usize, b: usize| (caret::chars(q, a), caret::chars(q, b));
    let mut out = Vec::new();
    // An unknown field one edit from a known one (the parser's rule).
    for t in toks.iter().filter(|t| t.kind == RawKind::Word) {
        let text = &q[t.start..t.end];
        let start = t.start + caret::negation(text);
        let body = &q[start..t.end];
        let Some((name, value)) = body.split_once(':') else {
            continue;
        };
        if !query::is_field_name(name) || query::field(name).is_some() {
            continue;
        }
        if let Some(near) = query::near_field(name) {
            let insert = format!("{near}:{value}");
            // `adul:x` → `adult:x`, `typ:adult` → `type:adult`: an
            // Adult clause, not offered while Adult is hidden.
            if !cx.req.shown.adult
                && (query::field(near) == Some(FieldKind::Tag(TagKey::Adult))
                    || query::parse(&insert).is_ok_and(|n| names_adult(&n)))
            {
                continue;
            }
            out.push(Suggestion {
                kind: Kind::DidYouMean,
                text: format!("{}{insert}{}", &q[..start], &q[t.end..]),
                insert,
                replace: span(start, t.end),
                count: None,
            });
        }
    }
    if !out.is_empty() {
        out.truncate(MAX_DID_YOU_MEAN);
        return Ok(out);
    }
    let Ok(ast) = query::parse(q) else {
        return Ok(out);
    };
    if ast == Node::All || count_up_to(&cx.searcher, &cx.compiled(ast)?, FEW, cx.cancel)? >= FEW {
        return Ok(out);
    }
    let text = cx.index.fields().text;
    // The words nearest the caret, at most [`FUZZY_WORDS`]: each costs a
    // fuzzy walk of every segment's dictionary.
    let mut words: Vec<&Raw> = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            t.kind == RawKind::Word
                && Some(*i) != typing
                && caret::is_title_word(&q[t.start..t.end])
        })
        .map(|(_, t)| t)
        .collect();
    words.sort_by_key(|t| t.start.abs_diff(caret).min(t.end.abs_diff(caret)));
    words.truncate(FUZZY_WORDS);
    words.sort_by_key(|t| t.start);
    for t in words {
        if out.len() >= MAX_DID_YOU_MEAN {
            break;
        }
        let w = &q[t.start..t.end];
        let toks = tokenizer::tokenize(w);
        let [tok] = toks.as_slice() else {
            continue;
        };
        let word = tok.text.as_str();
        if !word.bytes().all(|b| b.is_ascii_lowercase()) {
            continue;
        }
        let Some(d) = fuzzy::allowed(word.len()) else {
            continue;
        };
        if let Some(better) = correction(cx, text, word, d)? {
            out.push(Suggestion {
                kind: Kind::DidYouMean,
                text: format!("{}{better}{}", &q[..t.start], &q[t.end..]),
                insert: better,
                replace: span(t.start, t.end),
                count: None,
            });
        }
    }
    Ok(out)
}

/// The best visible term of `field` within `d` edits of a rare `word`:
/// fewest edits, then most documents. `None` when `word` isn't rare.
fn correction(cx: &Cx<'_>, field: Field, word: &str, d: u8) -> Result<Option<String>> {
    let mut own = 0u64;
    let mut by: std::collections::HashMap<String, u64> = Default::default();
    for seg in cx.searcher.segment_readers() {
        cx.cancel.check()?;
        let inv = seg.inverted_index(field)?;
        own += u64::from(
            inv.doc_freq(&Term::from_field_text(field, word))
                .map_err(Error::Io)?,
        );
        let mut s = inv
            .terms()
            .search(fuzzy::Lev::new(word, d))
            .into_stream()
            .map_err(Error::Io)?;
        let mut read = 0;
        while let Some((k, info)) = s.next() {
            if read == MAX_TERMS {
                break;
            }
            read += 1;
            if let Ok(t) = std::str::from_utf8(k)
                && t != word
            {
                *by.entry(t.to_owned()).or_default() += u64::from(info.doc_freq);
            }
        }
    }
    if own >= FEW as u64 {
        return Ok(None);
    }
    let mut found: Vec<(usize, u64, String)> = by
        .into_iter()
        .filter(|(_, n)| *n > own)
        .map(|(t, n)| (query::edit_distance(word, &t), n, t))
        .collect();
    found.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    for (_, _, t) in found.into_iter().take(FUZZY_CHECKED) {
        let node = Node::Word {
            field: TextField::Any,
            word: t.clone(),
        };
        if query::is_plain_word(&t) && cx.exists(node)? {
            return Ok(Some(t));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FILE_QUERY, append_files, index_with, tripwire};

    /// A suggest count's `file:(…)` clause scans the file index under the
    /// request's token (X-search-files-perf): cancelled partway, the
    /// request fails with `Cancelled` and no set is cached.
    #[test]
    fn a_file_scan_cancelled_partway_stops_suggest_and_caches_nothing() {
        let (index, _) = index_with(10, 1);
        let per = u64::from(crate::cancel::CHECK_EVERY);
        append_files(&index, 1..11, per);
        let cancel = CancelToken::new();
        let (trip, visits) = tripwire(100, &cancel);
        FILE_QUERY.with(|q| *q.borrow_mut() = Some(trip.query));
        let restrict = |n: Node| n;
        let req = Request {
            q: "file:(size:>0) knigth",
            pos: None,
            limit: 8,
            shown: Shown {
                adult: true,
                hidden: true,
            },
            restrict: &restrict,
            now: 0,
        };
        assert!(matches!(
            suggest(&index, &req, &cancel),
            Err(Error::Cancelled)
        ));
        let seen = visits.load(std::sync::atomic::Ordering::Relaxed);
        assert!((100..=per as usize).contains(&seen), "{seen} files visited");
        assert_eq!(index.cache_usage().file_set_entries, 0);
        FILE_QUERY.with(|q| q.borrow_mut().take());
    }

    fn field_texts(adult: bool, partial: &str) -> Vec<String> {
        field_names(partial, false, adult, (0, 0))
            .into_iter()
            .map(|s| s.text)
            .collect()
    }

    /// `ad` offers `adult:` only while Adult is shown (§20.9).
    #[test]
    fn the_adult_field_is_offered_only_while_adult_is_shown() {
        assert!(!field_texts(false, "ad").contains(&"adult:".to_owned()));
        assert!(field_texts(false, "adult").is_empty());
        assert!(field_texts(true, "ad").contains(&"adult:".to_owned()));
    }

    fn did_you_mean_for(q: &str, adult: bool) -> Vec<String> {
        let (index, _) = index_with(1, 1);
        let restrict = |n: Node| n;
        let req = Request {
            q,
            pos: None,
            limit: 8,
            shown: Shown {
                adult,
                hidden: false,
            },
            restrict: &restrict,
            now: 0,
        };
        suggest(&index, &req, &CancelToken::new())
            .unwrap()
            .into_iter()
            .filter(|s| s.kind == Kind::DidYouMean)
            .map(|s| s.insert)
            .collect()
    }

    /// A near field name that would write an Adult clause isn't offered
    /// while Adult is hidden; other corrections are.
    #[test]
    fn did_you_mean_writes_no_adult_clause_while_adult_is_hidden() {
        for (q, fixed) in [("adul:vr", "adult:vr"), ("typ:adult", "type:adult")] {
            assert!(did_you_mean_for(q, false).is_empty(), "{q}");
            assert_eq!(did_you_mean_for(q, true), [fixed], "{q}");
        }
        assert_eq!(did_you_mean_for("typ:movie", false), ["type:movie"]);
    }
}
