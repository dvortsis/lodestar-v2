//! ThePornDB REST answers (architecture §20.9, adult enrichment): scene
//! and movie search pages and single lookups (by id or by file hash).
//!
//! The safety rules are [`torznab`](crate::torznab)'s and
//! [`archive_org`](crate::archive_org)'s. It's a pure function from bytes
//! to matches with no I/O. It reads one match at a time with the bounded
//! [`json`](crate::json) reader, stops at a structural cap with a [`Stop`]
//! reason and keeps the matches it finished, and skips (and counts) a
//! match that breaks a field cap or has no usable id. A half-read match
//! is never used. It never panics.
//!
//! The shapes, matched by key in the root object:
//!
//! - a search page (`/scenes`, `/movies`, `/jav`): `{"data": [item, …],
//!   "links": {…}, "meta": {"total": n, …}}`;
//! - a lookup (`/scenes/{id}`, `/scenes/hash/{hash}`, `/movies/{id}`):
//!   `{"data": item}`. A miss is `{"data": []}`, `{"data": null}` or an
//!   error object such as `{"message": "…"}`, which give no match and no
//!   stop.
//!
//! **What it keeps is only what the enrichment claims can store** (plan
//! `docs/plan/2026-11-04-v06-rulepacks.md`, ENR-0): the item's kind, id,
//! title, studio (`site.name`) and release date, and each performer's
//! name. A performer's id isn't kept (owner §2.37, Q4: no per-performer
//! pages or cross-item lists), so nothing here can link a person across
//! items. Everything else is passed over unread: descriptions, images,
//! tags, hashes, URLs and, above all, every personal field of a person.
//! A performer's `extra`/`extras`, `bio` and `parent` objects, where the
//! service puts birthdays, birthplaces, measurements and the like (plan
//! Q5), are skipped without being decoded. Performers come back beside
//! the match, not inside it, so a caller that mustn't keep them (plan Q4)
//! drops them in one step ([`Found::without_performers`]), or never reads
//! them ([`Limits::max_performers`] of 0).

use crate::common::{Bad, clean_title, days_in_month, prepare, small, unsafe_char};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{self, R, Reader, TextError, Walk};

/// Bytes in a kept id: a scene's, movie's or performer's. The service's
/// ids are UUIDs (36 bytes) or integers.
pub const MAX_ID: usize = 64;
/// Bytes in a kept performer or studio name, after cleaning. A longer one
/// is dropped, not cut: a cut name may name someone else.
pub const MAX_NAME: usize = 256;

/// Caps on one parse. The defaults are [`LIMITS`]; why each value:
/// `docs/decisions/2026-11-04-ENR-2b.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Items in `data`, kept or skipped ([`Stop::Items`]).
    pub max_items: usize,
    /// Elements in one array inside an item; over it the item is skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser reads; over it the item is skipped.
    pub max_text: usize,
    /// Performers kept per match; the rest are left out
    /// ([`Performers::truncated`]). 0 reads none.
    pub max_performers: usize,
}

/// The caps for a real answer.
pub const LIMITS: Limits = Limits {
    max_bytes: 8 * 1024 * 1024,
    max_depth: 16,
    max_values: 500_000,
    max_items: 500,
    max_array: 10_000,
    max_text: 64 * 1024,
    max_performers: 64,
};

/// What the service says an item is, from its `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `Scene`.
    Scene,
    /// `Movie`: a full-length release, from the movie endpoints.
    Movie,
    /// `JAV`: a scene from the service's Japanese catalogue.
    Jav,
}

/// One item the service matched. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// `None` when `type` is missing or not one the parser knows. The
    /// service has no VR type or flag; VR is only a tag, which isn't read.
    pub kind: Option<Kind>,
    /// The service's id: 1–[`MAX_ID`] ASCII letters, digits and `-`.
    pub id: String,
    /// At most [`MAX_TITLE`] bytes, hidden characters removed, not empty.
    pub title: Option<String>,
    /// `site.name`: at most [`MAX_NAME`] bytes, hidden characters
    /// removed, not empty.
    pub studio: Option<String>,
    /// `date`, a real calendar date written `YYYY-MM-DD`.
    pub released: Option<String>,
}

/// A performer on a match: the name only. No id, so performers can't be
/// joined across matches (owner §2.37, plan Q4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Performer {
    /// At most [`MAX_NAME`] bytes, hidden characters removed, not empty.
    pub name: String,
}

/// A match's performers, kept apart from it (plan Q4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Performers {
    /// At most [`Limits::max_performers`], in the service's order.
    pub list: Vec<Performer>,
    /// Some were left out: over [`Limits::max_performers`], an entry
    /// without a usable name (a string over [`Limits::max_text`]
    /// or with a lone surrogate included), `performers` being neither
    /// a list nor `null`, or a non-empty list read with
    /// [`Limits::max_performers`] of 0.
    pub truncated: bool,
}

