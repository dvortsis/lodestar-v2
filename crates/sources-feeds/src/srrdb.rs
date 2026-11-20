//! srrDB API answers (ENR-srrdb; architecture §18.2, §18.7;
//! `docs/decisions/2026-11-15-ENR-srrdb.md`): a release search
//! (`/v1/search/<word>/<word>/…`) and a release's details
//! (`/v1/details/<release>`).
//!
//! The same pattern as the Wikidata and MusicBrainz parsers: pure
//! functions from bytes to values, on the bounded [`json`](crate::json)
//! reader, with no I/O and no panics. Every cap is a named field of
//! [`Limits`]. Anything outside the shape a parser reads, in a part it
//! reads, spoils that part and is counted in [`Skipped`]. A plugin uses an
//! answer only when [`Page::whole`]: read to its end with nothing
//! skipped, so a broken answer is never mistaken for a miss.
//!
//! The search shape: `{"results": [{"release": text, "imdbId": "0133093"
//! | null, "size": n, …}, …], "resultsCount": n, …}`. The details shape:
//! `{"name": text, "files": [{"name": text, "size": n, "crc": "04523E9B"},
//! …], "archived-files": [same], …}`. An error is `{"error": …}`
//! ([`Page::error`]; its text is never read).
//!
//! **What it keeps:** each result's scene release name, IMDb id and size;
//! a release's name and its stored and archived files' names, sizes and
//! CRC-32s. Never the dates, the NFO/SRS flags or anything else. Keys are
//! compared as written: an escaped key, or a repeat of a key a part
//! reads, spoils that part. Repeats are found with a bitmask over the
//! known keys, so a body of a million unknown keys is read in linear
//! time.

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
    /// Elements in one array; over it the parse stops ([`Stop::Items`]).
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the part is
    /// skipped.
    pub max_text: usize,
    /// Search results, or a release's files of each list, kept or not
    /// ([`Stop::Items`]).
    pub max_items: usize,
}

/// The caps for a search answer: srrDB pages its results (45 a page in
/// the answers seen), each about 200 bytes.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 1024 * 1024,
    max_depth: 8,
    max_values: 100_000,
    max_array: 1_000,
    max_text: 1024,
    max_items: 1_000,
};

/// The caps for a release's details: a release stores a few files (NFO,
/// SFV, samples) and archives a few hundred at most (a multi-disc set).
pub const DETAILS_LIMITS: Limits = Limits {
    max_bytes: 1024 * 1024,
    max_depth: 8,
    max_values: 100_000,
    max_array: 2_000,
    max_text: 1024,
    max_items: 2_000,
};

/// Bytes in a kept release or file name. Scene names are at most 255
/// bytes; a longer one spoils its part.
pub const MAX_RELEASE: usize = 255;

/// An IMDb title id in the store's one spelling: `tt` and seven digits,
/// or eight to ten with no leading zero.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImdbId(String);

