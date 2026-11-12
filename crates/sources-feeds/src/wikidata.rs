//! Wikidata Action API answers (ENR-14; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-12-ENR-14.md`): `wbsearchentities` (items whose
//! label or alias matches a title) and `wbgetentities` (those items'
//! labels, aliases and a few statements).
//!
//! It's the pattern for the v0.7 enrichment parsers: a pure function from
//! bytes to values, on the bounded [`json`](crate::json) reader, with no
//! I/O and no panics. Every cap is a named field of [`Limits`]. It reads
//! only the shape below, and anything else in a part it reads spoils that
//! entity or result, counted in [`Skipped`]. [`Page::whole`] is the one
//! rule a plugin applies: an answer is used only when it was read to its
//! end with nothing skipped, so a broken answer is never mistaken for a
//! miss.
//!
//! The shapes, by root key:
//!
//! - search: `{"search": [{"id": "Q172241", …}, …], …}`. Only each
//!   result's `id` is read; its label, description and URLs are passed
//!   over.
//! - entities: `{"entities": {"Q172241": {"id": "Q172241", "type":
//!   "item", "labels": {lang: {"language": lang, "value": text}},
//!   "aliases": {lang: [{"language": lang, "value": text}, …]}, "claims":
//!   {"P31": [statement, …], …}}}`. An unknown id is `{"id": …,
//!   "missing": ""}`, counted in [`Page::missing`].
//! - an error, on any status: `{"error": {"code": …}, …}` ([`Page::error`];
//!   the service's text is never read).
//!
//! **What it keeps:** ids, labels and aliases (the work's titles), and the
//! statements in [`Property`]: what the item is an instance of, its IMDb,
//! TMDB and TVDB ids, and the year it was published or started. Never a
//! description, a sitelink, a person or any other statement: those are
//! passed over unread. Ids are parsed strictly: an item is `Q` and digits
//! that fit a `u64`; a cross-id must be one of the known kinds (an IMDb
//! title `tt…`, TMDB and TVDB numbers), and any other value of the
//! property, such as an IMDb person id, is not kept.
//!
//! Keys are compared as written. A key with an escape in a part that's
//! read, or a key read twice there, spoils it: a real answer has neither.

use crate::common::{Bad, prepare, title};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{R, Reader, TextError, Walk};

/// Bytes in a language tag (BCP 47's longest registered form fits).
pub const MAX_LANGUAGE: usize = 35;

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array; over it the entity or result is skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the entity or
    /// result is skipped.
    pub max_text: usize,
    /// Search results or entities, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Labels and aliases kept per entity; the rest are passed over
    /// ([`Entity::truncated`]).
    pub max_texts: usize,
    /// Statements of the [`Property`] list decoded per entity; the rest
    /// are passed over ([`Entity::truncated`]).
    pub max_statements: usize,
}

/// The caps for a search answer: a few small results.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    max_depth: 16,
    max_values: 20_000,
    max_array: 1_000,
    max_text: 4 * 1024,
    max_items: 10,
    max_texts: 0,
    max_statements: 0,
};

/// The caps for an entities answer. An item's statements are most of it:
/// a well-known film is a few hundred KiB, so 8 MiB holds several.
pub const ENTITY_LIMITS: Limits = Limits {
    max_bytes: 8 * 1024 * 1024,
    max_depth: 32,
    max_values: 1_000_000,
    max_array: 20_000,
    max_text: 4 * 1024,
    max_items: 5,
    max_texts: 64,
    max_statements: 256,
};

/// An item id, `Q` and its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qid(pub u64);

impl Qid {
    /// `Q` and 1–20 digits with no leading zero that fit a `u64`; nothing
    /// else (no `q`, sign, space or other entity type).
    pub fn parse(s: &str) -> Option<Qid> {
        let d = s.strip_prefix('Q')?;
        if !digits(d) {
            return None;
        }
        d.parse().ok().filter(|&n| n > 0).map(Qid)
    }
}

impl std::fmt::Display for Qid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Q{}", self.0)
    }
}

/// The statements read. Each other property is passed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    /// P31, instance of: an item.
    InstanceOf,
    /// P345, IMDb id: a title `tt…` is kept.
    Imdb,
    /// P4947, TMDB movie id.
    TmdbMovie,
    /// P4983, TMDB TV series id.
    TmdbTv,
    /// P4835, TheTVDB series id: a number is kept.
    Tvdb,
    /// P577, publication date: its year.
    Published,
    /// P580, start time: its year.
    Started,
}