/// A kept match and, separately, its performers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub matched: Match,
    pub performers: Performers,
}

impl Found {
    /// The match alone, its performers dropped.
    pub fn without_performers(self) -> Match {
        self.matched
    }
}

/// Items skipped, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `id`.
    pub no_id: u32,
    /// An `id` that isn't a valid id.
    pub bad_id: u32,
    /// A string over [`Limits::max_text`] or an array over
    /// [`Limits::max_array`] inside the item.
    pub over_cap: u32,
    /// An item that isn't an object, or a string with a lone surrogate.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.no_id + self.bad_id + self.over_cap + self.malformed
    }
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub kept: u32,
    pub skipped: Skipped,
    pub stop: Option<Stop>,
    /// A search page's `meta.total`.
    pub total: Option<u64>,
}

/// Every kept match and how the parse went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub found: Vec<Found>,
    pub page: Page,
}

/// Parses `body` with [`LIMITS`].
pub fn parse(body: &[u8]) -> Answer {
    let mut found = Vec::new();
    let page = parse_each(body, &LIMITS, |f| found.push(f));
    Answer { found, page }
}

/// Parses `body`, handing each kept match to `emit` as soon as it ends.
pub fn parse_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Found)) -> Page {
    let mut summary = crate::common::Summary::default();
    let bytes = crate::common::Limits {
        max_bytes: limits.max_bytes,
        ..crate::common::LIMITS
    };
    let text = prepare(body, &bytes, &mut summary);
    let reader = json::Limits {
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
    // Every read propagates its `Stop` with `?`: after one, the reader's
    // cursor sits mid-token and nothing more may be read.
    if let Err(stop) = p.document(&mut emit) {
        p.page.stop.get_or_insert(stop);
    }
    p.page
}

/// The item being read.
#[derive(Debug, Default)]
struct Draft {
    bad: Option<Bad>,
    /// `Some(None)` when present but not a valid id.
    id: Option<Option<String>>,
    kind: Option<Option<Kind>>,
    title: Option<Option<String>>,
    studio: Option<Option<String>>,
    released: Option<Option<String>>,
    performers: Option<Performers>,
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

impl Parser<'_, '_> {
    // --- Values the parser keeps -------------------------------------

    /// Skips the next value inside an item; an over-long array spoils it.
    fn skip_in(&mut self, d: &mut Draft) -> R<()> {
        if self.j.skip()? {
            d.spoil(Bad::OverCap);
        }
        Ok(())
    }

    /// A string value, decoded, or `None` for anything else (`null`
    /// included), which is skipped. A string over the cap or with a bad
    /// escape spoils the item.
    fn text(&mut self, d: &mut Draft) -> R<Option<String>> {
        if self.j.peek() != Some(b'"') {
            return self.skip_in(d).map(|()| None);
        }
        match self.j.text()? {
            Ok(s) => Ok(Some(s)),
            Err(e) => {
                d.spoil(match e {
                    TextError::OverCap => Bad::OverCap,
                    TextError::Malformed => Bad::Malformed,
                });
                Ok(None)
            }
        }
    }

    /// A field that must be a string or `null`: `null` is `None`; any
    /// other value spoils `d` (malformed) and is skipped.
    fn strict(&mut self, d: &mut Draft) -> R<Option<String>> {
        match self.j.peek() {
            Some(b'"') => self.text(d),
            Some(b'n') => self.j.skip().map(|_| None),
            _ => {
                d.spoil(Bad::Malformed);
                self.skip_in(d).map(|()| None)
            }
        }
    }

    /// `strict`, then `ok` on the string: one it refuses spoils `d`.
    fn checked<T>(&mut self, d: &mut Draft, ok: impl Fn(&str) -> Option<T>) -> R<Option<T>> {
        let Some(s) = self.strict(d)? else {
            return Ok(None);
        };
        let v = ok(&s);
        if v.is_none() && d.bad.is_none() {
            d.spoil(Bad::Malformed);
        }
        Ok(v)
    }

    /// An id: a valid id string, or a non-negative integer as its digits.
    /// `None` inside when the value is anything else.
    fn id(&mut self, d: &mut Draft) -> R<Option<String>> {
        match self.j.peek() {
            Some(b'-' | b'0'..=b'9') => {
                let n = self.j.num()?;
                let ok = n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit());
                Ok(ok.then(|| n.to_owned()))
            }
            _ => Ok(self.text(d)?.filter(|s| id(s))),
        }
    }

    // --- The shapes --------------------------------------------------

    fn document(&mut self, emit: &mut dyn FnMut(Found)) -> R<()> {
        if self.j.peek() == Some(b'{') {
            let (mut data, mut meta) = (false, false);
            self.object(|p, k| match k {
                "data" if !data => {
                    data = true;
                    match p.j.peek() {
                        Some(b'[') => p.array(|p, _| p.item(emit)),
                        Some(b'{') => p.item(emit),
                        _ => p.j.skip().map(drop),
                    }
                }
                "meta" if !meta && p.j.peek() == Some(b'{') => {
                    meta = true;
                    let mut seen = false;
                    p.object(|p, k| match k {
                        "total" if !seen => {
                            seen = true;
                            p.page.total = p.count()?;
                            Ok(())
                        }
                        _ => p.j.skip().map(drop),
                    })
                }
                _ => p.j.skip().map(drop),
            })?;
        } else {
            self.j.skip()?;
        }
        self.j.end()
    }

    /// A count: a JSON number of up to 20 digits that fits a `u64`.
    fn count(&mut self) -> R<Option<u64>> {
        match self.j.peek() {
            Some(b'-' | b'0'..=b'9') => {
                let n = self.j.num()?;
                let ok = n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit());
                Ok(ok.then(|| n.parse().ok()).flatten())
            }
            _ => self.j.skip().map(|_| None),
        }
    }

    /// One element of `data`.
    fn item(&mut self, emit: &mut dyn FnMut(Found)) -> R<()> {
        self.items += 1;
        if self.items > self.l.max_items {
            return Err(Stop::Items);
        }
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            self.page.skipped.malformed = self.page.skipped.malformed.saturating_add(1);
            return Ok(());
        }
        let mut d = Draft::default();
        self.object(|p, k| p.field(&mut d, k))?;
        match d.finish() {
            Ok(f) => {
                self.page.kept = self.page.kept.saturating_add(1);
                emit(f);
            }
            Err(skip) => skip(&mut self.page.skipped),
        }
        Ok(())
    }

    /// A field of an item. The first value of each wins; a repeat is
    /// skipped.
    fn field(&mut self, d: &mut Draft, key: &str) -> R<()> {
        match key {
            "id" if d.id.is_none() => d.id = Some(self.id(d)?),
            // A wrong-typed value, an unknown `type` or a date that isn't
            // a real `YYYY-MM-DD` skips the item: the answer is not what
            // the parser understands, so none of it is trusted (runner).
            "type" if d.kind.is_none() => d.kind = Some(self.checked(d, kind)?),
            "date" if d.released.is_none() => {
                d.released = Some(self.checked(d, |s| date(s).then(|| s.to_owned()))?);
            }
            "title" if d.title.is_none() => {
                // Clean first: a space can sit behind a hidden character.
                let t = self.strict(d)?.map(|t| clean_title(&t).trim().to_owned());
                d.title = Some(t.filter(|t| !t.is_empty()));
            }
            "site" if d.studio.is_none() => {
                // A fault in the name drops only the studio; an over-long
                // array elsewhere in `site` still spoils the item.
                let mut name = None;
                if self.j.peek() == Some(b'{') {
                    self.object(|p, k| match k {
                        "name" if name.is_none() => {
                            let mut own = Draft::default();
                            let n = p.strict(&mut own)?.and_then(|s| clean_name(&s));
                            name = Some(n.filter(|_| own.bad.is_none()));
                            Ok(())
                        }
                        _ => p.skip_in(d),
                    })?;
                } else {
                    self.skip_in(d)?;
                }
                d.studio = Some(name.flatten());
            }
            "performers" if d.performers.is_none() => {
                let mut list = Performers::default();
                match self.j.peek() {
                    // The list's length isn't a fault: `max_values` and
                    // `max_bytes` bound it, and `max_performers` what's kept.
                    Some(b'[') => self.array(|p, _| p.performer(&mut list))?,
                    Some(b'n') => self.j.skip().map(drop)?,
                    _ => {
                        list.truncated = true;
                        self.skip_in(d)?;
                    }
                }
                d.performers = Some(list);
            }
            _ => self.skip_in(d)?,
        }
        Ok(())
    }

    /// One `performers` entry: its `name` only. The rest (`id`, `extra`,
    /// `bio`, `parent`, images) is passed over undecoded.
    ///
    /// Any fault in the entry (a bad `name`, an over-long array or string
    /// anywhere in it, or an entry that isn't an object) drops only that
    /// performer and sets `truncated`, so whether a match is kept doesn't
    /// depend on whether performers are read ([`Limits::max_performers`],
    /// plan Q4). The same rule as ENR-3's StashDB parser (runner, D3).
    fn performer(&mut self, list: &mut Performers) -> R<()> {
        // Every fault in the entry lands here, never on the item.
        let mut own = Draft::default();
        if self.j.peek() != Some(b'{') {
            list.truncated = true;
            return self.skip_in(&mut own);
        }
        let mut name = None;
        // Past the cap, the entry is only read past.
        let keep = list.list.len() < self.l.max_performers;
        self.object(|p, k| {
            match k {
                "name" if keep && name.is_none() => {
                    name = Some(p.strict(&mut own)?.and_then(|s| clean_name(&s)));
                }
                _ => p.skip_in(&mut own)?,
            }
            Ok(())
        })?;
        match (keep && own.bad.is_none(), name.flatten()) {
            (true, Some(name)) => list.list.push(Performer { name }),
            _ => list.truncated = true,
        }
        Ok(())
    }
}

