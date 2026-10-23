//! The query language (architecture §8): a hand-written parser from text
//! to an AST, and back. The AST is what the API takes and the UI builder
//! edits (as JSON), so text ↔ AST round-trips losslessly.
//!
//! ```text
//! dark knight          all words        "dark knight"      phrase
//! dark NEAR/5 knight   proximity        batman | joker     either (also OR)
//! (a | b) c            grouping         -cam  !cam         not
//! comput*              prefix           ~knigth            fuzzy (1–2 edits)
//! name:  path:  title:  field scope      ext:mkv,mp4        any of
//! size:>4GB  size:700MB..1.5GB  files:>=10  largest:>2GB  piece:16MB
//! discovered:<7d  discovered:2026-01-01..2026-03-31  age:<6h
//! private:no  v2:yes  hash:<40 hex>  (or a bare hash or magnet link)
//! type:movie,tv  res:4k  hdr:dv  codec:x265  audio:atmos  lang:fr
//! group:ntb  has:nfo  resolved:files  ambiguous:yes
//! year:2019..2021  s:2  e:5  junk:<20  cat:video>=80%  bytes:video>90%
//! file:(ext:flac size:>50MB)  file:"dark knight"   one file matching all
//! ```
//!
//! Keyword values are normalised as the release parser reads them
//! (`codec:x265` is `codec:h265`, `res:4k` is `res:2160p`), so the AST
//! holds what the index holds. Fields that need later milestones
//! (`seeders:`, `regex`, …) parse to a clear "not available yet" error
//! rather than being ignored.
//! Input is untrusted: length, clause count and nesting are bounded.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

/// Longest query accepted, in bytes.
pub const MAX_QUERY_BYTES: usize = 2048;
/// Most leaf clauses in one query.
pub const MAX_CLAUSES: usize = 64;
/// Deepest parenthesis nesting.
pub const MAX_DEPTH: usize = 16;
const MAX_NEAR: u32 = 50;
/// Most words in one NEAR chain.
pub const MAX_NEAR_WORDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextField {
    /// Name and file paths.
    Any,
    Name,
    Path,
    /// The effective title and alternative titles (§6).
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumField {
    Size,
    Files,
    Largest,
    Piece,
    /// Release year (any year of a span).
    Year,
    /// Season number (any season of a pack).
    Season,
    /// Episode number (any episode of a range, absolute numbers too).
    Episode,
    /// Junk score, 0–100.
    Junk,
}

impl NumField {
    /// Whether the field is one value per torrent in a fast column, so a
    /// range on it can be checked in the collector. Years, seasons and
    /// episodes are multi-valued and live in the inverted index.
    pub(crate) fn is_fast(self) -> bool {
        !matches!(self, NumField::Year | NumField::Season | NumField::Episode)
    }

    /// Counted in plain numbers rather than bytes.
    fn is_count(self) -> bool {
        !matches!(self, NumField::Size | NumField::Largest | NumField::Piece)
    }
}

/// Keyword facets from enrichment (§6), one term each in the `tag`
/// field as `key:value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagKey {
    Type,
    Res,
    Hdr,
    Codec,
    Audio,
    Lang,
    Group,
    Has,
    Resolved,
    /// What an adult torrent would otherwise be (§20.9).
    Adult,
}

impl TagKey {
    pub fn as_str(self) -> &'static str {
        match self {
            TagKey::Type => "type",
            TagKey::Res => "res",
            TagKey::Hdr => "hdr",
            TagKey::Codec => "codec",
            TagKey::Audio => "audio",
            TagKey::Lang => "lang",
            TagKey::Group => "group",
            TagKey::Has => "has",
            TagKey::Resolved => "resolved",
            TagKey::Adult => "adult",
        }
    }
}

/// How a `cat:`/`bytes:` share is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareBy {
    /// Share of the files.
    Count,
    /// Share of the bytes.
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateField {
    Discovered,
}

