//! PreDB search answers (ENR-predb; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-15-ENR-predb.md`): the scene releases a title
//! search on predb.net's public API found, each with its pre time, section
//! and nuke state.
//!
//! Built like the [`wikidata`](crate::wikidata) parser (ENR-14): a
//! pure function from bytes to values, on the bounded [`json`](crate::json)
//! reader, with no I/O and no panics. Every cap is a named field of
//! [`Limits`]. [`Page::whole`] is the one rule a plugin applies: an answer
//! is used only when it was read to its end with nothing skipped, so a
//! broken answer is never mistaken for a miss.
//!
//! The shape: `{"status": "success", "results": 2, "time": "0.01s",
//! "data": [{"id": 1, "pretime": 1700000000, "release":
//! "Some.Film.2021.1080p.WEB.H264-GRP", "section": "X264-HD", "files": 40,
//! "size": 4200, "status": 0, "reason": "", "group": "GRP", "genre":
//! "Drama", "url": "/rls/…"}, …], "page": 1}`. An empty search has no
//! `data` (and a `message`); an error is `{"status": "error", "message":
//! text}` on a 200 ([`Page::error`]; its text is never read).
//!
//! **What it keeps** ([`PreRelease`]): the release name, pre time,
//! section, nuke state with its reason, and genre. The id, file count,
//! size, group, links and NFO are passed over unread.
//!
//! **The pre claim's shape**, which IRC-3's live feed writes too: a
//! release name ([`release_name`]), its pre time in Unix seconds UTC
//! ([`MIN_PRETIME`]–[`MAX_PRETIME`]), its section ([`section`]: the
//! scene's own category string such as `X264-HD`) and its [`Nuke`] state
//! with the reason the nuke or unnuke gave.
//!
//! Keys are compared as written. In a record, a key with an escape spoils
//! it, and so does a repeat of a key it reads. Repeats are found with a
//! bitmask over the known keys, never a list of every key seen, so a body
//! of a million unknown keys is read in linear time.

use crate::common::{Bad, prepare, title};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{R, Reader, TextError, Walk};

/// Bytes in a kept release name. Scene names are well under it.
pub const MAX_RELEASE: usize = 255;
/// Bytes in a kept section.
pub const MAX_SECTION: usize = 64;
/// Bytes in a kept nuke reason.
pub const MAX_REASON: usize = 255;
/// Bytes in a kept genre.
pub const MAX_GENRE: usize = 64;
/// The earliest pre time kept: 1980-01-01T00:00:00Z. The scene's
/// records start in the 1990s; an earlier one is a placeholder.
pub const MIN_PRETIME: i64 = 315_532_800;
/// The latest pre time kept: 2100-01-01T00:00:00Z.
pub const MAX_PRETIME: i64 = 4_102_444_800;

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array, read or passed over; over it the answer
    /// isn't whole.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the record is
    /// skipped.
    pub max_text: usize,
    /// Records, kept or not ([`Stop::Items`]).
    pub max_items: usize,
}

/// The caps for a page of at most 100 records of a dozen fields each,
/// about 30 KiB in practice.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    max_depth: 6,
    max_values: 5_000,
    max_array: 200,
    max_text: 2 * 1024,
    max_items: 100,
};

/// A release's nuke state, predb.net's numeric `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nuke {
    /// `0`: never nuked.
    Clean,
    /// `1`: nuked (the release broke a rule).
    Nuked,
    /// `2`: unnuked (a nuke was lifted).
    Unnuked,
    /// `3`: deleted by its group after the pre ("delpre").
    Deleted,
    /// A value this parser doesn't know, which a later API may add.
    Unknown,
}

impl Nuke {
    fn parse(n: &str) -> Nuke {
        match n {
            "0" => Nuke::Clean,
            "1" => Nuke::Nuked,
            "2" => Nuke::Unnuked,
            "3" => Nuke::Deleted,
            _ => Nuke::Unknown,
        }
    }
}