impl Draft {
    fn spoil(&mut self, bad: Bad) {
        self.bad.get_or_insert(bad);
    }

    /// The finished match, or the counter it's skipped under.
    fn finish(self) -> Result<Found, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap = s.over_cap.saturating_add(1)),
            Some(Bad::Malformed) => return Err(|s| s.malformed = s.malformed.saturating_add(1)),
            None => {}
        }
        let id = match self.id {
            None => return Err(|s| s.no_id = s.no_id.saturating_add(1)),
            Some(None) => return Err(|s| s.bad_id = s.bad_id.saturating_add(1)),
            Some(Some(id)) => id,
        };
        Ok(Found {
            matched: Match {
                kind: self.kind.flatten(),
                id,
                title: self.title.flatten(),
                studio: self.studio.flatten(),
                released: self.released.flatten(),
            },
            performers: self.performers.unwrap_or_default(),
        })
    }
}

/// 1–[`MAX_ID`] ASCII letters, digits and `-`.
fn id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn kind(s: &str) -> Option<Kind> {
    let is = |w: &str| s.trim().eq_ignore_ascii_case(w);
    if is("scene") {
        Some(Kind::Scene)
    } else if is("movie") {
        Some(Kind::Movie)
    } else if is("jav") {
        Some(Kind::Jav)
    } else {
        None
    }
}