impl Property {
    fn of(key: &str) -> Option<Property> {
        Some(match key {
            "P31" => Property::InstanceOf,
            "P345" => Property::Imdb,
            "P4947" => Property::TmdbMovie,
            "P4983" => Property::TmdbTv,
            "P4835" => Property::Tvdb,
            "P577" => Property::Published,
            "P580" => Property::Started,
            _ => return None,
        })
    }
}

/// A label or alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// Lower case, 1–[`MAX_LANGUAGE`] bytes of ASCII letters, digits and `-`.
    pub language: String,
    /// Hidden characters removed, trimmed, at most [`MAX_TITLE`] bytes,
    /// not empty.
    pub value: String,
}

/// One item. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: Qid,
    pub labels: Vec<Text>,
    pub aliases: Vec<Text>,
    pub instance_of: Vec<Qid>,
    /// IMDb title ids: `tt` and 7 digits, or 8–10 with no leading zero.
    pub imdb: Vec<String>,
    /// TMDB and TVDB ids: 1–20 digits, no leading zero.
    pub tmdb_movie: Vec<String>,
    pub tmdb_tv: Vec<String>,
    pub tvdb: Vec<String>,
    /// Years 1–9999 of a publication date or start time given at least
    /// to the year.
    pub years: Vec<u16>,
    /// Labels, aliases or statements past the caps were passed over.
    pub truncated: bool,
}

impl Entity {
    fn new(id: Qid) -> Entity {
        Entity {
            id,
            labels: Vec::new(),
            aliases: Vec::new(),
            instance_of: Vec::new(),
            imdb: Vec::new(),
            tmdb_movie: Vec::new(),
            tmdb_tv: Vec::new(),
            tvdb: Vec::new(),
            years: Vec::new(),
            truncated: false,
        }
    }
}

/// Entities or results skipped, by reason. Any of them makes the answer
/// not [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't an item's.
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
    /// Entities the service says don't exist.
    pub missing: u32,
    pub skipped: Skipped,
    /// The first fault.
    pub stop: Option<Stop>,
    /// The root has an `error` key.
    pub error: bool,
    /// The root's `search` (an array) or `entities` (an object) key, as
    /// asked, was read.
    pub seen: bool,
    /// A root key was written twice or with an escape.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// The answer was read to its end, is the shape asked for, isn't an
    /// error and skipped nothing. Only such an answer may be used, as a
    /// match or as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none()
            && !self.trailing
            && !self.error
            && self.seen
            && !self.bad_root
            && self.skipped.total() == 0
    }
}

/// A search answer's result ids, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub ids: Vec<Qid>,
    pub page: Page,
}

/// An entities answer's items, in the answer's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entities {
    pub entities: Vec<Entity>,
    pub page: Page,
}

/// Parses a `wbsearchentities` answer.
pub fn parse_search(body: &[u8], limits: &Limits) -> Search {
    let mut ids = Vec::new();
    let page = run(body, limits, "search", |p| {
        p.array(|p, _| {
            let id = p.result()?;
            ids.extend(id);
            Ok(())
        })
    });
    Search { ids, page }
}

/// Parses a `wbgetentities` answer.
pub fn parse_entities(body: &[u8], limits: &Limits) -> Entities {
    let mut entities = Vec::new();
    let page = run(body, limits, "entities", |p| {
        p.object(|p, key| {
            let e = p.entity(key)?;
            entities.extend(e);
            Ok(())
        })
    });
    Entities { entities, page }
}

