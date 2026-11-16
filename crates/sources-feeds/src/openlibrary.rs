//! Open Library search answers (ENR-ol; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-15-ENR-ol.md`): `search.json`, the works whose
//! title matches a query.
//!
//! Built like [`wikidata`](crate::wikidata): a pure function from bytes to
//! values, on the bounded [`json`](crate::json) reader, with no I/O and no
//! panics. Every cap is a named field of [`Limits`]. It reads only the
//! shape below, and anything else in a part it reads spoils that work,
//! counted in [`Skipped`]. [`Page::whole`] is the one rule a plugin
//! applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The shape: `{"numFound": n, "start": 0, "docs": [{"key":
//! "/works/OL45883W", "title": text, "alternative_title": [text, …],
//! "first_publish_year": 1818, "publish_year": [1818, 1831, …]}, …], …}`.
//! Every field of a work but `key` may be left out. An error answer has an
//! `error` key at the root ([`Page::error`]; its text is never read).
//!
//! **What it keeps:** the work's id, its title and alternative titles, and
//! its publication years. Never an author, a subject, an edition, an ISBN
//! or any other field: those are passed over unread, even when the
//! service sends them unasked. A work's id is parsed strictly
//! ([`Olid::parse`]): an author's (`OL…A`) or an edition's (`OL…M`) is a
//! bad id.
//!
//! Keys are compared as written. In a part that's read, a key with an
//! escape spoils it, and so does a repeat of a key it reads. Repeats are
//! found with a bitmask over the known keys, never a list of every key
//! seen, so a body of a million unknown keys is read in linear time.

use crate::common::{Bad, prepare, title};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{R, Reader, TextError, Walk};

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array, read or passed over; over it the work is
    /// skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the work is
    /// skipped.
    pub max_text: usize,
    /// Works, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Titles kept per work, its own and its alternatives; the rest are
    /// passed over undecoded ([`Work::truncated`]).
    pub max_texts: usize,
    /// Publication years kept per work; the rest are passed over
    /// ([`Work::truncated`]).
    pub max_years: usize,
}

/// The caps for a search answer of at most ten works with five fields
/// each: a few KiB in practice.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 512 * 1024,
    max_depth: 8,
    max_values: 50_000,
    max_array: 2_000,
    max_text: 4 * 1024,
    max_items: 10,
    max_texts: 64,
    max_years: 256,
};

/// A work's id: `OL`, its number and `W`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Olid(pub u64);

impl Olid {
    /// `/works/OL…W` (the search answer's `key`) or `OL…W`, with 1–20
    /// digits, no leading zero, that fit a `u64`. Nothing else: no author
    /// (`A`) or edition (`M`), no lower case, sign or space.
    pub fn parse(s: &str) -> Option<Olid> {
        let s = s.strip_prefix("/works/").unwrap_or(s);
        let d = s.strip_prefix("OL")?.strip_suffix('W')?;
        if !digits(d) {
            return None;
        }
        d.parse().ok().map(Olid)
    }
}

impl std::fmt::Display for Olid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OL{}W", self.0)
    }
}

/// One work. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    pub id: Olid,
    /// Its `title`, cleaned as [`MAX_TITLE`] says; `None` when it has
    /// none.
    pub title: Option<String>,
    /// Its `alternative_title`s, cleaned the same way, in order.
    pub alternatives: Vec<String>,
    /// `first_publish_year`, when it's 1–9999.
    pub first_year: Option<u16>,
    /// `publish_year`s 1–9999, in order.
    pub years: Vec<u16>,
    /// Titles or years past the caps were passed over.
    pub truncated: bool,
}

/// Works skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `key`, or one that isn't a work's.
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
    /// The root has an `error` key.
    pub error: bool,
    /// The root's `docs` array was read.
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

/// A search answer's works, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub works: Vec<Work>,
    pub page: Page,
}