/// One end of a date range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Seconds before the time of the search (saved searches stay
    /// relative).
    Ago(i64),
    /// A unix time.
    At(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Node {
    /// Matches everything (an empty query: browse).
    All,
    And {
        of: Vec<Node>,
    },
    Or {
        of: Vec<Node>,
    },
    Not {
        of: Box<Node>,
    },
    /// A word, tokenized when compiled.
    Word {
        field: TextField,
        word: String,
    },
    Phrase {
        field: TextField,
        text: String,
    },
    Near {
        field: TextField,
        words: Vec<String>,
        slop: u32,
    },
    Prefix {
        field: TextField,
        prefix: String,
    },
    Fuzzy {
        field: TextField,
        word: String,
        distance: u8,
    },
    /// Inclusive bounds.
    Range {
        field: NumField,
        lo: Option<u64>,
        hi: Option<u64>,
    },
    /// Inclusive bounds.
    Date {
        field: DateField,
        from: Option<When>,
        to: Option<When>,
    },
    /// File extensions; matches any.
    Ext {
        any: Vec<String>,
    },
    Flag {
        name: String,
        value: bool,
    },
    /// 40 lowercase hex digits.
    Hash {
        hash: String,
    },
    /// A keyword facet; matches any of the values.
    Tag {
        key: TagKey,
        any: Vec<String>,
    },
    /// Torrents with at least one file matching every clause of `of`
    /// (words, phrases, `name:`, `path:`, `ext:`, `size:`), answered by
    /// the file index.
    File {
        of: Box<Node>,
    },
    /// A category's share of the files or bytes, inclusive percentages.
    Share {
        by: ShareBy,
        /// A category name (`video`, `software`, …).
        cat: String,
        lo: u8,
        hi: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseError {
    pub message: String,
    /// Byte range in the query text.
    pub start: usize,
    pub end: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at {}..{})", self.message, self.start, self.end)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(message: impl Into<String>, start: usize, end: usize) -> Result<T, ParseError> {
    Err(ParseError {
        message: message.into(),
        start,
        end,
    })
}

// ---- fields ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text(TextField),
    Num(NumField),
    Date(DateField),
    Ext,
    Flag(&'static str),
    Hash,
    Magnet,
    Tag(TagKey),
    Share(ShareBy),
    File,
    /// Known, but needs a later milestone.
    Later(&'static str),
}

pub(crate) const FIELDS: &[(&str, FieldKind)] = &[
    ("name", FieldKind::Text(TextField::Name)),
    ("path", FieldKind::Text(TextField::Path)),
    ("size", FieldKind::Num(NumField::Size)),
    ("files", FieldKind::Num(NumField::Files)),
    ("largest", FieldKind::Num(NumField::Largest)),
    ("piece", FieldKind::Num(NumField::Piece)),
    ("discovered", FieldKind::Date(DateField::Discovered)),
    ("age", FieldKind::Date(DateField::Discovered)),
    ("ext", FieldKind::Ext),
    ("private", FieldKind::Flag("private")),
    ("v2", FieldKind::Flag("v2")),
    ("hybrid", FieldKind::Flag("hybrid")),
    ("imported", FieldKind::Flag("imported")),
    ("backfill", FieldKind::Flag("backfill")),
    ("partial", FieldKind::Flag("files_partial")),
    ("hash", FieldKind::Hash),
    ("magnet", FieldKind::Magnet),
    ("title", FieldKind::Text(TextField::Title)),
    ("type", FieldKind::Tag(TagKey::Type)),
    ("res", FieldKind::Tag(TagKey::Res)),
    ("hdr", FieldKind::Tag(TagKey::Hdr)),
    ("codec", FieldKind::Tag(TagKey::Codec)),
    ("audio", FieldKind::Tag(TagKey::Audio)),
    ("lang", FieldKind::Tag(TagKey::Lang)),
    ("group", FieldKind::Tag(TagKey::Group)),
    ("has", FieldKind::Tag(TagKey::Has)),
    ("resolved", FieldKind::Tag(TagKey::Resolved)),
    ("adult", FieldKind::Tag(TagKey::Adult)),
    ("ambiguous", FieldKind::Flag("ambiguous")),
    // Content filtering (§20.3).
    ("hidden", FieldKind::Flag("hidden")),
    ("demoted", FieldKind::Flag("demoted")),
    ("flagged", FieldKind::Flag("flagged")),
    ("year", FieldKind::Num(NumField::Year)),
    ("s", FieldKind::Num(NumField::Season)),
    ("e", FieldKind::Num(NumField::Episode)),
    ("junk", FieldKind::Num(NumField::Junk)),
    ("cat", FieldKind::Share(ShareBy::Count)),
    ("bytes", FieldKind::Share(ShareBy::Bytes)),
    ("file", FieldKind::File),
    ("sig", FieldKind::Later("grouping keys (M7)")),
    ("seeders", FieldKind::Later("swarm health (M4)")),
    ("trackers", FieldKind::Later("swarm health (M4)")),
    ("dht", FieldKind::Later("swarm health (M4)")),
    ("seen", FieldKind::Later("swarm health (M4)")),
    ("time", FieldKind::Later("swarm health (M4)")),
];

pub(crate) fn field(name: &str) -> Option<FieldKind> {
    FIELDS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// A known field one typo away, for "did you mean".
pub(crate) fn near_field(name: &str) -> Option<&'static str> {
    if name.len() < 3 {
        return None;
    }
    FIELDS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| n.len() >= 3)
        .find(|n| edit_distance(name, n) <= 1)
}

// ---- lexer -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lex {
    LParen,
    RParen,
    Or,
    And,
    Near(u32),
    Not,
    Quoted(String),
    /// `name:` followed by its raw value (possibly empty).
    Field(String, String),
    Word(String),
}

#[derive(Debug, Clone)]
struct Spanned {
    lex: Lex,
    start: usize,
    end: usize,
}

pub(crate) fn is_field_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn lex(q: &str) -> Result<Vec<Spanned>, ParseError> {
    let b = q.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<Spanned>, lex, start, end| out.push(Spanned { lex, start, end });
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        match c {
            b'(' => {
                push(&mut out, Lex::LParen, i, i + 1);
                i += 1;
            }
            b')' => {
                push(&mut out, Lex::RParen, i, i + 1);
                i += 1;
            }
            b'|' => {
                push(&mut out, Lex::Or, i, i + 1);
                i += 1;
            }
            b'"' => {
                let close = q[i + 1..].find('"').map(|p| i + 1 + p);
                let Some(close) = close else {
                    return err("unclosed quote", i, q.len());
                };
                push(
                    &mut out,
                    Lex::Quoted(q[i + 1..close].to_string()),
                    i,
                    close + 1,
                );
                i = close + 1;
            }
            // Negation only when a term follows; `-` alone or before `)`
            // is just a word.
            b'-' | b'!'
                if b.get(i + 1)
                    .is_some_and(|n| !n.is_ascii_whitespace() && !matches!(n, b')' | b'|')) =>
            {
                push(&mut out, Lex::Not, i, i + 1);
                i += 1;
            }
            _ => {
                // A word runs to whitespace, a parenthesis, a quote or `|`.
                let mut end = i;
                while end < b.len()
                    && !b[end].is_ascii_whitespace()
                    && !matches!(b[end], b'(' | b')' | b'"' | b'|')
                {
                    end += 1;
                }
                let word = &q[i..end];
                let lex = match word {
                    "OR" => Lex::Or,
                    "AND" => Lex::And,
                    "NEAR" => Lex::Near(5),
                    w if w.starts_with("NEAR/") => {
                        let n: u32 = w[5..]
                            .parse()
                            .ok()
                            .filter(|n| (1..=MAX_NEAR).contains(n))
                            .map_or_else(
                                || err(format!("NEAR distance must be 1–{MAX_NEAR}"), start, end),
                                Ok,
                            )?;
                        Lex::Near(n)
                    }
                    w => match w.split_once(':') {
                        Some((name, value)) if is_field_name(name) => {
                            if field(name).is_some() {
                                Lex::Field(name.to_string(), value.to_string())
                            } else if let Some(near) = near_field(name) {
                                return err(
                                    format!("unknown field {name}: (did you mean {near}:?)"),
                                    start,
                                    start + name.len() + 1,
                                );
                            } else {
                                Lex::Word(w.to_string())
                            }
                        }
                        _ => Lex::Word(w.to_string()),
                    },
                };
                push(&mut out, lex, start, end);
                i = end;
            }
        }
    }
    Ok(out)
}

// ---- parser ----------------------------------------------------------------

struct Parser<'a> {
    toks: Vec<Spanned>,
    pos: usize,
    leaves: usize,
    depth: usize,
    text: &'a str,
}