/// The root object, handing the value of `want` to `read`.
fn run(
    body: &[u8],
    limits: &Limits,
    want: &str,
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
    let shape = if want == "search" { b'[' } else { b'{' };
    let mut read_all = |p: &mut Parser<'_, '_>| -> R<()> {
        if p.j.peek() != Some(b'{') {
            p.j.skip()?;
        } else {
            let mut keys: Vec<&str> = Vec::new();
            p.object(|p, k| {
                if k.contains('\\') || keys.contains(&k) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                keys.push(k);
                match k {
                    _ if k == want && p.j.peek() == Some(shape) => {
                        p.page.seen = true;
                        read(p)
                    }
                    "error" => {
                        p.page.error = true;
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

/// The part being read (a result or an entity): its first fault.
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
}

/// One decoded statement value.
enum Value {
    Item(Qid),
    Str(String),
    /// A time's year, when it's given at least to the year.
    Year(Option<u16>),
}

impl<'a> Parser<'a, '_> {
    /// Counts an item; past the cap it's read past and the parse stops
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

    /// A key of a part being read: an escaped or repeated one spoils it.
    fn key(&self, s: &mut Spoil, seen: &mut Vec<&'a str>, k: &'a str) {
        if k.contains('\\') || seen.contains(&k) {
            s.set(Bad::Malformed);
        }
        seen.push(k);
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

    /// An object that must be next. False when it isn't: it's then read
    /// past, and the caller spoils its part.
    fn obj(&mut self, f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<bool> {
        if self.j.peek() != Some(b'{') {
            return self.j.skip().map(|_| false);
        }
        self.object(f).map(|()| true)
    }

    /// An array that must be next, as [`obj`](Self::obj).
    fn arr(&mut self, f: impl FnMut(&mut Self, usize) -> R<()>) -> R<bool> {
        if self.j.peek() != Some(b'[') {
            return self.j.skip().map(|_| false);
        }
        self.array(f).map(|()| true)
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

    fn bad_id(&mut self) {
        self.page.skipped.bad_id = self.page.skipped.bad_id.saturating_add(1);
    }

    // --- Search ----------------------------------------------------------

    /// One search result: its `id` only.
    fn result(&mut self) -> R<Option<Qid>> {
        if !self.counted() {
            return self.j.skip().map(|_| None);
        }
        let (mut s, mut seen, mut id) = (Spoil::default(), Vec::new(), None);
        let ok = self.obj(|p, k| {
            p.key(&mut s, &mut seen, k);
            match k {
                "id" => id = p.string(&mut s)?,
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        if !ok {
            s.set(Bad::Malformed);
        }
        if !self.tally(&s) {
            return Ok(None);
        }
        let q = id.as_deref().and_then(Qid::parse);
        if q.is_none() {
            self.bad_id();
        } else {
            self.page.kept = self.page.kept.saturating_add(1);
        }
        Ok(q)
    }

    // --- Entities --------------------------------------------------------

    /// One entity under its key.
    fn entity(&mut self, key: &'a str) -> R<Option<Entity>> {
        if !self.counted() {
            return self.j.skip().map(|_| None);
        }
        let mut s = Spoil::default();
        let Some(qid) = Qid::parse(key) else {
            self.skip_in(&mut s)?;
            self.bad_id();
            return Ok(None);
        };
        let mut e = Entity::new(qid);
        let (mut seen, mut id, mut missing, mut statements) = (Vec::new(), None, false, 0);
        let ok = self.obj(|p, k| {
            p.key(&mut s, &mut seen, k);
            let shaped = match k {
                "id" => {
                    id = p.string(&mut s)?;
                    true
                }
                "type" => p.string(&mut s)?.is_some_and(|t| t == "item"),
                "missing" => {
                    missing = true;
                    p.skip_in(&mut s).map(|()| true)?
                }
                "labels" => p.obj(|p, lang| {
                    let t = p.text(&mut s, lang)?;
                    p.keep(&mut e, t, true);
                    Ok(())
                })?,
                "aliases" => p.obj(|p, lang| {
                    let ok = p.arr(|p, _| {
                        let t = p.text(&mut s, lang)?;
                        p.keep(&mut e, t, false);
                        Ok(())
                    })?;
                    s.check(ok);
                    Ok(())
                })?,
                "claims" => p.obj(|p, prop| {
                    let Some(which) = Property::of(prop) else {
                        return p.skip_in(&mut s);
                    };
                    let ok = p.arr(|p, _| {
                        if statements >= p.l.max_statements {
                            e.truncated = true;
                            return p.skip_in(&mut s);
                        }
                        statements += 1;
                        if let Some(v) = p.statement(&mut s, prop, which)? {
                            add(&mut e, which, v);
                        }
                        Ok(())
                    })?;
                    s.check(ok);
                    Ok(())
                })?,
                _ => p.skip_in(&mut s).map(|()| true)?,
            };
            s.check(shaped);
            Ok(())
        })?;
        s.check(ok && id.as_deref() == Some(key));
        if !self.tally(&s) {
            return Ok(None);
        }
        if missing {
            self.page.missing = self.page.missing.saturating_add(1);
            return Ok(None);
        }
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(e))
    }

    /// Keeps a label or alias within the cap.
    fn keep(&self, e: &mut Entity, t: Option<Text>, label: bool) {
        let Some(t) = t else { return };
        if e.labels.len() + e.aliases.len() >= self.l.max_texts {
            e.truncated = true;
        } else if label {
            e.labels.push(t);
        } else {
            e.aliases.push(t);
        }
    }

    /// `{"language": lang, "value": text}` under the key `lang`.
    fn text(&mut self, s: &mut Spoil, lang: &'a str) -> R<Option<Text>> {
        let (mut seen, mut language, mut value) = (Vec::new(), None, None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "language" => language = p.string(s)?,
                "value" => value = p.string(s)?,
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        s.check(ok && language.as_deref() == Some(lang) && language_ok(lang));
        let (None, Some(v)) = (s.0, value) else {
            s.check(false);
            return Ok(None);
        };
        // A value of only hidden characters keeps nothing.
        Ok(title(&v).map(|value| Text {
            language: lang.to_ascii_lowercase(),
            value,
        }))
    }

    /// One statement of `prop`: its value, unless it's deprecated or says
    /// there's no (or an unknown) value.
    fn statement(&mut self, s: &mut Spoil, prop: &str, which: Property) -> R<Option<Value>> {
        let (mut seen, mut rank, mut snak) = (Vec::new(), None, None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "rank" => rank = p.string(s)?,
                "mainsnak" => snak = Some(p.snak(s, prop, which)?),
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        let kept = match rank.as_deref() {
            Some("preferred" | "normal") => true,
            Some("deprecated") => false,
            _ => {
                s.check(false);
                false
            }
        };
        s.check(ok && snak.is_some());
        Ok(snak.flatten().filter(|_| kept))
    }

    /// A main snak: `snaktype`, `property` (which must be `prop`) and, for
    /// a value, its `datavalue`.
    fn snak(&mut self, s: &mut Spoil, prop: &str, which: Property) -> R<Option<Value>> {
        let (mut seen, mut kind, mut property, mut value) = (Vec::new(), None, None, None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "snaktype" => kind = p.string(s)?,
                "property" => property = p.string(s)?,
                "datavalue" => value = Some(p.datavalue(s, which)?),
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        s.check(ok && property.as_deref() == Some(prop));
        match (kind.as_deref(), value) {
            (Some("value"), Some(v)) => Ok(v),
            (Some("novalue" | "somevalue"), None) => Ok(None),
            _ => {
                s.check(false);
                Ok(None)
            }
        }
    }

    /// `{"value": …, "type": …}`, the type the property's values have.
    fn datavalue(&mut self, s: &mut Spoil, which: Property) -> R<Option<Value>> {
        let want = match which {
            Property::InstanceOf => "wikibase-entityid",
            Property::Published | Property::Started => "time",
            _ => "string",
        };
        let (mut seen, mut kind, mut value) = (Vec::new(), None, None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "type" => kind = p.string(s)?,
                "value" => {
                    value = match which {
                        Property::InstanceOf => p.item(s)?.map(Value::Item),
                        Property::Published | Property::Started => p.time(s)?.map(Value::Year),
                        _ => p.string(s)?.map(Value::Str),
                    }
                }
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        s.check(ok && kind.as_deref() == Some(want) && value.is_some());
        Ok(value)
    }

    /// An item value: `{"id": "Q…", …}`.
    fn item(&mut self, s: &mut Spoil) -> R<Option<Qid>> {
        let (mut seen, mut id) = (Vec::new(), None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "id" => id = p.string(s)?,
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        let q = id.as_deref().and_then(Qid::parse);
        s.check(ok && q.is_some());
        Ok(q)
    }

    /// A time value: `{"time": "+YYYY-MM-DDT00:00:00Z", "precision": n,
    /// …}`. `Some(None)` for a year it can't give: before year 1, after
    /// 9999, or a precision coarser than a year.
    fn time(&mut self, s: &mut Spoil) -> R<Option<Option<u16>>> {
        let (mut seen, mut time, mut precision) = (Vec::new(), None, None);
        let ok = self.obj(|p, k| {
            p.key(s, &mut seen, k);
            match k {
                "time" => time = p.string(s)?,
                "precision" if matches!(p.j.peek(), Some(b'0'..=b'9')) => {
                    precision = p.j.num()?.parse::<u8>().ok().filter(|&n| n <= 14);
                }
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        let year = time.as_deref().and_then(year_of);
        match (ok, year, precision) {
            (true, Some(year), Some(precision)) => Ok(Some(year.filter(|_| precision >= 9))),
            _ => {
                s.check(false);
                Ok(None)
            }
        }
    }
}

fn add(e: &mut Entity, which: Property, v: Value) {
    match (which, v) {
        (Property::InstanceOf, Value::Item(q)) => e.instance_of.push(q),
        (Property::Imdb, Value::Str(s)) if imdb_ok(&s) => e.imdb.push(s),
        (Property::TmdbMovie, Value::Str(s)) if digits(&s) => e.tmdb_movie.push(s),
        (Property::TmdbTv, Value::Str(s)) if digits(&s) => e.tmdb_tv.push(s),
        (Property::Tvdb, Value::Str(s)) if digits(&s) => e.tvdb.push(s),
        (Property::Published | Property::Started, Value::Year(Some(y))) => e.years.push(y),
        _ => {}
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

/// The year of a Wikibase time, `±Y…Y-MM-DDThh:mm:ssZ`: `Some(None)` is
/// well-formed but outside 1–9999 (a negative year, or a large one);
/// `None` isn't a Wikibase time at all.
fn year_of(t: &str) -> Option<Option<u16>> {
    let (negative, rest) = match t.as_bytes().first()? {
        b'+' => (false, &t[1..]),
        b'-' => (true, &t[1..]),
        _ => return None,
    };
    let (y, tail) = rest.split_once('-')?;
    if !(1..=16).contains(&y.len()) || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let shape = tail.as_bytes();
    let form = b"00-00T00:00:00Z";
    if shape.len() != form.len()
        || !shape.iter().zip(form).all(|(&c, &f)| {
            if f == b'0' {
                c.is_ascii_digit()
            } else {
                c == f
            }
        })
    {
        return None;
    }
    let n: u64 = y.parse().ok()?;
    Some(
        u16::try_from(n)
            .ok()
            .filter(|y| !negative && (1..=9999).contains(y)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        assert_eq!(Qid::parse("Q172241"), Some(Qid(172_241)));
        assert_eq!(Qid::parse(&format!("Q{}", u64::MAX)), Some(Qid(u64::MAX)));
        for bad in [
            "",
            "Q",
            "Q0",
            "Q01",
            "q1",
            "P31",
            "L1",
            "Q-1",
            "Q+1",
            " Q1",
            "Q1 ",
            "Q１",
            "Q18446744073709551616",
            "Q99999999999999999999",
        ] {
            assert_eq!(Qid::parse(bad), None, "{bad}");
        }
        assert_eq!(Qid(42).to_string(), "Q42");
        assert!(imdb_ok("tt0111161") && imdb_ok("tt10872600"));
        for bad in [
            "nm0000209",
            "tt011116",
            "tt01111610",
            "tt",
            "TT0111161",
            "tt0111161 ",
        ] {
            assert!(!imdb_ok(bad), "{bad}");
        }
        assert!(digits("278") && !digits("0278") && !digits("") && !digits("2 78"));
        assert!(language_ok("en") && language_ok("zh-hant") && language_ok("be-tarask"));
        assert!(language_ok("mul") && !language_ok("e") && !language_ok("en_GB"));
        assert!(!language_ok("1en") && !language_ok(&"a".repeat(40)));
        assert_eq!(year_of("+1994-09-10T00:00:00Z"), Some(Some(1994)));
        assert_eq!(year_of("+2008-00-00T00:00:00Z"), Some(Some(2008)));
        assert_eq!(year_of("-0500-00-00T00:00:00Z"), Some(None));
        assert_eq!(year_of("+0000-00-00T00:00:00Z"), Some(None));
        assert_eq!(year_of("+10000-00-00T00:00:00Z"), Some(None));
        assert_eq!(year_of("+99999999999999999-01-01T00:00:00Z"), None);
        for bad in [
            "1994-09-10",
            "+1994-09-10",
            "+1994-9-10T00:00:00Z",
            "+-09-10T00:00:00Z",
            "",
        ] {
            assert_eq!(year_of(bad), None, "{bad}");
        }
    }
}