impl ImdbId {
    /// srrDB writes the digits alone (`0133093`); `tt0133093` is read
    /// too. Anything else, all zeros among them, isn't one.
    pub fn parse(s: &str) -> Option<ImdbId> {
        let d = s.strip_prefix("tt").unwrap_or(s);
        let ok = d.bytes().all(|b| b.is_ascii_digit())
            && (d.len() == 7 || ((8..=10).contains(&d.len()) && !d.starts_with('0')))
            && d.bytes().any(|b| b != b'0');
        ok.then(|| ImdbId(format!("tt{d}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ImdbId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One search result. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The scene release name, hidden characters removed, at most
    /// [`MAX_RELEASE`] bytes, not empty.
    pub release: String,
    pub imdb: Option<ImdbId>,
    /// The release's size in bytes, as srrDB has it.
    pub size: Option<u64>,
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrrFile {
    /// Cleaned like a release name.
    pub name: String,
    pub size: u64,
    /// The CRC-32 srrDB stores for it.
    pub crc: u32,
}

/// Parts skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// A string over [`Limits::max_text`] or a name over
    /// [`MAX_RELEASE`].
    pub over_cap: u32,
    /// Anything outside the shape the parser reads.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.over_cap + self.malformed
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
    /// The root's list (`results`, or `name` for details) was read.
    pub seen: bool,
    /// A root key read was written twice or with an escape, or isn't the
    /// shape asked for.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// The answer was read to its end, is the shape asked for, isn't an
    /// error and skipped nothing. Only such an answer may be used.
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
    /// The `resultsCount` srrDB reports, when it's a plain count; more
    /// than `hits` when the answer is one page of several.
    pub count: Option<u64>,
    pub page: Page,
}

/// A release's details.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub name: Option<String>,
    /// Files stored in the SRR (NFO, SFV, samples).
    pub files: Vec<SrrFile>,
    /// Files inside the release's archives.
    pub archived: Vec<SrrFile>,
    pub page: Page,
}

/// Parses a search answer.
pub fn parse_search(body: &[u8], limits: &Limits) -> Search {
    let mut out = Search::default();
    let (mut hits, mut count) = (Vec::new(), None);
    let page = run(body, limits, |p, k, keys| {
        if keys.repeat(k, &["results", "resultsCount", "error"]) {
            p.page.bad_root = true;
            return p.j.skip().map(drop);
        }
        match k {
            "results" => {
                p.page.seen = true;
                p.list(|p| {
                    if let Some(h) = p.hit()? {
                        hits.push(h);
                    }
                    Ok(())
                })
            }
            "resultsCount" => {
                if p.j.peek().is_some_and(|b| b.is_ascii_digit()) {
                    count =
                        crate::common::small(p.j.num()?, 18).and_then(|n| u64::try_from(n).ok());
                    Ok(())
                } else {
                    p.j.skip().map(drop)
                }
            }
            "error" => {
                p.page.error = true;
                p.j.skip().map(drop)
            }
            _ => p.j.skip().map(drop),
        }
    });
    out.hits = hits;
    out.count = count;
    out.page = page;
    out
}

/// Parses a release's details.
pub fn parse_details(body: &[u8], limits: &Limits) -> Details {
    let mut out = Details::default();
    let (mut name, mut files, mut archived) = (None, Vec::new(), Vec::new());
    let page = run(body, limits, |p, k, keys| {
        if keys.repeat(k, &["name", "files", "archived-files", "error"]) {
            p.page.bad_root = true;
            return p.j.skip().map(drop);
        }
        match k {
            "name" => {
                p.page.seen = true;
                let mut s = Spoil::default();
                name = p.name(&mut s)?;
                if !p.tally(&s) || name.is_none() {
                    p.page.bad_root = true;
                }
                Ok(())
            }
            "files" | "archived-files" => {
                let into = if k == "files" {
                    &mut files
                } else {
                    &mut archived
                };
                p.list(|p| {
                    if let Some(f) = p.file()? {
                        into.push(f);
                    }
                    Ok(())
                })
            }
            "error" => {
                p.page.error = true;
                p.j.skip().map(drop)
            }
            _ => p.j.skip().map(drop),
        }
    });
    out.name = name;
    out.files = files;
    out.archived = archived;
    out.page = page;
    out
}

/// Reads a root object, handing each key to `f`.
fn run(
    body: &[u8],
    limits: &Limits,
    mut f: impl FnMut(&mut Parser<'_, '_>, &str, &mut Keys) -> R<()>,
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
        page: Page {
            stop: summary.stop,
            ..Page::default()
        },
    };
    let mut read_all = |p: &mut Parser<'_, '_>| -> R<()> {
        if p.j.peek() != Some(b'{') {
            p.page.bad_root = true;
            p.j.skip()?;
        } else {
            let mut keys = Keys::default();
            p.object(|p, k| f(p, k, &mut keys))?;
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
    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// An array of objects that must be next, each handed to `f`, up to
    /// [`Limits::max_items`] and [`Limits::max_array`]; past either the
    /// parse stops. Anything but an array is a bad root.
    fn list(&mut self, mut f: impl FnMut(&mut Self) -> R<()>) -> R<()> {
        if self.j.peek() != Some(b'[') {
            self.page.bad_root = true;
            return self.j.skip().map(drop);
        }
        let max = self.l.max_items.min(self.l.max_array);
        self.array(|p, n| {
            if n >= max {
                p.page.stop.get_or_insert(Stop::Items);
                return p.j.skip().map(drop);
            }
            f(p)
        })
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

    /// A release or file name: a string, cleaned, not empty, at most
    /// [`MAX_RELEASE`] bytes.
    fn name(&mut self, s: &mut Spoil) -> R<Option<String>> {
        let Some(raw) = self.string(s)? else {
            return Ok(None);
        };
        match title(&raw) {
            Some(t) if t.len() <= MAX_RELEASE => Ok(Some(t)),
            Some(_) => {
                s.set(Bad::OverCap);
                Ok(None)
            }
            None => {
                s.set(Bad::Malformed);
                Ok(None)
            }
        }
    }

    /// A size: a whole number of bytes, or `null` when `nullable`.
    fn size(&mut self, s: &mut Spoil, nullable: bool) -> R<Option<u64>> {
        match self.j.peek() {
            Some(b'n') if nullable => self.skip_in(s).map(|()| None),
            Some(b) if b.is_ascii_digit() => {
                let n = crate::common::small(self.j.num()?, 19).and_then(|n| u64::try_from(n).ok());
                if n.is_none() {
                    s.set(Bad::Malformed);
                }
                Ok(n)
            }
            _ => {
                s.set(Bad::Malformed);
                self.skip_in(s).map(|()| None)
            }
        }
    }

    fn obj(&mut self, s: &mut Spoil, f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<()> {
        if self.j.peek() != Some(b'{') {
            s.set(Bad::Malformed);
            return self.skip_in(s);
        }
        self.object(f)
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
    fn hit(&mut self) -> R<Option<Hit>> {
        const KNOWN: &[&str] = &["release", "imdbId", "size"];
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut release, mut imdb, mut size) = (None, None, None);
        let mut inner = Spoil::default();
        self.obj(&mut s, |p, k| {
            if keys.repeat(k, KNOWN) {
                inner.set(Bad::Malformed);
            }
            match k {
                "release" => release = p.name(&mut inner)?,
                "imdbId" => imdb = p.imdb(&mut inner)?,
                "size" => size = p.size(&mut inner, true)?,
                _ => p.skip_in(&mut inner)?,
            }
            Ok(())
        })?;
        if let Some(b) = inner.0 {
            s.set(b);
        }
        if release.is_none() {
            s.set(Bad::Malformed);
        }
        if !self.tally(&s) {
            return Ok(None);
        }
        let Some(release) = release else {
            return Ok(None);
        };
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(Hit {
            release,
            imdb,
            size,
        }))
    }

    /// `imdbId`: `null`, `""`, or an id [`ImdbId::parse`] reads; any
    /// other value spoils the result.
    fn imdb(&mut self, s: &mut Spoil) -> R<Option<ImdbId>> {
        if self.j.peek() == Some(b'n') {
            return self.skip_in(s).map(|()| None);
        }
        let Some(raw) = self.string(s)? else {
            return Ok(None);
        };
        if raw.is_empty() {
            return Ok(None);
        }
        let id = ImdbId::parse(&raw);
        if id.is_none() {
            s.set(Bad::Malformed);
        }
        Ok(id)
    }

    /// One file of a release: a name, a size and an eight-digit hex CRC.
    fn file(&mut self) -> R<Option<SrrFile>> {
        const KNOWN: &[&str] = &["name", "size", "crc"];
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut name, mut size, mut crc) = (None, None, None);
        let mut inner = Spoil::default();
        self.obj(&mut s, |p, k| {
            if keys.repeat(k, KNOWN) {
                inner.set(Bad::Malformed);
            }
            match k {
                "name" => name = p.name(&mut inner)?,
                "size" => size = p.size(&mut inner, false)?,
                "crc" => crc = p.string(&mut inner)?,
                _ => p.skip_in(&mut inner)?,
            }
            Ok(())
        })?;
        if let Some(b) = inner.0 {
            s.set(b);
        }
        let crc = crc.as_deref().and_then(crc32);
        let file = match (name, size, crc) {
            (Some(name), Some(size), Some(crc)) => Some(SrrFile { name, size, crc }),
            _ => {
                s.set(Bad::Malformed);
                None
            }
        };
        if !self.tally(&s) {
            return Ok(None);
        }
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(file)
    }
}

/// A CRC-32 written as exactly eight hex digits, either case.
pub fn crc32(s: &str) -> Option<u32> {
    (s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        assert_eq!(
            ImdbId::parse("0133093").map(|i| i.to_string()).as_deref(),
            Some("tt0133093")
        );
        assert_eq!(
            ImdbId::parse("tt10872600")
                .map(|i| i.to_string())
                .as_deref(),
            Some("tt10872600")
        );
        for bad in [
            "",
            "tt",
            "133093",
            "0000000",
            "01234567",
            "12345678901",
            "tt01",
            "+133093",
            "013309a",
            " 0133093",
            "tt 0133093",
            "TT0133093",
            "０１３３０９３",
        ] {
            assert_eq!(ImdbId::parse(bad), None, "{bad}");
        }
        assert_eq!(crc32("04523E9B"), Some(0x0452_3E9B));
        assert_eq!(crc32("04523e9b"), Some(0x0452_3E9B));
        for bad in [
            "",
            "4523E9B",
            "004523E9B",
            "0x523E9B",
            "+4523E9B",
            "04523E9G",
        ] {
            assert_eq!(crc32(bad), None, "{bad}");
        }
    }
}
