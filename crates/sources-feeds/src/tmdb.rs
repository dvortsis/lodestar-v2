//! TMDB search answers (ENR-tmdb; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-15-ENR-tmdb.md`): `GET /3/search/movie` and
//! `GET /3/search/tv`.
//!
//! Wikidata's shape (ENR-14): a pure function from bytes to values, on the
//! bounded [`json`](crate::json) reader, with no I/O and no panics. Every
//! cap is a named field of [`Limits`]. [`Page::whole`] is the one rule a
//! plugin applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The shape: `{"page": 1, "results": [result, …], "total_pages": n,
//! "total_results": n}`, a result being, for a film,
//! `{"id": 278, "title": text, "original_title": text,
//! "original_language": "en", "release_date": "1994-09-23",
//! "genre_ids": [18, 80], "adult": false, …}`, and for a series the same
//! with `name`, `original_name` and `first_air_date` ([`Form`]). An error
//! is `{"status_code": n, "status_message": text, "success": false}`
//! ([`Page::error`]; the service's text is never read).
//!
//! **What it keeps:** the id, the two titles, the original language, the
//! year and the genre ids. The overview, the image paths, popularity and
//! votes are passed over undecoded.
//!
//! Keys are compared as written. In a result, a key with an escape, a
//! repeat of a key it reads, or a read field of the wrong type spoils it.
//! Repeats are found with a bitmask over the known keys, so a body of a
//! million unknown keys is read in linear time.

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
    /// Elements in one array, read or passed over; over it the result is
    /// spoiled.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the result is
    /// spoiled.
    pub max_text: usize,
    /// Results, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Genre ids kept per result; the rest are passed over
    /// ([`Hit::truncated`]).
    pub max_genres: usize,
}

/// The caps for a search answer: TMDB's one page of at most 20 results.
pub const LIMITS: Limits = Limits {
    max_bytes: 512 * 1024,
    max_depth: 8,
    max_values: 50_000,
    max_array: 1_000,
    max_text: 4 * 1024,
    max_items: 20,
    max_genres: 16,
};

/// Bytes in a language code kept (ISO 639-1 is two letters; 639-2 three).
pub const MAX_LANGUAGE: usize = 3;

/// Which search was asked: the field names differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Movie,
    Tv,
}

impl Form {
    /// The result's keys read, in a fixed order (for repeat detection):
    /// id, title, original title, original language, date, genre ids,
    /// adult.
    fn keys(self) -> &'static [&'static str; 7] {
        match self {
            Form::Movie => &[
                "id",
                "title",
                "original_title",
                "original_language",
                "release_date",
                "genre_ids",
                "adult",
            ],
            Form::Tv => &[
                "id",
                "name",
                "original_name",
                "original_language",
                "first_air_date",
                "genre_ids",
                "adult",
            ],
        }
    }
}

/// One search result. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// TMDB's id: 1 to `u64::MAX`.
    pub id: u64,
    /// `title` (`name` for a series): hidden characters removed, trimmed,
    /// at most [`MAX_TITLE`] bytes, not empty.
    pub title: Option<String>,
    /// `original_title` (`original_name`), cleaned the same way.
    pub original_title: Option<String>,
    /// Two or three lower-case ASCII letters; `xx` (no language) isn't kept.
    pub original_language: Option<String>,
    /// The year of `release_date` (`first_air_date`), 1–9999.
    pub year: Option<u16>,
    /// At most [`Limits::max_genres`].
    pub genre_ids: Vec<u32>,
    /// TMDB's adult marker: `Some(true)` marked adult, `Some(false)`
    /// marked not adult, `None` when the result has no `adult` key.
    pub adult: Option<bool>,
    /// Genre ids past the cap were passed over.
    pub truncated: bool,
}

/// Results skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't a positive integer.
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
    /// The root has a `status_code` or `success` key: an error.
    pub error: bool,
    /// The root's `results` array was read.
    pub seen: bool,
    /// A root key it reads was written twice or with an escape.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// Read to its end, the shape asked for, not an error, nothing
    /// skipped. Only such an answer may be used, as a match or as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none()
            && !self.trailing
            && !self.error
            && self.seen
            && !self.bad_root
            && self.skipped.total() == 0
    }
}