/// Parses a query. An empty query is [`Node::All`].
pub fn parse(text: &str) -> Result<Node, ParseError> {
    if text.len() > MAX_QUERY_BYTES {
        return err(
            format!("query longer than {MAX_QUERY_BYTES} bytes"),
            MAX_QUERY_BYTES,
            text.len(),
        );
    }
    let toks = lex(text)?;
    if toks.is_empty() {
        return Ok(Node::All);
    }
    let mut p = Parser {
        toks,
        pos: 0,
        leaves: 0,
        depth: 0,
        text,
    };
    let node = p.or()?;
    if let Some(t) = p.toks.get(p.pos) {
        return err("unexpected ')'", t.start, t.end);
    }
    Ok(node)
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Lex> {
        self.toks.get(self.pos).map(|t| &t.lex)
    }

    fn span(&self) -> (usize, usize) {
        self.toks
            .get(self.pos)
            .map_or((self.text.len(), self.text.len()), |t| (t.start, t.end))
    }

    fn or(&mut self) -> Result<Node, ParseError> {
        let mut of = vec![self.and()?];
        while self.peek() == Some(&Lex::Or) {
            self.pos += 1;
            of.push(self.and()?);
        }
        Ok(if of.len() == 1 {
            of.pop().expect("one")
        } else {
            Node::Or { of }
        })
    }

    fn and(&mut self) -> Result<Node, ParseError> {
        let mut of = vec![self.near()?];
        loop {
            match self.peek() {
                None | Some(Lex::Or | Lex::RParen) => break,
                Some(Lex::And) => {
                    self.pos += 1;
                    of.push(self.near()?);
                }
                Some(_) => of.push(self.near()?),
            }
        }
        Ok(if of.len() == 1 {
            of.pop().expect("one")
        } else {
            Node::And { of }
        })
    }

    fn near(&mut self) -> Result<Node, ParseError> {
        let (start, _) = self.span();
        let first = self.unary()?;
        let Some(&Lex::Near(slop)) = self.peek() else {
            return Ok(first);
        };
        let mut field = None;
        let mut words = Vec::new();
        let mut take = |n: Node, at: (usize, usize)| -> Result<(), ParseError> {
            let (f, w) = match n {
                Node::Word { field, word } => (field, vec![word]),
                Node::Phrase { field, text } => {
                    (field, text.split_whitespace().map(str::to_string).collect())
                }
                _ => return err("NEAR joins words or phrases", at.0, at.1),
            };
            if *field.get_or_insert(f) != f {
                return err("NEAR terms must be in the same field", at.0, at.1);
            }
            words.extend(w);
            Ok(())
        };
        take(first, (start, start))?;
        let mut operands = 1;
        while let Some(&Lex::Near(n)) = self.peek() {
            if n != slop {
                let (s, e) = self.span();
                return err("use one NEAR distance per chain", s, e);
            }
            self.pos += 1;
            let at = self.span();
            let next = self.unary()?;
            take(next, at)?;
            operands += 1;
        }
        let (s, e) = self.span();
        if words.len() > MAX_NEAR_WORDS {
            return err(
                format!("NEAR joins at most {MAX_NEAR_WORDS} words"),
                start,
                e.max(s),
            );
        }
        // Each word of a phrase inside NEAR is a clause of its own.
        self.leaves += words.len() - operands;
        if self.leaves > MAX_CLAUSES {
            return err(format!("more than {MAX_CLAUSES} clauses"), start, e.max(s));
        }
        Ok(Node::Near {
            field: field.unwrap_or(TextField::Any),
            words,
            slop,
        })
    }

    fn unary(&mut self) -> Result<Node, ParseError> {
        if self.peek() == Some(&Lex::Not) {
            self.pos += 1;
            let inner = self.unary()?;
            return Ok(match inner {
                Node::Not { of } => *of,
                other => Node::Not {
                    of: Box::new(other),
                },
            });
        }
        self.primary()
    }

    fn leaf(&mut self, start: usize, end: usize) -> Result<(), ParseError> {
        self.leaves += 1;
        if self.leaves > MAX_CLAUSES {
            return err(format!("more than {MAX_CLAUSES} clauses"), start, end);
        }
        Ok(())
    }

    fn primary(&mut self) -> Result<Node, ParseError> {
        let (start, end) = self.span();
        let Some(tok) = self.toks.get(self.pos).cloned() else {
            return err("query ends early", start, end);
        };
        self.pos += 1;
        match tok.lex {
            Lex::LParen => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return err(format!("nested deeper than {MAX_DEPTH}"), start, end);
                }
                if self.peek() == Some(&Lex::RParen) {
                    return err("empty parentheses", start, end + 1);
                }
                let inner = self.or()?;
                if self.peek() != Some(&Lex::RParen) {
                    let (s, e) = self.span();
                    return err("missing ')'", s, e);
                }
                self.pos += 1;
                self.depth -= 1;
                Ok(inner)
            }
            Lex::RParen => err("unexpected ')'", start, end),
            Lex::Or | Lex::And | Lex::Near(_) => {
                err("operator without a term before it", start, end)
            }
            Lex::Not => unreachable!("handled by unary"),
            Lex::Quoted(text) => {
                self.leaf(start, end)?;
                phrase(TextField::Any, &text, start, end)
            }
            Lex::Word(w) => {
                self.leaf(start, end)?;
                word(TextField::Any, &w, start, end)
            }
            Lex::Field(name, value) => {
                self.leaf(start, end)?;
                let kind = field(&name).expect("lexer checked");
                let vstart = start + name.len() + 1;
                if value.is_empty() {
                    // name:"dark knight"
                    if let (FieldKind::Text(f), Some(Lex::Quoted(text))) =
                        (kind, self.peek().cloned())
                    {
                        let end = self.toks[self.pos].end;
                        self.pos += 1;
                        return phrase(f, &text, start, end);
                    }
                    // file:(ext:flac size:>50MB), file:"dark knight"
                    if kind == FieldKind::File
                        && matches!(self.peek(), Some(Lex::LParen | Lex::Quoted(_)))
                    {
                        let inner = self.primary()?;
                        let end = self.toks[self.pos - 1].end;
                        return file(inner, start, end);
                    }
                    if let FieldKind::Later(what) = kind {
                        return err(format!("{name}: arrives with {what}"), start, end);
                    }
                    return err(format!("{name}: needs a value"), start, end);
                }
                field_value(kind, &name, &value, vstart, end)
            }
        }
    }
}

fn phrase(field: TextField, text: &str, start: usize, end: usize) -> Result<Node, ParseError> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return err("empty phrase", start, end);
    }
    Ok(Node::Phrase { field, text })
}

fn word(field: TextField, w: &str, start: usize, end: usize) -> Result<Node, ParseError> {
    if field == TextField::Any {
        if let Some(h) = bare_hash(w) {
            return Ok(Node::Hash { hash: h });
        }
        if w.starts_with('/') {
            return err("regex search arrives with the trigram index", start, end);
        }
    }
    if let Some(rest) = w.strip_prefix('~') {
        if rest.chars().count() < 3 {
            return err("fuzzy terms need at least 3 letters", start, end);
        }
        let distance = if rest.chars().count() < 6 { 1 } else { 2 };
        return Ok(Node::Fuzzy {
            field,
            word: rest.to_string(),
            distance,
        });
    }
    if let Some(prefix) = w.strip_suffix('*') {
        if prefix.chars().count() < 2 || prefix.contains('*') {
            return err("prefixes need at least 2 letters", start, end);
        }
        return Ok(Node::Prefix {
            field,
            prefix: prefix.to_string(),
        });
    }
    if w.contains('*') {
        return err("'*' only works at the end of a word", start, end);
    }
    Ok(Node::Word {
        field,
        word: w.to_string(),
    })
}

/// A bare 40-hex info hash or a magnet link.
fn bare_hash(w: &str) -> Option<String> {
    if w.len() == 40 && w.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(w.to_ascii_lowercase());
    }
    let rest = w.strip_prefix("magnet:?")?;
    rest.split('&').find_map(|kv| {
        let h = kv.strip_prefix("xt=urn:btih:")?;
        (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
    })
}

