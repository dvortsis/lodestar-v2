//! TheTVDB API v4 answers (ENR-tvdb; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-15-ENR-tvdb.md`): `POST /v4/login` (an API key
//! exchanged for a bearer token) and `GET /v4/search` (series whose name,
//! alias or translation matches a title).
//!
//! It follows the Wikidata parser's pattern (ENR-14): a pure function
//! from bytes to values on the bounded [`json`](crate::json) reader, no
//! I/O and no panics. Every cap is a named field of [`Limits`]. Anything
//! outside the shape below, in a part it reads, spoils that result,
//! counted in [`Skipped`]; [`Page::whole`] is the one rule a plugin
//! applies, so a broken answer is never mistaken for a miss.
//!
//! The shapes, by root key:
//!
//! - login: `{"status": "success", "data": {"token": text}}`. The token
//!   is kept as a [`Token`], whose `Debug` never prints it.
//! - search: `{"status": "success", "data": [{"tvdb_id": "81189", "type":
//!   "series", "name": text, "aliases": [text, …], "translations": {lang:
//!   text, …}, "primary_language": lang, "year": "2008",
//!   "first_air_time": "2008-01-20", "genres": [text, …], "remote_ids":
//!   [{"id": text, "type": n, "sourceName": text}, …], …}, …], "links":
//!   {…}}`. `data` may be `null` for no result.
//! - anything else on any status: `status` other than `success` is
//!   [`Page::failed`]; the service's `message` is never read.
//!
//! **What it keeps:** a result's TVDB id, type, titles (name, aliases and
//! translated names), primary language, years (`year` and the first air
//! date's), genres, and its IMDb title and TMDB ids from `remote_ids`.
//! Never an overview, an image, a network, a company, a director, a
//! person or any other remote id: those are passed over unread. Ids are
//! parsed strictly: a TVDB or TMDB id is 1–20 digits with no leading
//! zero; an IMDb id must be a title (`tt…`), never a person (`nm…`).
//!
//! Keys are compared as written. In a part that's read, a key with an
//! escape spoils it, and so does a repeat of a key it reads: a field, a
//! translation's language or a root key. Repeats are found with a
//! bitmask over the known keys and a scan of what's kept (at most the
//! count caps), never a list of every key seen, so a body of a million
//! unknown keys is read in linear time.

use std::fmt;

use crate::common::{Bad, prepare, title};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{R, Reader, TextError, Walk};

/// Bytes in a language tag (BCP 47's longest registered form fits).
pub const MAX_LANGUAGE: usize = 35;
/// Bytes in a bearer token. TheTVDB's are JWTs of under a kilobyte; the
/// credential store's limit (`consent::MAX_VALUE`) is the bound.
pub const MAX_TOKEN: usize = 4096;

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array, read or passed over; over it the result is
    /// skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the result is
    /// skipped.
    pub max_text: usize,
    /// Search results, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Titles kept per result (name, aliases and translations together);
    /// the rest are passed over undecoded ([`Series::truncated`]).
    pub max_texts: usize,
    /// Remote ids decoded per result; the rest are passed over
    /// ([`Series::truncated`]).
    pub max_ids: usize,
    /// Genres kept per result; the rest are passed over
    /// ([`Series::truncated`]).
    pub max_genres: usize,
}

/// The caps for a search answer: at most a page of results, each with
/// many translated overviews the parser passes over.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 1024 * 1024,
    max_depth: 16,
    max_values: 100_000,
    max_array: 1_000,
    max_text: 64 * 1024,
    max_items: 10,
    max_texts: 64,
    max_ids: 32,
    max_genres: 16,
};

/// The caps for a login answer: one small object.
pub const LOGIN_LIMITS: Limits = Limits {
    max_bytes: 16 * 1024,
    max_depth: 8,
    max_values: 256,
    max_array: 16,
    max_text: MAX_TOKEN,
    max_items: 0,
    max_texts: 0,
    max_ids: 0,
    max_genres: 0,
};