/// One pre record. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreRelease {
    /// The release name as the scene wrote it ([`release_name`]).
    pub name: String,
    /// When it was pred, Unix seconds UTC; `None` outside
    /// [`MIN_PRETIME`]–[`MAX_PRETIME`].
    pub pretime: Option<i64>,
    /// Its section ([`section`]).
    pub section: Option<String>,
    pub nuke: Nuke,
    /// Why it was nuked or unnuked: cleaned, at most [`MAX_REASON`]
    /// bytes, kept only when the state isn't [`Nuke::Clean`].
    pub reason: Option<String>,
    /// Its genre, cleaned, at most [`MAX_GENRE`] bytes.
    pub genre: Option<String>,
}

/// A release name: 1–[`MAX_RELEASE`] bytes of printable ASCII with no
/// space, the scene's naming rules. Anything else is `None`.
pub fn release_name(s: &str) -> Option<&str> {
    ((1..=MAX_RELEASE).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())).then_some(s)
}

/// A section: 1–[`MAX_SECTION`] bytes of printable ASCII with no space.
pub fn section(s: &str) -> Option<&str> {
    ((1..=MAX_SECTION).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())).then_some(s)
}

/// Records skipped, by reason. Any of them but `foreign` makes the answer
/// not [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `release` or no `pretime`.
    pub missing: u32,
    /// A string or array over the caps inside it.
    pub over_cap: u32,
    /// Anything outside the shape the parser reads.
    pub malformed: u32,
    /// A well-formed record whose name isn't a [`release_name`]: dropped,
    /// but the answer stays whole, since the service may hold such rows.
    pub foreign: u32,
}

impl Skipped {
    /// The skips that spoil an answer.
    pub fn spoiling(&self) -> u32 {
        self.missing + self.over_cap + self.malformed
    }
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub kept: u32,
    pub skipped: Skipped,
    /// The first fault.
    pub stop: Option<Stop>,
    /// The root's `status` is `"error"`.
    pub error: bool,
    /// The root's `status` is `"success"`.
    pub success: bool,
    /// A root key it reads was written twice, with an escape, held the
    /// wrong type, or `status` was missing or another word.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// The answer was read to its end, is a success and skipped nothing
    /// but foreign rows. Only such an answer may be used, as a match or
    /// as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none()
            && !self.trailing
            && !self.error
            && self.success
            && !self.bad_root
            && self.skipped.spoiling() == 0
    }
}

/// A search answer's records, in the service's order (newest pre first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub releases: Vec<PreRelease>,
    pub page: Page,
}

/// The root keys the parser reads.
const ROOT_KEYS: &[&str] = &["status", "data"];
/// The keys of a record the parser reads.
const RECORD_KEYS: &[&str] = &["release", "pretime", "section", "status", "reason", "genre"];