fn field_value(
    kind: FieldKind,
    name: &str,
    value: &str,
    start: usize,
    end: usize,
) -> Result<Node, ParseError> {
    match kind {
        FieldKind::Text(f) => word(f, value, start, end),
        FieldKind::Num(f) => {
            let unit = if f.is_count() {
                parse_count
            } else {
                parse_bytes
            };
            let (lo, hi) = range(value, unit, start, end)?;
            Ok(Node::Range { field: f, lo, hi })
        }
        FieldKind::Date(f) => {
            let (from, to) = date_range(value, start, end)?;
            Ok(Node::Date { field: f, from, to })
        }
        FieldKind::Ext => {
            let mut any = Vec::new();
            for e in value.split(',') {
                let e = e.trim_start_matches('.').to_ascii_lowercase();
                if e.is_empty() || e.len() > 10 || !e.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return err(
                        format!("{name}: expects extensions like mkv,mp4"),
                        start,
                        end,
                    );
                }
                if !any.contains(&e) {
                    any.push(e);
                }
            }
            Ok(Node::Ext { any })
        }
        FieldKind::Flag(flag) => {
            let value = match value {
                "yes" | "true" | "1" => true,
                "no" | "false" | "0" => false,
                _ => return err(format!("{name}: expects yes or no"), start, end),
            };
            Ok(Node::Flag {
                name: flag.to_string(),
                value,
            })
        }
        FieldKind::Hash => match bare_hash(value) {
            Some(hash) => Ok(Node::Hash { hash }),
            None => err("hash: expects 40 hex digits", start, end),
        },
        FieldKind::Magnet => match bare_hash(&format!("magnet:{value}")) {
            Some(hash) => Ok(Node::Hash { hash }),
            None => err("magnet link without a btih hash", start, end),
        },
        FieldKind::Tag(key) => {
            let mut any = Vec::new();
            for v in value.split(',') {
                let v = tag_value(key, v).ok_or_else(|| ParseError {
                    message: tag_help(key, name, v),
                    start,
                    end,
                })?;
                if !any.contains(&v) {
                    any.push(v);
                }
            }
            Ok(Node::Tag { key, any })
        }
        FieldKind::Share(by) => share(by, name, value, start, end),
        FieldKind::File => file(word(TextField::Any, value, start, end)?, start, end),
        FieldKind::Later(what) => err(format!("{name}: arrives with {what}"), start, end),
    }
}

fn file(of: Node, start: usize, end: usize) -> Result<Node, ParseError> {
    if !crate::files::file_clause_ok(&of) {
        return err(
            "file:(…) takes words, phrases, name:, path:, ext: and size:",
            start,
            end,
        );
    }
    Ok(Node::File { of: Box::new(of) })
}

/// Longest keyword value accepted.
const MAX_TAG_BYTES: usize = 64;

/// A keyword value as the index holds it, or `None` if it can't be one.
pub(crate) fn tag_value(key: TagKey, v: &str) -> Option<String> {
    use lodestar_classify::{AdultKind, ContentType, ResolvedVia, Structure};
    let mut lower = v.to_lowercase();
    // `res:360` is read as `360p`, so both spellings land on the same
    // stored form (the parser files 360p under 480p), and the limit
    // applies to that form, which is what renders.
    let bare_res =
        key == TagKey::Res && !lower.is_empty() && lower.bytes().all(|b| b.is_ascii_digit());
    if bare_res {
        lower.push('p');
    }
    if lower.is_empty() || lower.len() > MAX_TAG_BYTES {
        return None;
    }
    let release_key = match key {
        TagKey::Type => return ContentType::from_name(&lower).map(|t| t.as_str().to_owned()),
        TagKey::Has => return Structure::from_name(&lower).map(|_| lower),
        TagKey::Resolved => {
            return ResolvedVia::from_name(&lower).map(|r| r.as_str().to_owned());
        }
        TagKey::Adult => return AdultKind::from_name(&lower).map(|k| k.as_str().to_owned()),
        TagKey::Group => return Some(lower),
        TagKey::Res => "res",
        TagKey::Hdr => "hdr",
        TagKey::Codec => "vcodec",
        TagKey::Audio => "acodec",
        TagKey::Lang => "lang",
    };
    // Read the value the way the parser reads it in a name, so aliases
    // (`x265`, `4k`, `dovi`, `french`) land on the stored form.
    let v = if bare_res { lower.as_str() } else { v };
    let parsed = lodestar_release_parser::parse(&format!("A [{v}]"));
    if let Some((_, found)) = parsed.fields().into_iter().find(|(k, _)| *k == release_key) {
        return found.split(", ").next().map(str::to_owned);
    }
    // Otherwise take it as written, if it looks like a stored value
    // (`fr`, `1080p`).
    let plain = lower
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if !plain {
        return None;
    }
    Some(lower)
}

fn tag_help(key: TagKey, name: &str, v: &str) -> String {
    use lodestar_classify::{AdultKind, ContentType, ResolvedVia, Structure};
    let list = |names: Vec<&str>| names.join(", ");
    match key {
        TagKey::Type => format!(
            "{name}: expects one of {}",
            list(ContentType::ALL.iter().map(|t| t.as_str()).collect())
        ),
        TagKey::Has => format!(
            "{name}: expects one of {}",
            list(Structure::NAMED.iter().map(|(_, n)| *n).collect())
        ),
        TagKey::Resolved => format!(
            "{name}: expects one of {}",
            list(ResolvedVia::ALL.iter().map(|r| r.as_str()).collect())
        ),
        TagKey::Adult => format!(
            "{name}: expects one of {}",
            list(AdultKind::ALL.iter().map(|k| k.as_str()).collect())
        ),
        _ => format!("{name}: can't read {v:?}"),
    }
}

/// `cat:video` (any), `cat:video>=80%`, `cat:software=0`,
/// `bytes:video=10..50`.
fn share(
    by: ShareBy,
    name: &str,
    value: &str,
    start: usize,
    end: usize,
) -> Result<Node, ParseError> {
    use lodestar_classify::Category;
    let split = value.find(['<', '>', '=']).unwrap_or(value.len());
    let (cat, rest) = value.split_at(split);
    let cat = cat.to_ascii_lowercase();
    let Some(cat) = Category::ALL.iter().find(|c| c.as_str() == cat) else {
        let names: Vec<&str> = Category::ALL.iter().map(|c| c.as_str()).collect();
        return err(
            format!("{name}: expects a category: {}", names.join(", ")),
            start,
            end,
        );
    };
    let (lo, hi) = if rest.is_empty() {
        (1, 100)
    } else {
        let rest = rest
            .strip_prefix('=')
            .filter(|r| r.contains(".."))
            .unwrap_or(rest);
        let (lo, hi) = range(rest, parse_percent, start, end)?;
        let (lo, hi) = (lo.unwrap_or(0), hi.unwrap_or(100).min(100));
        if lo > hi {
            return err(format!("{name}: shares run 0–100%"), start, end);
        }
        (lo as u8, hi as u8)
    };
    Ok(Node::Share {
        by,
        cat: cat.as_str().to_owned(),
        lo,
        hi,
    })
}

fn parse_percent(s: &str) -> Option<u64> {
    s.strip_suffix('%').unwrap_or(s).parse().ok()
}

// ---- values ----------------------------------------------------------------