/// A bearer token. Its `Debug` prints its length only; the plugin moves
/// it into a zeroizing secret at once.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// The token's text, to be moved into a secret.
    pub fn into_inner(self) -> String {
        self.0
    }

    /// 1–[`MAX_TOKEN`] bytes of what a JWT or opaque bearer token holds:
    /// ASCII letters, digits and `-`, `_`, `.`, `~`, `+`, `/`, `=`. Nothing
    /// that could end a header.
    pub fn parse(s: String) -> Option<Token> {
        let ok = (1..=MAX_TOKEN).contains(&s.len())
            && s.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
            });
        ok.then_some(Token(s))
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token(len {})", self.0.len())
    }
}

/// A result's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Series,
    Movie,
    /// A person, a company or anything newer: never matched.
    Other,
}

/// A translated name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// Lower case, 2–[`MAX_LANGUAGE`] bytes of ASCII letters, digits and
    /// `-`, as the service wrote it (usually ISO 639-2, such as `eng`).
    pub language: String,
    /// Hidden characters removed, trimmed, at most [`MAX_TITLE`] bytes,
    /// not empty.
    pub value: String,
}

/// One search result. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    /// 1–20 digits, no leading zero.
    pub tvdb_id: String,
    pub kind: Type,
    pub name: Option<String>,
    pub aliases: Vec<String>,
    pub translations: Vec<Text>,
    /// The language of `name`, by the same rule as [`Text::language`].
    pub primary_language: Option<String>,
    /// Years 1–9999 from `year` and `first_air_time`.
    pub years: Vec<u16>,
    pub genres: Vec<String>,
    /// IMDb title ids: `tt` and 7 digits, or 8–10 with no leading zero.
    pub imdb: Vec<String>,
    /// TMDB ids: 1–20 digits, no leading zero.
    pub tmdb: Vec<String>,
    /// Titles, remote ids or genres past the caps were passed over.
    pub truncated: bool,
}

impl Series {
    fn new(tvdb_id: String, kind: Type) -> Series {
        Series {
            tvdb_id,
            kind,
            name: None,
            aliases: Vec::new(),
            translations: Vec::new(),
            primary_language: None,
            years: Vec::new(),
            genres: Vec::new(),
            imdb: Vec::new(),
            tmdb: Vec::new(),
            truncated: false,
        }
    }

    /// Every title: the name, the aliases and the translated names.
    pub fn titles(&self) -> impl Iterator<Item = &str> {
        self.name
            .as_deref()
            .into_iter()
            .chain(self.aliases.iter().map(String::as_str))
            .chain(self.translations.iter().map(|t| t.value.as_str()))
    }
}

/// Results skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `tvdb_id`, or one that isn't a number.
    pub bad_id: u32,
    /// A string or array over the caps inside it.
    pub over_cap: u32,
    /// Anything outside the shape the parser reads.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.bad_id + self.over_cap + self.malformed
    }
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub kept: u32,
    pub skipped: Skipped,
    /// The first fault.
    pub stop: Option<Stop>,
    /// The root's `status` was read and is `success`.
    pub success: bool,
    /// The root's `status` was read and isn't `success` (an API error).
    pub failed: bool,
    /// The root's `data` key, of the type asked for, was read.
    pub seen: bool,
    /// A root key was written twice or with an escape, or `data` held
    /// something other than what was asked for.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// The answer was read to its end, is the shape asked for, says
    /// `success` and skipped nothing. Only such an answer may be used, as
    /// a match or as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none()
            && !self.trailing
            && self.success
            && !self.failed
            && self.seen
            && !self.bad_root
            && self.skipped.total() == 0
    }
}

/// A search answer's results, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub results: Vec<Series>,
    pub page: Page,
}

/// A login answer's token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Login {
    pub token: Option<Token>,
    pub page: Page,
}

/// Parses a `GET /v4/search` answer.
pub fn parse_search(body: &[u8], limits: &Limits) -> Search {
    let mut results = Vec::new();
    let page = run(body, limits, Root::Search, |p| {
        // No result may come as `null`.
        if p.j.peek() == Some(b'n') {
            return p.j.skip().map(drop);
        }
        let shape = p.arr(|p, _| {
            let r = p.result()?;
            results.extend(r);
            Ok(())
        })?;
        if shape == Shape::OverLong {
            p.page.stop.get_or_insert(Stop::Items);
        }
        Ok(())
    });
    Search { results, page }
}