/// Parses a predb.net search answer.
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
    let mut releases = Vec::new();
    if let Err(stop) = p.root(&mut releases) {
        p.page.stop.get_or_insert(stop);
    }
    if !p.page.success && !p.page.error {
        p.page.bad_root = true;
    }
    Search {
        releases,
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

/// The record being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
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

impl<'a> Parser<'a, '_> {
    fn root(&mut self, releases: &mut Vec<PreRelease>) -> R<()> {
        if self.j.peek() != Some(b'{') {
            self.page.bad_root = true;
            self.j.skip()?;
            return self.j.end();
        }
        let mut keys = Keys::default();
        self.object(|p, k| {
            if keys.repeat(k, ROOT_KEYS) {
                p.page.bad_root = true;
                return p.j.skip().map(drop);
            }
            match k {
                "status" => {
                    if p.j.peek() != Some(b'"') {
                        p.page.bad_root = true;
                        return p.j.skip().map(drop);
                    }
                    match p.j.text()?.as_deref() {
                        Ok("success") => p.page.success = true,
                        Ok("error") => p.page.error = true,
                        _ => p.page.bad_root = true,
                    }
                    Ok(())
                }
                "data" => {
                    if p.j.peek() != Some(b'[') {
                        p.page.bad_root = true;
                        return p.j.skip().map(drop);
                    }
                    let max = p.l.max_array;
                    let mut over = false;
                    p.array(|p, n| {
                        if n >= max {
                            over = true;
                            return p.j.skip().map(drop);
                        }
                        releases.extend(p.record()?);
                        Ok(())
                    })?;
                    if over {
                        p.page.stop.get_or_insert(Stop::Items);
                    }
                    Ok(())
                }
                _ => p.j.skip().map(drop),
            }
        })?;
        let end = self.j.end();
        self.page.trailing = end.is_err();
        end
    }

    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// A string or `null`; anything else spoils `s` and is skipped.
    fn string(&mut self, s: &mut Spoil) -> R<Option<String>> {
        if self.j.peek() == Some(b'n') {
            self.j.skip()?;
            return Ok(None);
        }
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

    /// An integer as written; anything else (including `null`) spoils `s`.
    fn integer(&mut self, s: &mut Spoil) -> R<Option<&'a str>> {
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let n = self.j.num()?;
        if !n
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-'))
        {
            s.set(Bad::Malformed);
            return Ok(None);
        }
        Ok(Some(n))
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

    /// One record.
    fn record(&mut self) -> R<Option<PreRelease>> {
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
        let (mut name, mut pretime, mut seen_time) = (None, None, false);
        let (mut sec, mut nuke, mut reason, mut genre) = (None, Nuke::Clean, None, None);
        self.object(|p, k| {
            if keys.repeat(k, RECORD_KEYS) {
                s.set(Bad::Malformed);
            }
            match k {
                "release" => name = p.string(&mut s)?,
                "pretime" => {
                    seen_time = true;
                    pretime = p
                        .integer(&mut s)?
                        .and_then(|n| n.parse::<i64>().ok())
                        .filter(|t| (MIN_PRETIME..=MAX_PRETIME).contains(t));
                }
                "section" => {
                    sec = p.string(&mut s)?.filter(|t| section(t).is_some());
                }
                "status" => {
                    if let Some(n) = p.integer(&mut s)? {
                        nuke = Nuke::parse(n);
                    }
                }
                "reason" => {
                    reason = p
                        .string(&mut s)?
                        .as_deref()
                        .and_then(title)
                        .map(|t| cut(t, MAX_REASON));
                }
                "genre" => {
                    genre = p
                        .string(&mut s)?
                        .as_deref()
                        .and_then(title)
                        .filter(|t| t.len() <= MAX_GENRE);
                }
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        if !self.tally(&s) {
            return Ok(None);
        }
        let (Some(name), true) = (name, seen_time) else {
            self.page.skipped.missing = self.page.skipped.missing.saturating_add(1);
            return Ok(None);
        };
        if release_name(&name).is_none() {
            self.page.skipped.foreign = self.page.skipped.foreign.saturating_add(1);
            return Ok(None);
        }
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(PreRelease {
            name,
            pretime,
            section: sec,
            reason: reason.filter(|_| nuke != Nuke::Clean),
            nuke,
            genre,
        }))
    }
}

/// `t` cut to at most `max` bytes on a character boundary, trailing
/// whitespace trimmed.
fn cut(mut t: String, max: usize) -> String {
    if t.len() > max {
        let mut end = max;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        t.truncate(end);
    }
    let trimmed = t.trim_end().len();
    t.truncate(trimmed);
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sections_are_printable_ascii_without_spaces() {
        assert_eq!(release_name("A.B-GRP"), Some("A.B-GRP"));
        for bad in ["", "A B", "A\u{7f}", "Ä.B", "A\tB"] {
            assert_eq!(release_name(bad), None, "{bad:?}");
        }
        assert!(release_name(&"a".repeat(MAX_RELEASE)).is_some());
        assert!(release_name(&"a".repeat(MAX_RELEASE + 1)).is_none());
        assert_eq!(section("X264-HD"), Some("X264-HD"));
        assert!(section(&"a".repeat(MAX_SECTION + 1)).is_none());
    }

    #[test]
    fn nuke_states_map_from_the_status_number() {
        assert_eq!(Nuke::parse("0"), Nuke::Clean);
        assert_eq!(Nuke::parse("1"), Nuke::Nuked);
        assert_eq!(Nuke::parse("2"), Nuke::Unnuked);
        assert_eq!(Nuke::parse("3"), Nuke::Deleted);
        assert_eq!(Nuke::parse("4"), Nuke::Unknown);
        assert_eq!(Nuke::parse("-1"), Nuke::Unknown);
    }

    #[test]
    fn cut_keeps_char_boundaries() {
        assert_eq!(cut("ééé".to_owned(), 3), "é");
        assert_eq!(cut("ab  ".to_owned(), 3), "ab");
    }
}