/// `>x`, `>=x`, `<x`, `<=x`, `=x`, `x`, `a..b`, `a..`, `..b`, inclusive.
fn range(
    value: &str,
    unit: fn(&str) -> Option<u64>,
    start: usize,
    end: usize,
) -> Result<(Option<u64>, Option<u64>), ParseError> {
    let bad = || ParseError {
        message: format!("can't read {value:?} as a number or range"),
        start,
        end,
    };
    let one = |s: &str| unit(s).ok_or_else(bad);
    if let Some((a, b)) = value.split_once("..") {
        let lo = if a.is_empty() { None } else { Some(one(a)?) };
        let hi = if b.is_empty() { None } else { Some(one(b)?) };
        if lo.is_none() && hi.is_none() {
            return Err(bad());
        }
        if let (Some(l), Some(h)) = (lo, hi) {
            if l > h {
                return err("range runs backwards", start, end);
            }
        }
        return Ok((lo, hi));
    }
    Ok(if let Some(v) = value.strip_prefix(">=") {
        (Some(one(v)?), None)
    } else if let Some(v) = value.strip_prefix("<=") {
        (None, Some(one(v)?))
    } else if let Some(v) = value.strip_prefix('>') {
        (Some(one(v)?.saturating_add(1)), None)
    } else if let Some(v) = value.strip_prefix('<') {
        (None, Some(one(v)?.checked_sub(1).ok_or_else(bad)?))
    } else {
        let v = one(value.strip_prefix('=').unwrap_or(value))?;
        (Some(v), Some(v))
    })
}

fn parse_count(s: &str) -> Option<u64> {
    s.parse().ok()
}

/// `4GB`, `700MB`, `1.5GiB`, `512`. KB/MB/GB/TB are binary (1024-based),
/// matching how the UI shows sizes (§10.12).
fn parse_bytes(s: &str) -> Option<u64> {
    let lower = s.to_ascii_lowercase();
    let split = lower
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(lower.len());
    let (num, unit) = lower.split_at(split);
    let mult: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => return None,
    };
    if num.is_empty() || num.matches('.').count() > 1 || num.len() > 20 {
        return None;
    }
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    let int: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut value = int.checked_mul(mult)?;
    if !frac.is_empty() {
        let digits = frac.len().min(9) as u32;
        let f: u64 = frac[..digits as usize].parse().ok()?;
        value = value.checked_add(f.checked_mul(mult)? / 10u64.pow(digits))?;
    }
    Some(value)
}

/// `<7d` (newer than 7 days ago), `>2w` (older), `2026-01-01` (that UTC
/// day), `2026-01-01..2026-03-31`, `>2026-01-01`, open ranges.
fn date_range(
    value: &str,
    start: usize,
    end: usize,
) -> Result<(Option<When>, Option<When>), ParseError> {
    let bad = || ParseError {
        message: format!("can't read {value:?} as a date, range or age like <7d"),
        start,
        end,
    };
    if let Some(v) = value.strip_prefix('<') {
        return Ok((Some(When::Ago(duration(v).ok_or_else(bad)?)), None));
    }
    if let Some(v) = value.strip_prefix('>') {
        if let Some(d) = duration(v) {
            return Ok((None, Some(When::Ago(d))));
        }
        let day = day_start(v).ok_or_else(bad)?;
        return Ok((Some(When::At(day + 86_400)), None));
    }
    if let Some((a, b)) = value.split_once("..") {
        let from = if a.is_empty() {
            None
        } else {
            Some(When::At(day_start(a).ok_or_else(bad)?))
        };
        let to = if b.is_empty() {
            None
        } else {
            Some(When::At(day_start(b).ok_or_else(bad)? + 86_399))
        };
        if from.is_none() && to.is_none() {
            return Err(bad());
        }
        if let (Some(When::At(f)), Some(When::At(t))) = (from, to) {
            if f > t {
                return err("range runs backwards", start, end);
            }
        }
        return Ok((from, to));
    }
    let day = day_start(value).ok_or_else(bad)?;
    Ok((Some(When::At(day)), Some(When::At(day + 86_399))))
}

/// `30m`, `6h`, `7d`, `2w`, `3mo`, `1y` in seconds.
fn duration(s: &str) -> Option<i64> {
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok().filter(|n| *n > 0)?;
    let secs = match unit {
        "m" | "min" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "mo" => 30 * 86_400,
        "y" => 365 * 86_400,
        _ => return None,
    };
    n.checked_mul(secs).filter(|s| *s <= 200 * 365 * 86_400)
}