/// Parses a `POST /v4/login` answer.
pub fn parse_login(body: &[u8], limits: &Limits) -> Login {
    let mut token = None;
    let page = run(body, limits, Root::Login, |p| {
        let (mut s, mut keys, mut raw) = (Spoil::default(), Keys::default(), None);
        p.object(|p, k| {
            Parser::key(&mut s, &mut keys, k, &["token"]);
            match k {
                "token" => raw = p.string(&mut s)?,
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        token = raw.and_then(Token::parse);
        s.check(token.is_some());
        if !p.tally(&s) {
            token = None;
        }
        Ok(())
    });
    Login { token, page }
}

/// The root key `data`'s expected type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    Search,
    Login,
}

/// The root object, handing the value of `data` to `read`.
fn run(
    body: &[u8],
    limits: &Limits,
    want: Root,
    mut read: impl FnMut(&mut Parser<'_, '_>) -> R<()>,
) -> Page {
    let mut summary = crate::common::Summary::default();
    let bytes = crate::common::Limits {
        max_bytes: limits.max_bytes,
        ..crate::common::LIMITS
    };
    let text = prepare(body, &bytes, &mut summary);
    let reader = crate::json::Limits {
        max_depth: limits.max_depth,
        max_values: limits.max_values,
        max_array: limits.max_array,
        max_text: limits.max_text,
    };
    let mut p = Parser {
        j: Reader::new(text, reader),
        l: limits,
        items: 0,
        page: Page {
            stop: summary.stop,
            ..Page::default()
        },
    };
    let fits = |b: Option<u8>| match want {
        Root::Search => matches!(b, Some(b'[' | b'n')),
        Root::Login => b == Some(b'{'),
    };
    let mut read_all = |p: &mut Parser<'_, '_>| -> R<()> {
        if p.j.peek() != Some(b'{') {
            p.j.skip()?;
        } else {
            let mut keys = Keys::default();
            p.object(|p, k| {
                // Unknown root keys are passed over; their repeats don't
                // matter.
                if keys.repeat(k, &["status", "data"]) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                match k {
                    "status" => {
                        let mut s = Spoil::default();
                        match p.string(&mut s)?.as_deref() {
                            Some("success") if s.0.is_none() => p.page.success = true,
                            _ => p.page.failed = true,
                        }
                        Ok(())
                    }
                    "data" if fits(p.j.peek()) => {
                        p.page.seen = true;
                        read(p)
                    }
                    "data" => {
                        p.page.bad_root = true;
                        p.j.skip().map(drop)
                    }
                    _ => p.j.skip().map(drop),
                }
            })?;
        }
        let end = p.j.end();
        p.page.trailing = end.is_err();
        end
    };
    if let Err(stop) = read_all(&mut p) {
        p.page.stop.get_or_insert(stop);
    }
    p.page
}

struct Parser<'a, 'l> {
    j: Reader<'a>,
    l: &'l Limits,
    items: usize,
    page: Page,
}

impl<'a> Walk<'a> for Parser<'a, '_> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

/// The part being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
    }

    /// Spoils the part as malformed unless `shaped`.
    fn check(&mut self, shaped: bool) {
        if !shaped {
            self.set(Bad::Malformed);
        }
    }

    /// Spoils the part unless the container was there, and in its caps.
    fn shape(&mut self, shape: Shape) {
        match shape {
            Shape::Fine => {}
            Shape::OverLong => self.set(Bad::OverCap),
            Shape::Wrong => self.set(Bad::Malformed),
        }
    }
}

/// The keys of one object a parser reads, as a bitmask over their place
/// in a short list. Constant work a key, whatever the object holds.
#[derive(Debug, Default)]
struct Keys(u32);

impl Keys {
    /// True when `k` is escaped, or is one of `known` and was seen before.
    fn repeat(&mut self, k: &str, known: &[&str]) -> bool {
        if k.contains('\\') {
            return true;
        }
        let Some(i) = known.iter().position(|w| *w == k) else {
            return false;
        };
        let bit = 1u32 << i;
        let again = self.0 & bit != 0;
        self.0 |= bit;
        again
    }
}