/// A name with hidden characters removed and trimmed; `None` when that
/// leaves nothing or more than [`MAX_NAME`] bytes.
fn clean_name(s: &str) -> Option<String> {
    let n: String = s.chars().filter(|&c| !unsafe_char(c)).collect();
    let n = n.trim();
    (!n.is_empty() && n.len() <= MAX_NAME).then(|| n.to_owned())
}

/// `YYYY-MM-DD`, a real date in years 1–9999.
fn date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let (Some(y), Some(m), Some(d)) = (
        s.get(..4).and_then(|x| small(x, 4)),
        s.get(5..7).and_then(|x| small(x, 2)),
        s.get(8..).and_then(|x| small(x, 2)),
    ) else {
        return false;
    };
    y >= 1 && (1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        assert!(id("0b9a3c1e-5d7f-4e2a-9c8b-1a2b3c4d5e6f") && id("42"));
        for bad in ["", "a b", "a/b", "a_b", "é", &"a".repeat(MAX_ID + 1)] {
            assert!(!id(bad), "{bad}");
        }
        assert_eq!(kind("Scene"), Some(Kind::Scene));
        assert_eq!(kind(" MOVIE "), Some(Kind::Movie));
        assert_eq!(kind("JAV"), Some(Kind::Jav));
        assert_eq!(kind("VR"), None);
        assert!(date("2024-02-29") && date("1999-12-31"));
        for bad in [
            "2023-02-29",
            "2024-13-01",
            "2024-00-10",
            "0000-01-01",
            "2024-1-01",
            "2024/01/01",
            "2024-01-01T00:00:00Z",
            "+024-01-01",
            "",
        ] {
            assert!(!date(bad), "{bad}");
        }
        assert_eq!(clean_name(" A\u{200B}b "), Some("Ab".into()));
        assert_eq!(clean_name("\u{202E}"), None);
        assert_eq!(clean_name(&"a".repeat(MAX_NAME + 1)), None);
        assert!(clean_name(&"a".repeat(MAX_NAME)).is_some());
    }

    #[test]
    fn without_performers_keeps_the_match() {
        let a = parse(br#"{"data":{"id":"s1","performers":[{"id":"p1","name":"N"}]}}"#);
        let f = a.found.into_iter().next().unwrap();
        assert_eq!(f.performers.list.len(), 1);
        assert_eq!(f.without_performers().id, "s1");
    }
}