/// Unix time of 00:00 UTC on `YYYY-MM-DD`.
fn day_start(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=2200).contains(&y)
        || !(1..=12).contains(&m)
        || d == 0
        || d > days_in[m as usize - 1]
    {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

fn date_text(t: i64) -> String {
    // Civil from days.
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---- rendering -------------------------------------------------------------

impl Node {
    /// Whether any clause tests this flag.
    pub fn mentions_flag(&self, flag: &str) -> bool {
        match self {
            Node::Flag { name, .. } => name == flag,
            Node::And { of } | Node::Or { of } => of.iter().any(|n| n.mentions_flag(flag)),
            Node::Not { of } | Node::File { of } => of.mentions_flag(flag),
            _ => false,
        }
    }

    /// Whether the query tests the `key` facet anywhere.
    pub fn mentions_tag(&self, key: TagKey) -> bool {
        match self {
            Node::Tag { key: k, .. } => *k == key,
            Node::And { of } | Node::Or { of } => of.iter().any(|n| n.mentions_tag(key)),
            Node::Not { of } | Node::File { of } => of.mentions_tag(key),
            _ => false,
        }
    }

    /// The query with torrents a filter rule hid left out (§20.3), unless
    /// it asks about hidden torrents itself.
    pub fn without_hidden(self) -> Node {
        if self.mentions_flag("hidden") {
            return self;
        }
        let visible = Node::Flag {
            name: "hidden".into(),
            value: false,
        };
        match self {
            Node::All => visible,
            Node::And { mut of } => {
                of.push(visible);
                Node::And { of }
            }
            other => Node::And {
                of: vec![other, visible],
            },
        }
    }

    /// Whether the query has a `file:(…)` clause, so its results have
    /// matched files ([`SearchIndex::matched_files`]).
    ///
    /// [`SearchIndex::matched_files`]: crate::SearchIndex::matched_files
    pub fn has_file_clause(&self) -> bool {
        match self {
            Node::And { of } | Node::Or { of } => of.iter().any(Node::has_file_clause),
            Node::Not { of } => of.has_file_clause(),
            Node::File { .. } => true,
            _ => false,
        }
    }

    /// Canonical query text; [`parse`] of it gives back the same AST.
    pub fn to_query(&self) -> String {
        let mut s = String::new();
        self.render(&mut s, false);
        s
    }

    fn render(&self, out: &mut String, nested: bool) {
        let text_field = |f: TextField| match f {
            TextField::Any => "",
            TextField::Name => "name:",
            TextField::Path => "path:",
            TextField::Title => "title:",
        };
        match self {
            Node::All => {}
            Node::And { of } => {
                if nested {
                    out.push('(');
                }
                for (i, n) in of.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    n.render(out, matches!(n, Node::Or { .. } | Node::And { .. }));
                }
                if nested {
                    out.push(')');
                }
            }
            Node::Or { of } => {
                if nested {
                    out.push('(');
                }
                for (i, n) in of.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    n.render(out, matches!(n, Node::Or { .. }));
                }
                if nested {
                    out.push(')');
                }
            }
            Node::Not { of } => {
                out.push('-');
                of.render(
                    out,
                    matches!(**of, Node::And { .. } | Node::Or { .. } | Node::Near { .. }),
                );
            }
            Node::Word { field, word } => {
                let _ = write!(out, "{}{word}", text_field(*field));
            }
            Node::Phrase { field, text } => {
                let _ = write!(out, "{}\"{text}\"", text_field(*field));
            }
            Node::Near { field, words, slop } => {
                // Negated, it needs brackets: `-a NEAR b` reads as NEAR
                // applied to `-a`.
                if nested {
                    out.push('(');
                }
                for (i, w) in words.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(out, " NEAR/{slop} ");
                    }
                    // Words from a phrase can look like operators or
                    // syntax; quoting keeps them words.
                    if is_plain_word(w) {
                        let _ = write!(out, "{}{w}", text_field(*field));
                    } else {
                        let _ = write!(out, "{}\"{w}\"", text_field(*field));
                    }
                }
                if nested {
                    out.push(')');
                }
            }
            Node::Prefix { field, prefix } => {
                let _ = write!(out, "{}{prefix}*", text_field(*field));
            }
            Node::Fuzzy { field, word, .. } => {
                let _ = write!(out, "{}~{word}", text_field(*field));
            }
            Node::Range { field, lo, hi } => {
                let name = match field {
                    NumField::Size => "size",
                    NumField::Files => "files",
                    NumField::Largest => "largest",
                    NumField::Piece => "piece",
                    NumField::Year => "year",
                    NumField::Season => "s",
                    NumField::Episode => "e",
                    NumField::Junk => "junk",
                };
                let count = field.is_count();
                let v = |n: u64| if count { n.to_string() } else { bytes_text(n) };
                let _ = match (lo, hi) {
                    (Some(l), Some(h)) if l == h => write!(out, "{name}:{}", v(*l)),
                    (Some(l), Some(h)) => write!(out, "{name}:{}..{}", v(*l), v(*h)),
                    (Some(l), None) => write!(out, "{name}:>={}", v(*l)),
                    (None, Some(h)) => write!(out, "{name}:<={}", v(*h)),
                    (None, None) => write!(out, "{name}:0.."),
                };
            }
            Node::Date { from, to, .. } => {
                let day = |t: &i64| date_text(*t);
                let _ = match (from, to) {
                    (Some(When::Ago(s)), None) => write!(out, "discovered:<{}", ago_text(*s)),
                    (None, Some(When::Ago(s))) => write!(out, "discovered:>{}", ago_text(*s)),
                    (Some(When::At(f)), Some(When::At(t)))
                        if t - f == 86_399 && f % 86_400 == 0 =>
                    {
                        write!(out, "discovered:{}", day(f))
                    }
                    (Some(When::At(f)), None) if f % 86_400 == 0 => {
                        write!(out, "discovered:>{}", day(&(f - 86_400)))
                    }
                    (f, t) => {
                        let end = |w: &Option<When>, last: bool| match w {
                            Some(When::At(t)) => day(&(if last { t - 86_399 } else { *t })),
                            _ => String::new(),
                        };
                        write!(out, "discovered:{}..{}", end(f, false), end(t, true))
                    }
                };
            }
            Node::Ext { any } => {
                let _ = write!(out, "ext:{}", any.join(","));
            }
            Node::Flag { name, value } => {
                let key = FIELDS
                    .iter()
                    .find(|(_, k)| matches!(k, FieldKind::Flag(f) if f == name))
                    .map_or(name.as_str(), |(n, _)| n);
                let _ = write!(out, "{key}:{}", if *value { "yes" } else { "no" });
            }
            Node::Hash { hash } => {
                let _ = write!(out, "hash:{hash}");
            }
            Node::Tag { key, any } => {
                let _ = write!(out, "{}:{}", key.as_str(), any.join(","));
            }
            Node::File { of } => match **of {
                // A single word keeps the `file:word` form: in brackets,
                // `file:res:x`'s word would read as a field, and
                // `file:-x`'s as a negation.
                Node::Word {
                    field: TextField::Any,
                    ..
                }
                | Node::Prefix {
                    field: TextField::Any,
                    ..
                }
                | Node::Fuzzy {
                    field: TextField::Any,
                    ..
                } => {
                    out.push_str("file:");
                    of.render(out, false);
                }
                _ => {
                    out.push_str("file:(");
                    of.render(out, false);
                    out.push(')');
                }
            },
            Node::Share { by, cat, lo, hi } => {
                let name = match by {
                    ShareBy::Count => "cat",
                    ShareBy::Bytes => "bytes",
                };
                let _ = match (lo, hi) {
                    (1, 100) => write!(out, "{name}:{cat}"),
                    (l, h) if l == h => write!(out, "{name}:{cat}={l}"),
                    (l, 100) => write!(out, "{name}:{cat}>={l}"),
                    (0, h) => write!(out, "{name}:{cat}<={h}"),
                    (l, h) => write!(out, "{name}:{cat}={l}..{h}"),
                };
            }
        }
    }
}

/// Whether `w` on its own parses back to the plain word `w`.
pub(crate) fn is_plain_word(w: &str) -> bool {
    matches!(
        parse(w),
        Ok(Node::Word { field: TextField::Any, word }) if word == w
    )
}

fn bytes_text(n: u64) -> String {
    for (unit, shift) in [("TB", 40), ("GB", 30), ("MB", 20), ("KB", 10)] {
        if n >= 1 << shift && n % (1 << shift) == 0 {
            return format!("{}{unit}", n >> shift);
        }
    }
    n.to_string()
}