/// A search answer's results, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub hits: Vec<Hit>,
    pub page: Page,
}

/// Parses a `search/movie` or `search/tv` answer.
pub fn parse_search(body: &[u8], form: Form, limits: &Limits) -> Search {
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
        form,
        items: 0,
        hits: Vec::new(),
        page: Page {
            stop: summary.stop,
            ..Page::default()
        },
    };
    if let Err(stop) = p.root() {
        p.page.stop.get_or_insert(stop);
    }
    p.page.kept = u32::try_from(p.hits.len()).unwrap_or(u32::MAX);
    Search {
        hits: p.hits,
        page: p.page,
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

/// The result being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
    }
}

struct Parser<'a, 'l> {
    j: Reader<'a>,
    l: &'l Limits,
    form: Form,
    items: usize,
    hits: Vec<Hit>,
    page: Page,
}

impl<'a> Walk<'a> for Parser<'a, '_> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

impl<'a> Parser<'a, '_> {
    fn root(&mut self) -> R<()> {
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
        } else {
            let mut keys = Keys::default();
            const ROOT: &[&str] = &["results", "status_code", "success"];
            self.object(|p, k| {
                // Unknown root keys (`page`, the totals) are passed over.
                if keys.repeat(k, ROOT) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                match k {
                    "results" if p.j.peek() == Some(b'[') => {
                        p.page.seen = true;
                        p.results()
                    }
                    "status_code" | "success" => {
                        p.page.error = true;
                        p.j.skip().map(drop)
                    }
                    _ => p.j.skip().map(drop),
                }
            })?;
        }
        let end = self.j.end();
        self.page.trailing = end.is_err();
        end
    }

    fn results(&mut self) -> R<()> {
        let max = self.l.max_array;
        self.array(|p, n| {
            if n >= max {
                // An over-long list is past the item cap too.
                p.page.stop.get_or_insert(Stop::Items);
                return p.j.skip().map(drop);
            }
            p.result()
        })
    }

    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// A string, or `null` (None, unspoiled, only when `nullable`);
    /// anything else spoils `s` and is skipped.
    fn string(&mut self, s: &mut Spoil, nullable: bool) -> R<Option<String>> {
        match self.j.peek() {
            Some(b'"') => {}
            Some(b'n') if nullable => return self.j.skip().map(|_| None),
            _ => {
                s.set(Bad::Malformed);
                return self.skip_in(s).map(|()| None);
            }
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

    /// A non-negative integer that fits a `u64`: digits only, no sign,
    /// fraction or exponent. Anything else spoils `s`.
    fn integer(&mut self, s: &mut Spoil) -> R<Option<u64>> {
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let raw = self.j.num()?;
        let n = raw
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| raw.parse::<u64>().ok())
            .flatten();
        if n.is_none() {
            s.set(Bad::Malformed);
        }
        Ok(n)
    }

    /// One result: the fields [`Form::keys`] names; everything else is
    /// passed over undecoded.
    fn result(&mut self) -> R<()> {
        self.items += 1;
        if self.items > self.l.max_items {
            self.page.stop.get_or_insert(Stop::Items);
            return self.j.skip().map(drop);
        }
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            self.page.skipped.malformed = self.page.skipped.malformed.saturating_add(1);
            return Ok(());
        }
        let known = self.form.keys();
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let mut id: Option<u64> = None;
        let mut hit = Hit {
            id: 0,
            title: None,
            original_title: None,
            original_language: None,
            year: None,
            genre_ids: Vec::new(),
            adult: None,
            truncated: false,
        };
        self.object(|p, k| {
            if keys.repeat(k, known) {
                s.set(Bad::Malformed);
                return p.skip_in(&mut s);
            }
            match known.iter().position(|w| *w == k) {
                Some(0) => id = p.integer(&mut s)?,
                Some(1) => hit.title = p.string(&mut s, false)?.as_deref().and_then(title),
                Some(2) => hit.original_title = p.string(&mut s, false)?.as_deref().and_then(title),
                Some(3) => {
                    hit.original_language = p.string(&mut s, true)?.filter(|l| language(l));
                }
                Some(4) => {
                    if let Some(d) = p.string(&mut s, true)? {
                        match date_year(&d) {
                            Ok(y) => hit.year = y,
                            Err(()) => s.set(Bad::Malformed),
                        }
                    }
                }
                Some(5) => p.genres(&mut s, &mut hit)?,
                Some(6) => match p.j.peek() {
                    Some(b't') => {
                        hit.adult = Some(true);
                        p.skip_in(&mut s)?;
                    }
                    Some(b'f') => {
                        hit.adult = Some(false);
                        p.skip_in(&mut s)?;
                    }
                    _ => {
                        s.set(Bad::Malformed);
                        p.skip_in(&mut s)?;
                    }
                },
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        let k = &mut self.page.skipped;
        match (s.0, id) {
            (Some(Bad::OverCap), _) => k.over_cap = k.over_cap.saturating_add(1),
            (Some(Bad::Malformed), _) => k.malformed = k.malformed.saturating_add(1),
            (None, None | Some(0)) => k.bad_id = k.bad_id.saturating_add(1),
            (None, Some(n)) => {
                hit.id = n;
                self.hits.push(hit);
            }
        }
        Ok(())
    }

    /// `genre_ids`: an array of integers that fit a `u32`, the first
    /// [`Limits::max_genres`] kept.
    fn genres(&mut self, s: &mut Spoil, hit: &mut Hit) -> R<()> {
        if self.j.peek() != Some(b'[') {
            s.set(Bad::Malformed);
            return self.skip_in(s);
        }
        let (max_array, max_genres) = (self.l.max_array, self.l.max_genres);
        self.array(|p, n| {
            if n >= max_array {
                s.set(Bad::OverCap);
                return p.j.skip().map(drop);
            }
            let g = p.integer(s)?;
            match g.map(u32::try_from) {
                Some(Ok(g)) if hit.genre_ids.len() < max_genres => hit.genre_ids.push(g),
                Some(Ok(_)) => hit.truncated = true,
                Some(Err(_)) => s.set(Bad::Malformed),
                None => {}
            }
            Ok(())
        })
    }
}

/// Two or three lower-case ASCII letters, and not `xx` (TMDB's "no
/// language").
fn language(l: &str) -> bool {
    (2..=MAX_LANGUAGE).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase()) && l != "xx"
}

/// The year of `YYYY-MM-DD`: `Ok(None)` for an empty date, `Err` for
/// anything else that isn't a date of years 1–9999.
fn date_year(d: &str) -> Result<Option<u16>, ()> {
    if d.is_empty() {
        return Ok(None);
    }
    let b = d.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !digits(0..4)
        || !digits(5..7)
        || !digits(8..10)
    {
        return Err(());
    }
    let num = |r: std::ops::Range<usize>| d.get(r).and_then(|x| x.parse::<u16>().ok());
    let (y, m, day) = (num(0..4), num(5..7), num(8..10));
    match (y, m, day) {
        (Some(y), Some(1..=12), Some(1..=31)) if y >= 1 => Ok(Some(y)),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_strict() {
        assert_eq!(date_year(""), Ok(None));
        assert_eq!(date_year("1994-09-23"), Ok(Some(1994)));
        for bad in [
            "1994",
            "0000-01-01",
            "1994-13-01",
            "1994-01-00",
            "+1994-01-01",
            "1994-1-01",
        ] {
            assert_eq!(date_year(bad), Err(()), "{bad}");
        }
    }

    #[test]
    fn languages_are_codes() {
        assert!(language("en") && language("cmn"));
        assert!(!language("xx") && !language("EN") && !language("e") && !language("engl"));
    }
}
