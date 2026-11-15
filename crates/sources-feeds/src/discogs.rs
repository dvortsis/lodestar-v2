//! Discogs database search answers (ENR-discogs; architecture §18.2;
//! `docs/decisions/2026-11-15-ENR-discogs.md`): `GET /database/search`
//! with `type=master`.
//!
//! Wikidata's shape (ENR-14): a pure function from bytes to values, on the
//! bounded [`json`](crate::json) reader, with no I/O and no panics. Every
//! cap is a named field of [`Limits`]. [`Page::whole`] is the one rule a
//! plugin applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The shape: `{"pagination": {…}, "results": [result, …]}`, a result
//! being `{"id": 33876, "type": "master", "master_id": 33876,
//! "title": "Artist - Album", "year": "1998", "genre": […], "style": […],
//! "thumb": url, "cover_image": url, …}`. An error is
//! `{"message": text}` ([`Page::error`]; the service's text is never
//! read).
//!
//! **What it keeps:** the master's id, its title as Discogs prints it
//! (`Artist - Album`) and its year. Images, genres, styles, formats,
//! labels, barcodes, community counts and links are passed over
//! undecoded.
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
}

/// The caps for a search answer: the plugin asks for 5 masters a page.
pub const LIMITS: Limits = Limits {
    max_bytes: 512 * 1024,
    max_depth: 8,
    max_values: 50_000,
    max_array: 1_000,
    max_text: 4 * 1024,
    max_items: 10,
};

/// One search result, a master release. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The master's id: 1 to `u64::MAX`.
    pub id: u64,
    /// `title`, as Discogs prints it (`Artist - Album`): hidden characters
    /// removed, trimmed, at most [`MAX_TITLE`] bytes, not empty.
    pub title: String,
    /// `year`, 1–9999; `"0"`, `0`, `""` and `null` are none.
    pub year: Option<u16>,
}

/// Results skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't a positive integer.
    pub bad_id: u32,
    /// No title, or one empty once cleaned.
    pub no_title: u32,
    /// A string or array over the caps inside it.
    pub over_cap: u32,
    /// Anything outside the shape the parser reads, a `type` other than
    /// `master` among it.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.bad_id + self.no_title + self.over_cap + self.malformed
    }
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub kept: u32,
    pub skipped: Skipped,
    /// The first fault.
    pub stop: Option<Stop>,
    /// The root has a `message` key: an error.
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

/// Parses a `database/search` answer.
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

/// A result's keys read, in a fixed order (for repeat detection).
const RESULT: &[&str] = &["id", "type", "title", "year"];

struct Parser<'a, 'l> {
    j: Reader<'a>,
    l: &'l Limits,
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
            const ROOT: &[&str] = &["results", "message"];
            self.object(|p, k| {
                // Unknown root keys (`pagination`) are passed over.
                if keys.repeat(k, ROOT) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                match k {
                    "results" if p.j.peek() == Some(b'[') => {
                        p.page.seen = true;
                        p.results()
                    }
                    "message" => {
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
        let n = digits(&raw);
        if n.is_none() {
            s.set(Bad::Malformed);
        }
        Ok(n)
    }

    /// `year`: Discogs writes it as a string of digits, and sometimes as a
    /// number; `0`, the empty string and `null` mean not known. Anything
    /// else, or a year past 9999, spoils `s`.
    fn year(&mut self, s: &mut Spoil) -> R<Option<u16>> {
        let raw = match self.j.peek() {
            Some(b'"' | b'n') => match self.string(s, true)? {
                Some(t) if t.is_empty() => return Ok(None),
                Some(t) => digits(&t),
                None => return Ok(None),
            },
            _ => self.integer(s)?,
        };
        match raw.map(u16::try_from) {
            Some(Ok(0)) => Ok(None),
            Some(Ok(y @ 1..=9999)) => Ok(Some(y)),
            _ => {
                s.set(Bad::Malformed);
                Ok(None)
            }
        }
    }

    /// One result: the fields [`RESULT`] names; everything else is passed
    /// over undecoded.
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
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut id, mut name, mut year) = (None::<u64>, None::<String>, None::<u16>);
        self.object(|p, k| {
            if keys.repeat(k, RESULT) {
                s.set(Bad::Malformed);
                return p.skip_in(&mut s);
            }
            match RESULT.iter().position(|w| *w == k) {
                Some(0) => id = p.integer(&mut s)?,
                Some(1) => {
                    // Only masters were asked for.
                    if p.string(&mut s, false)?.is_some_and(|t| t != "master") {
                        s.set(Bad::Malformed);
                    }
                }
                Some(2) => name = p.string(&mut s, false)?.as_deref().and_then(title),
                Some(3) => year = p.year(&mut s)?,
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        let k = &mut self.page.skipped;
        match (s.0, id, name) {
            (Some(Bad::OverCap), _, _) => k.over_cap = k.over_cap.saturating_add(1),
            (Some(Bad::Malformed), _, _) => k.malformed = k.malformed.saturating_add(1),
            (None, None | Some(0), _) => k.bad_id = k.bad_id.saturating_add(1),
            (None, Some(_), None) => k.no_title = k.no_title.saturating_add(1),
            (None, Some(id), Some(title)) => self.hits.push(Hit { id, title, year }),
        }
        Ok(())
    }
}

/// Plain ASCII digits that fit a `u64`; anything else is `None`.
fn digits(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_plain() {
        assert_eq!(digits("1998"), Some(1998));
        for bad in ["", "-1", "+1", "1.0", "1e3", " 1", "18446744073709551616"] {
            assert_eq!(digits(bad), None, "{bad}");
        }
    }
}