/// Parses a `search.json` answer.
pub fn parse_search(body: &[u8], limits: &Limits) -> Search {
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
    let mut works = Vec::new();
    let mut read_all = |p: &mut Parser<'_, '_>| -> R<()> {
        if p.j.peek() != Some(b'{') {
            p.j.skip()?;
        } else {
            let mut keys = Keys::default();
            p.object(|p, k| {
                // Unknown root keys are passed over; their repeats don't
                // matter.
                if keys.repeat(k, &["docs", "error"]) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                match k {
                    "docs" if p.j.peek() == Some(b'[') => {
                        p.page.seen = true;
                        let shape = p.arr(|p, _| {
                            works.extend(p.work()?);
                            Ok(())
                        })?;
                        if shape == Shape::OverLong {
                            p.page.stop.get_or_insert(Stop::Items);
                        }
                        Ok(())
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
    Search {
        works,
        page: p.page,
    }
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

/// The work being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
    }

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
    /// Not the container asked for; read past.
    Wrong,
}

/// The keys of a work the parser reads.
const WORK_KEYS: &[&str] = &[
    "key",
    "title",
    "alternative_title",
    "first_publish_year",
    "publish_year",
];

impl<'a> Parser<'a, '_> {
    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
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

    /// A number that's a year: `Some(year)` for an integer 1–9999,
    /// `Some(None)` for any other number, and anything but a number
    /// spoils `s`.
    fn year(&mut self, s: &mut Spoil) -> R<Option<u16>> {
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let n = self.j.num()?;
        Ok(n.parse::<u16>().ok().filter(|y| (1..=9999).contains(y)))
    }

    /// An array that must be next, handing `f` its first
    /// [`Limits::max_array`] elements and passing over the rest.
    fn arr(&mut self, mut f: impl FnMut(&mut Self, usize) -> R<()>) -> R<Shape> {
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

    fn tally(&mut self, s: &Spoil) -> bool {
        let k = &mut self.page.skipped;
        match s.0 {
            None => return true,
            Some(Bad::OverCap) => k.over_cap = k.over_cap.saturating_add(1),
            Some(Bad::Malformed) => k.malformed = k.malformed.saturating_add(1),
        }
        false
    }

    /// One work: its `key`, titles and years.
    fn work(&mut self) -> R<Option<Work>> {
        self.items += 1;
        if self.items > self.l.max_items {
            self.page.stop.get_or_insert(Stop::Items);
            return self.j.skip().map(|_| None);
        }
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            self.page.skipped.malformed = self.page.skipped.malformed.saturating_add(1);
            return Ok(None);
        }
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut key, mut name, mut alternatives) = (None, None, Vec::new());
        let (mut first_year, mut years, mut truncated) = (None, Vec::new(), false);
        // Titles kept so far, the work's own included.
        let mut texts = 0usize;
        self.object(|p, k| {
            if keys.repeat(k, WORK_KEYS) {
                s.set(Bad::Malformed);
            }
            match k {
                "key" => key = p.string(&mut s)?,
                "title" => {
                    if texts >= p.l.max_texts {
                        truncated = true;
                        return p.skip_in(&mut s);
                    }
                    name = p.string(&mut s)?.as_deref().and_then(title);
                    texts += usize::from(name.is_some());
                }
                "alternative_title" => {
                    let shape = p.arr(|p, _| {
                        if texts >= p.l.max_texts {
                            truncated = true;
                            return p.skip_in(&mut s);
                        }
                        if let Some(t) = p.string(&mut s)?.as_deref().and_then(title) {
                            alternatives.push(t);
                            texts += 1;
                        }
                        Ok(())
                    })?;
                    s.shape(shape);
                }
                "first_publish_year" => first_year = p.year(&mut s)?,
                "publish_year" => {
                    let shape = p.arr(|p, _| {
                        if years.len() >= p.l.max_years {
                            truncated = true;
                            return p.skip_in(&mut s);
                        }
                        years.extend(p.year(&mut s)?);
                        Ok(())
                    })?;
                    s.shape(shape);
                }
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        if !self.tally(&s) {
            return Ok(None);
        }
        let Some(id) = key.as_deref().and_then(Olid::parse) else {
            self.page.skipped.bad_id = self.page.skipped.bad_id.saturating_add(1);
            return Ok(None);
        };
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(Work {
            id,
            title: name,
            alternatives,
            first_year,
            years,
            truncated,
        }))
    }
}

/// 1–20 ASCII digits, no leading zero.
fn digits(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_ids_are_strict() {
        assert_eq!(Olid::parse("/works/OL45883W"), Some(Olid(45_883)));
        assert_eq!(Olid::parse("OL45883W"), Some(Olid(45_883)));
        assert_eq!(
            Olid::parse(&format!("OL{}W", u64::MAX)),
            Some(Olid(u64::MAX))
        );
        for bad in [
            "",
            "OLW",
            "OL0W",
            "OL01W",
            "ol1w",
            "OL1A",
            "OL1M",
            "/authors/OL1A",
            "/books/OL1M",
            "/works/OL1M",
            "/works//works/OL1W",
            "works/OL1W",
            " OL1W",
            "OL1W ",
            "OL-1W",
            "OL+1W",
            "OL１W",
            "OL18446744073709551616W",
        ] {
            assert_eq!(Olid::parse(bad), None, "{bad}");
        }
        assert_eq!(Olid(42).to_string(), "OL42W");
    }
}