/// How a container that must be next turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Fine,
    /// An array with more than [`Limits::max_array`] elements; those past
    /// it were passed over.
    OverLong,
    /// Not the container asked for (nor `null`); read past.
    Wrong,
}

/// The result keys read, in a fixed order (for repeat detection).
const RESULT_KEYS: &[&str] = &[
    "tvdb_id",
    "type",
    "name",
    "aliases",
    "translations",
    "primary_language",
    "year",
    "first_air_time",
    "genres",
    "remote_ids",
];

impl<'a> Parser<'a, '_> {
    /// Counts a result; past the cap it's read past and the parse stops
    /// with [`Stop::Items`] once the document ends.
    fn counted(&mut self) -> bool {
        self.items += 1;
        if self.items > self.l.max_items {
            self.page.stop.get_or_insert(Stop::Items);
            return false;
        }
        true
    }

    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// A key of a part being read: an escaped key, or a repeat of one of
    /// `known`, spoils it.
    fn key(s: &mut Spoil, keys: &mut Keys, k: &str, known: &[&str]) {
        if keys.repeat(k, known) {
            s.set(Bad::Malformed);
        }
    }

    /// A string; anything else spoils `s` and is skipped.
    fn string(&mut self, s: &mut Spoil) -> R<Option<String>> {
        if self.j.peek() != Some(b'"') {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        Ok(match self.j.text()? {
            Ok(t) => Some(t),
            Err(TextError::OverCap) => {
                s.set(Bad::OverCap);
                None
            }
            Err(TextError::Malformed) => {
                s.set(Bad::Malformed);
                None
            }
        })
    }

    /// A string or `null` (`None`); anything else spoils `s`.
    fn opt_string(&mut self, s: &mut Spoil) -> R<Option<String>> {
        if self.j.peek() == Some(b'n') {
            return self.j.skip().map(|_| None);
        }
        self.string(s)
    }

    /// `null` is next: read past, and true.
    fn null(&mut self) -> R<bool> {
        if self.j.peek() != Some(b'n') {
            return Ok(false);
        }
        self.j.skip().map(|_| true)
    }

    /// An array (or `null`, as empty) that must be next, handing `f` its
    /// first [`Limits::max_array`] elements and passing over the rest.
    fn arr(&mut self, mut f: impl FnMut(&mut Self, usize) -> R<()>) -> R<Shape> {
        if self.null()? {
            return Ok(Shape::Fine);
        }
        if self.j.peek() != Some(b'[') {
            return self.j.skip().map(|_| Shape::Wrong);
        }
        let (max, mut over) = (self.l.max_array, false);
        self.array(|p, n| {
            if n >= max {
                over = true;
                return p.j.skip().map(drop);
            }
            f(p, n)
        })?;
        Ok(if over { Shape::OverLong } else { Shape::Fine })
    }

    /// An object (or `null`, as empty) that must be next.
    fn obj(&mut self, f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<Shape> {
        if self.null()? {
            return Ok(Shape::Fine);
        }
        if self.j.peek() != Some(b'{') {
            return self.j.skip().map(|_| Shape::Wrong);
        }
        self.object(f).map(|()| Shape::Fine)
    }

    fn tally(&mut self, s: &Spoil) -> bool {
        let k = &mut self.page.skipped;
        match s.0 {
            None => return true,
            Some(Bad::OverCap) => k.over_cap = k.over_cap.saturating_add(1),
            Some(Bad::Malformed) => k.malformed = k.malformed.saturating_add(1),
        }
        false
    }

    /// One search result.
    fn result(&mut self) -> R<Option<Series>> {
        if !self.counted() {
            return self.j.skip().map(|_| None);
        }
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut id, mut kind) = (None, None);
        // Filled in place, given its id at the end.
        let mut e = Series::new(String::new(), Type::Other);
        let mut texts = 0usize;
        let mut ids = 0usize;
        let shape = self.obj(|p, k| {
            Self::key(&mut s, &mut keys, k, RESULT_KEYS);
            let shape = match k {
                "tvdb_id" => {
                    id = p.string(&mut s)?;
                    Shape::Fine
                }
                "type" => {
                    kind = p.string(&mut s)?;
                    Shape::Fine
                }
                "name" if texts >= p.l.max_texts => {
                    e.truncated = true;
                    p.skip_in(&mut s)?;
                    Shape::Fine
                }
                "name" => {
                    if let Some(t) = p.opt_string(&mut s)?.as_deref().and_then(title) {
                        texts += 1;
                        e.name = Some(t);
                    }
                    Shape::Fine
                }
                "aliases" => p.arr(|p, _| {
                    if texts >= p.l.max_texts {
                        e.truncated = true;
                        return p.skip_in(&mut s);
                    }
                    if let Some(t) = p.string(&mut s)?.as_deref().and_then(title) {
                        texts += 1;
                        e.aliases.push(t);
                    }
                    Ok(())
                })?,
                "translations" => {
                    // The languages read, at most `max_texts`: past it the
                    // rest are passed over unchecked.
                    let mut langs = 0usize;
                    p.obj(|p, lang| {
                        if langs >= p.l.max_texts || texts >= p.l.max_texts {
                            e.truncated = true;
                            return p.skip_in(&mut s);
                        }
                        langs += 1;
                        if !language_ok(lang)
                            || e.translations
                                .iter()
                                .any(|t| t.language.eq_ignore_ascii_case(lang))
                        {
                            s.set(Bad::Malformed);
                        }
                        if let Some(v) = p.opt_string(&mut s)?.as_deref().and_then(title) {
                            texts += 1;
                            e.translations.push(Text {
                                language: lang.to_ascii_lowercase(),
                                value: v,
                            });
                        }
                        Ok(())
                    })?
                }
                "primary_language" => {
                    if let Some(l) = p.opt_string(&mut s)?.filter(|l| !l.is_empty()) {
                        s.check(language_ok(&l));
                        e.primary_language = Some(l.to_ascii_lowercase());
                    }
                    Shape::Fine
                }
                "year" => {
                    if let Some(y) = p.opt_string(&mut s)?.filter(|y| !y.is_empty()) {
                        let year = year_of(&y);
                        s.check(year.is_some());
                        e.years.extend(year.flatten());
                    }
                    Shape::Fine
                }
                "first_air_time" => {
                    if let Some(d) = p.opt_string(&mut s)?.filter(|d| !d.is_empty()) {
                        let year = date_year(&d);
                        s.check(year.is_some());
                        e.years.extend(year.flatten());
                    }
                    Shape::Fine
                }
                "genres" => p.arr(|p, _| {
                    if e.genres.len() >= p.l.max_genres {
                        e.truncated = true;
                        return p.skip_in(&mut s);
                    }
                    if let Some(g) = p.string(&mut s)?.as_deref().and_then(title) {
                        e.genres.push(g);
                    }
                    Ok(())
                })?,
                "remote_ids" => p.arr(|p, _| {
                    if ids >= p.l.max_ids {
                        e.truncated = true;
                        return p.skip_in(&mut s);
                    }
                    ids += 1;
                    p.remote(&mut s, &mut e)
                })?,
                _ => p.skip_in(&mut s).map(|()| Shape::Fine)?,
            };
            s.shape(shape);
            Ok(())
        })?;
        s.shape(shape);
        if !self.tally(&s) {
            return Ok(None);
        }
        let Some(id) = id.filter(|i| digits(i)) else {
            self.page.skipped.bad_id = self.page.skipped.bad_id.saturating_add(1);
            return Ok(None);
        };
        e.tvdb_id = id;
        e.kind = match kind.as_deref() {
            Some("series") => Type::Series,
            Some("movie") => Type::Movie,
            _ => Type::Other,
        };
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(e))
    }

    /// `{"id": text, "type": n, "sourceName": text}`: an IMDb title or a
    /// TMDB id is kept, any other source passed over.
    fn remote(&mut self, s: &mut Spoil, e: &mut Series) -> R<()> {
        let (mut keys, mut id, mut source) = (Keys::default(), None, None);
        let shape = self.obj(|p, k| {
            Self::key(s, &mut keys, k, &["id", "sourceName"]);
            match k {
                "id" => id = p.string(s)?,
                "sourceName" => source = p.string(s)?,
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        s.shape(shape);
        match (source.as_deref(), id) {
            (Some("IMDB"), Some(v)) if imdb_ok(&v) => e.imdb.push(v),
            (Some("TheMovieDB.com"), Some(v)) if digits(&v) => e.tmdb.push(v),
            // Another source, or a value of a kind not kept (an IMDb
            // person): passed over.
            (Some(_), Some(_)) => {}
            _ => s.check(false),
        }
        Ok(())
    }
}

/// 1–20 ASCII digits, no leading zero.
fn digits(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

/// An IMDb title id: `tt` and 7 digits, or 8–10 with no leading zero
/// (`lodestar_store::claims::id_ok`'s rule).
fn imdb_ok(s: &str) -> bool {
    s.strip_prefix("tt").is_some_and(|d| {
        d.bytes().all(|b| b.is_ascii_digit())
            && (d.len() == 7 || ((8..=10).contains(&d.len()) && !d.starts_with('0')))
    })
}

/// A language key: 1–[`MAX_LANGUAGE`] bytes of `-`-separated subtags of
/// 1–8 ASCII letters and digits, the first 2–8 letters.
fn language_ok(s: &str) -> bool {
    s.len() <= MAX_LANGUAGE
        && s.split('-').enumerate().all(|(i, t)| {
            let n = t.len();
            if i == 0 {
                (2..=8).contains(&n) && t.bytes().all(|b| b.is_ascii_alphabetic())
            } else {
                (1..=8).contains(&n) && t.bytes().all(|b| b.is_ascii_alphanumeric())
            }
        })
}

/// A `year` field: 4 ASCII digits. `Some(None)` for `0000`; `None` for
/// anything that isn't 4 digits.
fn year_of(s: &str) -> Option<Option<u16>> {
    if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: u16 = s.parse().ok()?;
    Some((y >= 1).then_some(y))
}

/// A `first_air_time` field, `YYYY-MM-DD`: its year.
fn date_year(s: &str) -> Option<Option<u16>> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            _ => c.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    year_of(s.get(..4)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        assert!(digits("81189") && !digits("081189") && !digits("") && !digits("8 1"));
        assert!(!digits(&"1".repeat(21)));
        assert!(imdb_ok("tt0903747") && imdb_ok("tt10872600"));
        for bad in ["nm0000209", "tt090374", "tt", "TT0903747", "tt0903747 "] {
            assert!(!imdb_ok(bad), "{bad}");
        }
        assert!(language_ok("eng") && language_ok("pt-br") && language_ok("zhtw"));
        assert!(!language_ok("e") && !language_ok("en_GB") && !language_ok("1en"));
        assert_eq!(year_of("2008"), Some(Some(2008)));
        assert_eq!(year_of("0000"), Some(None));
        for bad in ["208", "20081", "2o08", "-200", "+200"] {
            assert_eq!(year_of(bad), None, "{bad}");
        }
        assert_eq!(date_year("2008-01-20"), Some(Some(2008)));
        for bad in ["2008-1-20", "2008/01/20", "2008-01-20T00", "20080120"] {
            assert_eq!(date_year(bad), None, "{bad}");
        }
    }

    #[test]
    fn tokens_are_bounded_and_never_printed() {
        let t = Token::parse("eyJhbGciOi.eyJzdWIi.c2ln-_".into()).unwrap();
        assert_eq!(format!("{t:?}"), "Token(len 26)");
        for bad in ["", "a b", "a\r\nX-Evil: 1", "a\"b", "tök"] {
            assert_eq!(Token::parse(bad.into()), None, "{bad:?}");
        }
        assert!(Token::parse("a".repeat(MAX_TOKEN)).is_some());
        assert!(Token::parse("a".repeat(MAX_TOKEN + 1)).is_none());
    }
}