fn ago_text(s: i64) -> String {
    for (unit, secs) in [("y", 365 * 86_400), ("d", 86_400), ("h", 3600), ("m", 60)] {
        if s % secs == 0 {
            return format!("{}{unit}", s / secs);
        }
    }
    format!("{}m", s / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(word: &str) -> Node {
        Node::Word {
            field: TextField::Any,
            word: word.into(),
        }
    }

    #[test]
    fn words_phrases_and_operators() {
        assert_eq!(parse("").unwrap(), Node::All);
        assert_eq!(
            parse("dark knight").unwrap(),
            Node::And {
                of: vec![w("dark"), w("knight")]
            }
        );
        assert_eq!(
            parse("batman | joker OR bane").unwrap(),
            Node::Or {
                of: vec![w("batman"), w("joker"), w("bane")]
            }
        );
        assert_eq!(
            parse("(a | b) AND c -cam !ts").unwrap(),
            Node::And {
                of: vec![
                    Node::Or {
                        of: vec![w("a"), w("b")]
                    },
                    w("c"),
                    Node::Not {
                        of: Box::new(w("cam"))
                    },
                    Node::Not {
                        of: Box::new(w("ts"))
                    },
                ]
            }
        );
        assert_eq!(
            parse("name:\"dark  knight\"").unwrap(),
            Node::Phrase {
                field: TextField::Name,
                text: "dark knight".into()
            }
        );
        assert_eq!(
            parse("dark NEAR/3 knight").unwrap(),
            Node::Near {
                field: TextField::Any,
                words: vec!["dark".into(), "knight".into()],
                slop: 3
            }
        );
        assert_eq!(
            parse("comput*").unwrap(),
            Node::Prefix {
                field: TextField::Any,
                prefix: "comput".into()
            }
        );
        assert_eq!(
            parse("~knigth").unwrap(),
            Node::Fuzzy {
                field: TextField::Any,
                word: "knigth".into(),
                distance: 2
            }
        );
        // A hyphen inside a word isn't negation; Re:Zero isn't a field.
        assert_eq!(parse("web-dl").unwrap(), w("web-dl"));
        assert_eq!(parse("Re:Zero").unwrap(), w("Re:Zero"));
    }

    #[test]
    fn fields_and_values() {
        let gib = 1u64 << 30;
        assert_eq!(
            parse("size:>4GB").unwrap(),
            Node::Range {
                field: NumField::Size,
                lo: Some(4 * gib + 1),
                hi: None
            }
        );
        assert_eq!(
            parse("size:700MB..1.5GB").unwrap(),
            Node::Range {
                field: NumField::Size,
                lo: Some(700 << 20),
                hi: Some(gib + gib / 2)
            }
        );
        assert_eq!(
            parse("files:>=10").unwrap(),
            Node::Range {
                field: NumField::Files,
                lo: Some(10),
                hi: None
            }
        );
        assert_eq!(
            parse("piece:16MB").unwrap(),
            Node::Range {
                field: NumField::Piece,
                lo: Some(16 << 20),
                hi: Some(16 << 20)
            }
        );
        assert_eq!(
            parse("ext:mkv,.MP4,mkv").unwrap(),
            Node::Ext {
                any: vec!["mkv".into(), "mp4".into()]
            }
        );
        assert_eq!(
            parse("private:no").unwrap(),
            Node::Flag {
                name: "private".into(),
                value: false
            }
        );
        assert_eq!(
            parse("discovered:<7d").unwrap(),
            Node::Date {
                field: DateField::Discovered,
                from: Some(When::Ago(7 * 86_400)),
                to: None
            }
        );
        let jan1 = day_start("2026-01-01").unwrap();
        assert_eq!(jan1, 1_767_225_600);
        assert_eq!(
            parse("discovered:2026-01-01..2026-03-31").unwrap(),
            Node::Date {
                field: DateField::Discovered,
                from: Some(When::At(jan1)),
                to: Some(When::At(day_start("2026-03-31").unwrap() + 86_399))
            }
        );
        let h = "0123456789abcdef0123456789ABCDEF01234567";
        let want = Node::Hash {
            hash: h.to_lowercase(),
        };
        assert_eq!(parse(h).unwrap(), want);
        assert_eq!(parse(&format!("hash:{h}")).unwrap(), want);
        assert_eq!(
            parse(&format!("magnet:?xt=urn:btih:{h}&dn=x")).unwrap(),
            want
        );
    }

    #[test]
    fn helpful_errors() {
        let e = |q: &str| parse(q).unwrap_err().message;
        assert_eq!(e("sise:>1GB"), "unknown field sise: (did you mean size:?)");
        assert_eq!(
            e("file:(res:1080p)"),
            "file:(…) takes words, phrases, name:, path:, ext: and size:"
        );
        assert!(e("file:(file:(x))").starts_with("file:(…) takes"));
        assert_eq!(e("file:"), "file: needs a value");
        assert!(e("type:film").starts_with("type: expects one of movie, tv"));
        assert!(e("has:nope").contains("nfo"));
        assert!(e("cat:vid>1").contains("expects a category"));
        assert!(e("cat:video>100").contains("0–100%"));
        assert_eq!(e("codec:a$b"), "codec: can't read \"a$b\"");
        assert_eq!(e("seeders:>5"), "seeders: arrives with swarm health (M4)");
        assert!(e("size:huge").contains("can't read"));
        assert!(e("(a b").contains("missing ')'"));
        assert!(e("a b)").contains("unexpected ')'"));
        assert!(e("\"open").contains("unclosed quote"));
        assert!(e("| a").contains("operator"));
        assert!(e("ext:m!k").contains("ext"));
        assert!(e("a*b").contains("'*'"));
        assert!(e("a NEAR/0 b").contains("NEAR"));
        assert!(e("size:5GB..1GB").contains("backwards"));
        assert!(e("/S0[1-9]/").contains("regex"));
        let err = parse("x sise:1").unwrap_err();
        assert_eq!((err.start, err.end), (2, 7));
    }

    #[test]
    fn a_bare_resolution_reads_as_its_p_form() {
        // Found by fuzz (query): `res:360` took `360p` as written, which
        // renders and re-parses as `480p`.
        for n in ["240", "360", "480", "576", "720", "1080", "2160", "999"] {
            let bare = parse(&format!("res:{n}")).unwrap();
            assert_eq!(bare, parse(&format!("res:{n}p")).unwrap(), "res:{n}");
            assert_eq!(parse(&bare.to_query()).unwrap(), bare, "res:{n}");
        }
        let fuzzed = parse("\n\n!!?\nres:360\n").unwrap();
        assert_eq!(parse(&fuzzed.to_query()).unwrap(), fuzzed);
        // Also found by fuzz (query): 64 digits passed the 64-byte limit,
        // then rendered as 65 bytes with their `p`, which didn't parse
        // back. The limit applies to the stored form, either spelling.
        // An empty part of a list is no resolution: refused, as for
        // every other key (`codec:x265,`), not read as "p".
        for empty in ["res:720,", "res:,720", "res:,", "res:"] {
            assert!(parse(empty).is_err(), "{empty}");
        }
        let digits = "4".repeat(63) + "5";
        assert!(parse(&format!("res:{digits}")).is_err());
        assert!(parse(&format!("res:{digits}p")).is_err());
        let fits = &digits[1..];
        assert_eq!(
            parse(&format!("res:{fits}")).unwrap(),
            parse(&format!("res:{fits}p")).unwrap()
        );
        let raw = "<\u{1}\u{1}\u{d} \u{0}. ! res:4444444444444444444444444444444444444444444444444444444444444445\u{a}!NR\u{5}\u{5}\u{0}3\u{0}\u{1e}\u{0}\u{0}";
        assert!(raw.contains(&format!("res:{digits}\n")));
        if let Ok(ast) = parse(raw) {
            assert_eq!(parse(&ast.to_query()).unwrap(), ast, "{raw:?}");
        }
    }

    #[test]
    fn enrichment_fields() {
        let tag = |key, any: &[&str]| Node::Tag {
            key,
            any: any.iter().map(|s| s.to_string()).collect(),
        };
        // Values land on the stored form, as the parser reads them.
        assert_eq!(parse("codec:x265").unwrap(), tag(TagKey::Codec, &["h265"]));
        assert_eq!(parse("res:4K").unwrap(), tag(TagKey::Res, &["2160p"]));
        assert_eq!(parse("res:1080").unwrap(), tag(TagKey::Res, &["1080p"]));
        assert_eq!(parse("hdr:dovi").unwrap(), tag(TagKey::Hdr, &["dv"]));
        assert_eq!(parse("audio:DD+").unwrap(), tag(TagKey::Audio, &["eac3"]));
        assert_eq!(
            parse("lang:french,fr,multi").unwrap(),
            tag(TagKey::Lang, &["fr", "multi"])
        );
        assert_eq!(
            parse("type:Movie,tv").unwrap(),
            tag(TagKey::Type, &["movie", "tv"])
        );
        assert_eq!(parse("group:NTb").unwrap(), tag(TagKey::Group, &["ntb"]));
        assert_eq!(parse("has:nfo").unwrap(), tag(TagKey::Has, &["nfo"]));
        assert_eq!(
            parse("hidden:yes").unwrap(),
            Node::Flag {
                name: "hidden".into(),
                value: true
            }
        );
        assert_eq!(parse("type:Adult").unwrap(), tag(TagKey::Type, &["adult"]));
        assert_eq!(
            parse("adult:scene,VR").unwrap(),
            tag(TagKey::Adult, &["scene", "vr"])
        );
        assert!(
            parse("adult:nope")
                .unwrap_err()
                .message
                .contains("scene, movie")
        );
        assert_eq!(
            parse("resolved:files").unwrap(),
            tag(TagKey::Resolved, &["files"])
        );
        assert_eq!(
            parse("ambiguous:yes").unwrap(),
            Node::Flag {
                name: "ambiguous".into(),
                value: true
            }
        );
        assert_eq!(
            parse("title:\"dark knight\"").unwrap(),
            Node::Phrase {
                field: TextField::Title,
                text: "dark knight".into()
            }
        );
        let range = |field, lo, hi| Node::Range { field, lo, hi };
        assert_eq!(
            parse("year:2019..2021").unwrap(),
            range(NumField::Year, Some(2019), Some(2021))
        );
        assert_eq!(
            parse("s:2").unwrap(),
            range(NumField::Season, Some(2), Some(2))
        );
        assert_eq!(
            parse("e:>5").unwrap(),
            range(NumField::Episode, Some(6), None)
        );
        assert_eq!(
            parse("junk:<20").unwrap(),
            range(NumField::Junk, None, Some(19))
        );
        let share = |by, lo, hi| Node::Share {
            by,
            cat: "video".into(),
            lo,
            hi,
        };
        assert_eq!(
            parse("cat:video>=80%").unwrap(),
            share(ShareBy::Count, 80, 100)
        );
        assert_eq!(parse("cat:video=0").unwrap(), share(ShareBy::Count, 0, 0));
        assert_eq!(parse("cat:VIDEO").unwrap(), share(ShareBy::Count, 1, 100));
        assert_eq!(
            parse("bytes:video>90%").unwrap(),
            share(ShareBy::Bytes, 91, 100)
        );
        assert_eq!(
            parse("bytes:video=10..50").unwrap(),
            share(ShareBy::Bytes, 10, 50)
        );
        assert_eq!(
            parse("cat:video<=100").unwrap(),
            share(ShareBy::Count, 0, 100)
        );
    }

    #[test]
    fn stored_values_normalise_to_themselves() {
        // Every value the release parser stores must query as itself, or
        // a facet the UI offers would match nothing.
        for (key, values) in [
            (
                TagKey::Res,
                &[
                    "480p", "576p", "720p", "1080i", "1080p", "1440p", "2160p", "4320p",
                ][..],
            ),
            (
                TagKey::Codec,
                &["h264", "h265", "av1", "vp9", "xvid", "divx", "mpeg2", "vc1"],
            ),
            (TagKey::Hdr, &["hdr", "hdr10", "hdr10+", "dv", "hlg"]),
            (
                TagKey::Audio,
                &[
                    "aac",
                    "ac3",
                    "eac3",
                    "dts",
                    "dts-hd",
                    "dts-hd-ma",
                    "dts-x",
                    "truehd",
                    "atmos",
                    "flac",
                    "mp3",
                    "opus",
                    "lpcm",
                    "alac",
                    "vorbis",
                ],
            ),
            (TagKey::Lang, &["en", "fr", "de", "ru", "ja", "multi"]),
        ] {
            for v in values {
                assert_eq!(tag_value(key, v).as_deref(), Some(*v), "{key:?} {v}");
            }
        }
    }

    #[test]
    fn file_clauses() {
        let file = |of| Node::File { of: Box::new(of) };
        assert_eq!(parse("file:sample").unwrap(), file(w("sample")));
        assert_eq!(
            parse("file:\"dark knight\"").unwrap(),
            file(Node::Phrase {
                field: TextField::Any,
                text: "dark knight".into()
            })
        );
        assert_eq!(
            parse("file:(ext:flac size:>50MB)").unwrap(),
            file(Node::And {
                of: vec![
                    Node::Ext {
                        any: vec!["flac".into()]
                    },
                    Node::Range {
                        field: NumField::Size,
                        lo: Some((50 << 20) + 1),
                        hi: None
                    },
                ]
            })
        );
    }

    #[test]
    fn limits() {
        assert!(parse(&"a ".repeat(MAX_CLAUSES)).is_ok());
        assert!(parse(&"a ".repeat(MAX_CLAUSES + 1)).is_err());
        assert!(
            parse(&format!(
                "{}a{}",
                "(".repeat(MAX_DEPTH),
                ")".repeat(MAX_DEPTH)
            ))
            .is_ok()
        );
        assert!(
            parse(&format!(
                "{}a{}",
                "(".repeat(MAX_DEPTH + 1),
                ")".repeat(MAX_DEPTH + 1)
            ))
            .is_err()
        );
        assert!(parse(&"x".repeat(MAX_QUERY_BYTES + 1)).is_err());
    }

    #[test]
    fn round_trips() {
        for q in [
            "dark knight",
            "(batman | joker) -cam",
            "name:\"dark knight\" path:extras",
            "dark NEAR/5 knight",
            "comput* ~knigth",
            "size:>=4GB files:10..20 largest:<=700MB piece:16MB",
            "ext:mkv,mp4 private:no partial:yes",
            "discovered:<7d age:>2w",
            "discovered:2026-01-01..2026-03-31 discovered:2026-02-02 discovered:>2026-01-01",
            "-(a | b) -\"x y\"",
            "hash:0123456789abcdef0123456789abcdef01234567",
            "title:\"dark knight\" title:batman type:movie,tv res:2160p codec:h265",
            "hdr:dv audio:atmos lang:fr group:ntb has:nfo resolved:folder ambiguous:no",
            "year:2019..2021 s:2 e:>=5 junk:<=20",
            "cat:video cat:software=0 bytes:video>=91 cat:audio<=40 bytes:image=10..50",
            "file:(ext:flac size:>50MB) -file:sample file:(\"a b\" | name:co* path:extras)",
            "(x | file:(a -b)) file:(dark NEAR/2 knight)",
            // Found by fuzzing: phrase words that look like syntax, a
            // lone `!` before `)`, and a negated NEAR.
            "\"a\" NEAR \"NEAR $*\" NEAR \"x|y\"",
            "!(\"?\"NEAR\n\0m){A",
            "file:res:x file:-y file:NEAR file:~abcd file:res:x* file:(a)",
            "(a ,$!\r!\r) b",
        ] {
            let ast = parse(q).unwrap();
            let text = ast.to_query();
            assert_eq!(parse(&text).unwrap(), ast, "{q} → {text}");
            let json = serde_json::to_string(&ast).unwrap();
            assert_eq!(serde_json::from_str::<Node>(&json).unwrap(), ast);
        }
    }
}
