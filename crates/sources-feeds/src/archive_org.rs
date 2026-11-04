//! Internet Archive JSON (architecture §18.2, source #6): `advancedsearch`
//! result pages, scrape API pages and item `metadata` responses.
//!
//! The safety rules are [`torznab`](crate::torznab)'s. It's a pure
//! function from bytes to items with no I/O. It reads one item at a time,
//! stops at a structural cap with a [`Stop`] reason and keeps the items it
//! finished, and skips (and counts) an item that breaks a field cap or has
//! no usable hash. A half-read item is never used. It never panics.
//!
//! It reads with the bounded [`json`](crate::json) reader rather than
//! `serde_json`, so every cap holds while reading, not after a whole tree
//! is built: nesting is walked without recursion past the few levels the
//! shapes have, a string is copied only when it's a field the parser
//! keeps, and its raw length is checked against [`Limits::max_text`]
//! first.
//!
//! The shapes, matched by key wherever they appear in the root object:
//!
//! - `advancedsearch.php?output=json`: `{"response": {"numFound": n,
//!   "docs": [doc, …]}}`;
//! - the scrape API: `{"items": [doc, …], "cursor": "…", "total": n}`;
//! - `metadata/<identifier>`: `{"files": [file, …], "metadata": {…},
//!   "item_size": n}`. The root object is then the item.
//!
//! A doc's hash is its `btih` field. A metadata item's hash is the `btih`
//! of its `<identifier>_archive.torrent` file entry, else of any
//! `*_archive.torrent` entry, else the `metadata` object's `btih`, else of
//! the first other `.torrent` entry with one; that last may be an
//! uploader's torrent of something else, so the item then claims only the
//! hash and title. Only v1 hashes count (`btih` is a v1 hash by name); a
//! malformed one skips the item (`bad_hash`), a missing one too
//! (`no_hash`).

use crate::common::{Bad, clean_title, hash, prepare, rfc3339};
pub use crate::common::{Hashes, MAX_REF, MAX_TITLE, Skipped, Stop, Summary};
use crate::json::{self, R, Reader, TextError, Walk};

/// Bytes in a kept identifier. IA's own limit is 100.
pub const MAX_IDENTIFIER: usize = 100;
/// Bytes in a kept file path: the claims' path limit.
pub const MAX_PATH: usize = 4096;
/// Collections kept per item; later ones are ignored.
pub const MAX_COLLECTIONS: usize = 16;
/// Bytes in a kept media type (`movies`, `audio`, …).
pub const MAX_MEDIATYPE: usize = 32;
/// Bytes in a kept scrape cursor.
pub const MAX_CURSOR: usize = 1024;
/// `.torrent` entries remembered per item as hash candidates.
pub const MAX_TORRENTS: usize = 16;
/// The item page a kept identifier links to.
pub const DETAILS: &str = "https://archive.org/details/";

/// Caps on one parse. The defaults are [`LIMITS`]; why each value:
/// `docs/decisions/2026-11-02-SRC-9.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Docs and metadata items, kept or skipped ([`Stop::Items`]).
    pub max_items: usize,
    /// Elements in one array inside an item, other than `files`; over it
    /// the item is skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser reads; over it the item is skipped.
    pub max_text: usize,
    /// Files kept per item; the rest are left out (`files_truncated`).
    pub max_files: usize,
    /// Bytes of kept file paths per item; past it the rest are left out.
    pub max_name_bytes: usize,
}

/// The caps for a real response.
pub const LIMITS: Limits = Limits {
    max_bytes: 16 * 1024 * 1024,
    max_depth: 16,
    max_values: 1_000_000,
    max_items: 5_000,
    max_array: 10_000,
    max_text: 64 * 1024,
    max_files: 10_000,
    // The claims' text budget (`MAX_TOTAL`, 1 MiB) less the title and
    // the ids SRC-10c may claim, so the claims clamp never drops a file
    // this list kept without saying so.
    max_name_bytes: 1024 * 1024 - 4096,
};

/// One file an item lists, as IA states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Not empty, at most [`MAX_PATH`] bytes, hidden characters removed.
    pub path: String,
    pub size: Option<u64>,
}

/// One item, as IA stated it. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// A v1 hash only.
    pub hashes: Hashes,
    /// ASCII letters, digits and `._-`, at most [`MAX_IDENTIFIER`] bytes.
    pub identifier: Option<String>,
    /// At most [`MAX_TITLE`] bytes, hidden characters removed.
    pub title: Option<String>,
    /// `item_size`: the item's bytes, which IA's torrent roughly covers.
    pub size: Option<u64>,
    /// `publicdate`, else `addeddate`, in Unix seconds.
    pub published: Option<i64>,
    /// ASCII lower-case letters and digits.
    pub mediatype: Option<String>,
    /// Valid identifiers, at most [`MAX_COLLECTIONS`].
    pub collections: Vec<String>,
    /// [`DETAILS`] and the identifier: the external reference.
    pub link: Option<String>,
    /// A metadata item's files, without the `.torrent` entry the hash came
    /// from. Empty for a search doc.
    pub files: Vec<File>,
    /// Some files were left out: over [`Limits::max_files`] or
    /// [`Limits::max_name_bytes`], a path over [`MAX_PATH`], an entry
    /// whose name isn't one string, or files dropped because the hash
    /// came from an uploader's torrent. Entries with no name, or only
    /// hidden characters, name nothing and don't set it.
    pub files_truncated: bool,
}

/// How a parse went, and the paging a search answer gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// `summary.error` is always `None`.
    pub summary: Summary,
    /// The scrape API's next-page cursor: ASCII letters, digits and
    /// `+/=_.~-`, at most [`MAX_CURSOR`] bytes.
    pub cursor: Option<String>,
    /// `numFound` or the scrape API's `total`.
    pub total: Option<u64>,
}

/// Every kept item and the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub items: Vec<Item>,
    pub page: Page,
}

/// Parses `body` with [`LIMITS`].
pub fn parse(body: &[u8]) -> Feed {
    let mut items = Vec::new();
    let page = parse_each(body, &LIMITS, |i| items.push(i));
    Feed { items, page }
}

/// Parses `body`, handing each kept item to `emit` as soon as it ends.
pub fn parse_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Item)) -> Page {
    let mut page = Page::default();
    let bytes = crate::common::Limits {
        max_bytes: limits.max_bytes,
        ..crate::common::LIMITS
    };
    let text = prepare(body, &bytes, &mut page.summary);
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
        page,
    };
    if let Err(stop) = p.document(&mut emit) {
        p.page.summary.stop.get_or_insert(stop);
    }
    p.page
}

/// The item being read: a search doc, or the root of a metadata answer.
#[derive(Debug, Default)]
struct Draft {
    bad: Option<Bad>,
    identifier: Option<String>,
    title: Option<String>,
    size: Option<u64>,
    published: Option<i64>,
    added: Option<i64>,
    mediatype: Option<String>,
    collections: Option<Vec<String>>,
    /// A `btih` field: `Some(None)` when present but not a v1 hash.
    btih: Option<Option<Hashes>>,
    /// `*_archive.torrent` entries with a `btih`, at most [`MAX_TORRENTS`].
    archives: Vec<Torrent>,
    /// The first other `.torrent` entry with a `btih`.
    other: Option<Torrent>,
    files: Vec<File>,
    name_bytes: usize,
    truncated: bool,
}

/// A `.torrent` entry that can be the item's hash source.
#[derive(Debug)]
struct Torrent {
    /// The path, when short enough to be `<identifier>_archive.torrent`.
    name: Option<String>,
    /// `None` when the `btih` isn't a v1 hash.
    hash: Option<Hashes>,
    /// Its place in `files`, when it was kept there.
    file: Option<usize>,
    /// It was over a cap: unless it's the hash source, a file is missing.
    dropped: bool,
}

/// A `files` entry being read.
#[derive(Default)]
struct FileDraft {
    name: Option<String>,
    /// A `name` that isn't one string, or a second `name`: the entry
    /// can't be listed truthfully.
    bad_name: bool,
    size: Option<u64>,
    btih: Option<Option<Hashes>>,
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

    /// A string within [`Limits::max_text`], decoded; one over the cap or
    /// with a bad escape spoils the item.
    fn text(&mut self, d: &mut Draft) -> R<Option<String>> {
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

    /// Up to `keep` strings: a string value, or the first strings of an
    /// array (IA writes a field either way). Anything else is skipped.
    fn strings(&mut self, d: &mut Draft, keep: usize) -> R<Vec<String>> {
        let mut out = Vec::new();
        match self.j.peek() {
            Some(b'"') => out.extend(self.text(d)?),
            Some(b'[') => {
                let max = self.l.max_array;
                self.array(|p, n| {
                    if n >= max {
                        d.spoil(Bad::OverCap);
                    }
                    if out.len() < keep && p.j.peek() == Some(b'"') {
                        out.extend(p.text(d)?);
                        Ok(())
                    } else {
                        p.skip_in(d)
                    }
                })?;
            }
            _ => self.skip_in(d)?,
        }
        Ok(out)
    }

    /// The first string of [`Self::strings`], trimmed.
    fn first(&mut self, d: &mut Draft) -> R<Option<String>> {
        let s = self.strings(d, 1)?.into_iter().next();
        Ok(s.map(|s| s.trim().to_owned()))
    }

    /// A count: a number or a string of up to 20 digits, nothing else.
    fn count(&mut self, d: &mut Draft) -> R<Option<u64>> {
        match self.j.peek() {
            Some(b'-' | b'0'..=b'9') => Ok(digits(self.j.num()?)),
            Some(b'"') => Ok(self.text(d)?.as_deref().map(str::trim).and_then(digits)),
            _ => self.skip_in(d).map(|()| None),
        }
    }

    // --- The shapes --------------------------------------------------

    fn document(&mut self, emit: &mut dyn FnMut(Item)) -> R<()> {
        if self.j.peek() == Some(b'{') {
            let mut root = Draft::default();
            let mut is_item = false;
            self.object(|p, k| p.root(k, &mut root, &mut is_item, emit))?;
            if is_item {
                self.finish(root, emit);
            }
        } else {
            self.j.skip()?;
        }
        self.j.end()
    }

    fn root(
        &mut self,
        key: &str,
        d: &mut Draft,
        is_item: &mut bool,
        emit: &mut dyn FnMut(Item),
    ) -> R<()> {
        let mut scratch = Draft::default();
        match key {
            "response" if self.j.peek() == Some(b'{') => self.object(|p, k| match k {
                "docs" => p.docs(emit),
                "numFound" if p.page.total.is_none() => {
                    p.page.total = p.count(&mut scratch)?;
                    Ok(())
                }
                _ => p.j.skip().map(drop),
            }),
            "items" => self.docs(emit),
            "total" if self.page.total.is_none() => {
                self.page.total = self.count(&mut scratch)?;
                Ok(())
            }
            "cursor" if self.page.cursor.is_none() => {
                self.page.cursor = self.first(&mut scratch)?.filter(|c| cursor(c));
                Ok(())
            }
            "metadata" | "files" | "item_size" => {
                if !*is_item {
                    *is_item = true;
                    self.start_item()?;
                }
                match key {
                    "metadata" if self.j.peek() == Some(b'{') => self.object(|p, k| p.field(d, k)),
                    "files" if self.j.peek() == Some(b'[') => self.array(|p, _| p.file(d)),
                    "files" => {
                        // Not a list: whatever it names isn't listed.
                        d.truncated = true;
                        self.skip_in(d)
                    }
                    "item_size" => self.field(d, key),
                    _ => self.skip_in(d),
                }
            }
            _ => self.j.skip().map(drop),
        }
    }

    fn start_item(&mut self) -> R<()> {
        self.items += 1;
        if self.items > self.l.max_items {
            return Err(Stop::Items);
        }
        Ok(())
    }

    /// `docs` or `items`: an array of search docs.
    fn docs(&mut self, emit: &mut dyn FnMut(Item)) -> R<()> {
        if self.j.peek() != Some(b'[') {
            return self.j.skip().map(drop);
        }
        self.array(|p, _| {
            p.start_item()?;
            if p.j.peek() != Some(b'{') {
                p.j.skip()?;
                p.page.summary.skipped.malformed += 1;
                return Ok(());
            }
            let mut d = Draft::default();
            p.object(|p, k| p.field(&mut d, k))?;
            p.finish(d, emit);
            Ok(())
        })
    }

    /// A field of a search doc or of a metadata item's `metadata` object.
    /// The first value of each wins; a repeat is skipped.
    fn field(&mut self, d: &mut Draft, key: &str) -> R<()> {
        match key {
            "identifier" if d.identifier.is_none() => {
                d.identifier = self.first(d)?.filter(|s| identifier(s));
            }
            "title" if d.title.is_none() => {
                d.title = self
                    .first(d)?
                    .map(|t| clean_title(&t))
                    .filter(|t| !t.is_empty());
            }
            "btih" if d.btih.is_none() => d.btih = Some(self.first(d)?.and_then(|s| btih(&s))),
            "item_size" if d.size.is_none() => d.size = self.count(d)?,
            "publicdate" if d.published.is_none() => {
                d.published = self.first(d)?.and_then(|s| date(&s));
            }
            "addeddate" if d.added.is_none() => d.added = self.first(d)?.and_then(|s| date(&s)),
            "mediatype" if d.mediatype.is_none() => {
                d.mediatype = self.first(d)?.filter(|s| mediatype(s));
            }
            "collection" if d.collections.is_none() => {
                let mut c = self.strings(d, MAX_COLLECTIONS)?;
                c.iter_mut().for_each(|s| *s = s.trim().to_owned());
                c.retain(|s| identifier(s));
                d.collections = Some(c);
            }
            _ => self.skip_in(d)?,
        }
        Ok(())
    }

    /// One `files` entry of a metadata item.
    fn file(&mut self, d: &mut Draft) -> R<()> {
        if self.j.peek() != Some(b'{') {
            // An entry that isn't an object may still name a file.
            d.truncated = true;
            return self.skip_in(d);
        }
        let mut f = FileDraft::default();
        let mut named = false;
        self.object(|p, k| {
            match k {
                "name" if !named && p.j.peek() == Some(b'"') => {
                    named = true;
                    f.name = p.text(d)?.map(|s| s.trim().to_owned());
                }
                "name" => {
                    f.bad_name = true;
                    p.skip_in(d)?;
                }
                "size" if f.size.is_none() => f.size = p.count(d)?,
                "btih" if f.btih.is_none() => f.btih = Some(p.first(d)?.and_then(|s| btih(&s))),
                _ => p.skip_in(d)?,
            }
            Ok(())
        })?;
        d.add_file(f, self.l);
        Ok(())
    }

    fn finish(&mut self, d: Draft, emit: &mut dyn FnMut(Item)) {
        match d.finish() {
            Ok(item) => {
                self.page.summary.kept += 1;
                emit(item);
            }
            Err(skip) => skip(&mut self.page.summary.skipped),
        }
    }
}

impl Draft {
    fn spoil(&mut self, bad: Bad) {
        self.bad.get_or_insert(bad);
    }

    /// Lists the entry within the caps. Every `.torrent` is listed; one
    /// with a `btih` is also a hash candidate, and the one the hash comes
    /// from is taken out in [`Self::finish`]. An entry with no name, or
    /// only hidden characters, names nothing and is dropped.
    fn add_file(&mut self, f: FileDraft, l: &Limits) {
        if f.bad_name {
            self.truncated = true;
            return;
        }
        let Some(name) = f.name else { return };
        let path: String = name
            .chars()
            .filter(|&c| !crate::common::unsafe_char(c))
            .collect();
        if path.is_empty() {
            return;
        }
        let candidate = f.btih.is_some() && ends_with(&path, ".torrent");
        let over = path.len() > MAX_PATH
            || self.files.len() >= l.max_files
            || self.name_bytes + path.len() > l.max_name_bytes;
        // A dropped candidate counts as missing only if it isn't the
        // hash source, which isn't listed anyway; `finish` decides.
        let file = if over {
            self.truncated |= !candidate;
            None
        } else {
            self.name_bytes += path.len();
            self.files.push(File {
                path: path.clone(),
                size: f.size,
            });
            Some(self.files.len() - 1)
        };
        let Some(hash) = f.btih.filter(|_| candidate) else {
            return;
        };
        let t = Torrent {
            name: (path.len() <= MAX_IDENTIFIER + ARCHIVE.len()).then(|| path.clone()),
            hash,
            file,
            dropped: over,
        };
        let slot = if ends_with(&path, ARCHIVE) {
            if self.archives.len() < MAX_TORRENTS {
                self.archives.push(t);
                return;
            }
            Some(t)
        } else if self.other.is_none() {
            self.other = Some(t);
            return;
        } else {
            Some(t)
        };
        // Not remembered as a candidate, so never the hash source.
        if slot.is_some_and(|t| t.dropped) {
            self.truncated = true;
        }
    }

    /// The finished item, or the counter it's skipped under.
    ///
    /// The hash comes from IA's own torrent (`<identifier>_archive.torrent`,
    /// else any `*_archive.torrent`), else the `btih` field, else the first
    /// other `.torrent`. That last one may be an uploader's torrent of
    /// something else, so the item then claims only the hash and title.
    fn finish(mut self) -> Result<Item, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap += 1),
            Some(Bad::Malformed) => return Err(|s| s.malformed += 1),
            None => {}
        }
        let exact = self.identifier.as_ref().map(|i| format!("{i}{ARCHIVE}"));
        let archive = self
            .archives
            .iter()
            .position(|t| exact.is_some() && t.name == exact)
            .or((!self.archives.is_empty()).then_some(0))
            .map(|at| self.archives.swap_remove(at));
        let other = match (&archive, self.btih) {
            (None, None) => self.other.take(),
            _ => None,
        };
        let whole = other.is_none();
        let source = match (archive, self.btih, other) {
            (Some(t), _, _) | (None, None, Some(t)) => Some((t.hash, t.file)),
            (None, Some(h), _) => Some((h, None)),
            (None, None, None) => None,
        };
        let (hashes, file) = match source {
            None => return Err(|s| s.no_hash += 1),
            Some((None, _)) => return Err(|s| s.bad_hash += 1),
            Some((Some(h), file)) => (h, file),
        };
        // Candidates that weren't the source and didn't fit are missing.
        self.truncated |= self.archives.iter().chain(&self.other).any(|t| t.dropped);
        // The hash source describes the torrent, not a file in it.
        if let Some(at) = file.filter(|&at| at < self.files.len()) {
            self.files.remove(at);
        }
        if !whole {
            return Ok(Item {
                hashes,
                identifier: None,
                title: self.title,
                size: None,
                published: None,
                mediatype: None,
                collections: Vec::new(),
                link: None,
                files_truncated: self.truncated || !self.files.is_empty(),
                files: Vec::new(),
            });
        }
        Ok(Item {
            hashes,
            link: self.identifier.as_ref().map(|i| format!("{DETAILS}{i}")),
            identifier: self.identifier,
            title: self.title,
            size: self.size,
            published: self.published.or(self.added),
            mediatype: self.mediatype,
            collections: self.collections.unwrap_or_default(),
            files: self.files,
            files_truncated: self.truncated,
        })
    }
}

/// IA's own torrent of an item is `<identifier>_archive.torrent`.
const ARCHIVE: &str = "_archive.torrent";

/// Whether `s` ends with `suffix`, ignoring ASCII case.
fn ends_with(s: &str, suffix: &str) -> bool {
    s.len() > suffix.len()
        && s.get(s.len() - suffix.len()..)
            .is_some_and(|e| e.eq_ignore_ascii_case(suffix))
}

/// Up to 20 ASCII digits that fit a `u64`.
fn digits(s: &str) -> Option<u64> {
    let ok = !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| s.parse().ok()).flatten()
}

/// A v1 hash: 40 hex digits or 32 base32 characters, not all zero.
fn btih(s: &str) -> Option<Hashes> {
    hash(s.trim()).filter(|h| h.v1.is_some() && h.v2.is_none())
}

/// An IA identifier: 1–[`MAX_IDENTIFIER`] ASCII letters, digits and `._-`.
fn identifier(s: &str) -> bool {
    (1..=MAX_IDENTIFIER).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn mediatype(s: &str) -> bool {
    (1..=MAX_MEDIATYPE).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn cursor(s: &str) -> bool {
    (1..=MAX_CURSOR).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_.~-".contains(&b))
}

/// IA's dates: RFC 3339 (`2026-11-01T12:34:56Z`, the search index), or
/// `2026-11-01 12:34:56` and `2026-11-01` in UTC (item metadata).
fn date(s: &str) -> Option<i64> {
    match s.len() {
        10 => rfc3339(&format!("{s}T00:00:00Z")),
        19 => rfc3339(&format!("{s}Z")),
        _ => rfc3339(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{decode, hex4};

    #[test]
    fn decodes_escapes() {
        let u = |s: &str| s.replace('U', "\\u");
        assert_eq!(
            decode(&u(r"aU00e9\n\/\\U0022")).as_deref(),
            Some("a\u{e9}\n/\\\"")
        );
        assert_eq!(decode(&u("Ud83dUde00")).as_deref(), Some("\u{1F600}"));
        for bad in ["Ud83d", "Ude00", "Ud83dx", "Ud83dU0041", r"\x"] {
            assert_eq!(decode(&u(bad)), None, "{bad}");
        }
        // Decoding never grows the text, so the raw length bounds it.
        let raw = u("U0041Ud83dUde00");
        assert!(decode(&raw).unwrap().len() <= raw.len());
    }

    #[test]
    fn small_parsers() {
        assert_eq!(digits("12345"), Some(12345));
        assert_eq!(digits("18446744073709551615"), Some(u64::MAX));
        for bad in ["", "-1", "1.5", "1e3", "18446744073709551616", "+1"] {
            assert_eq!(digits(bad), None, "{bad}");
        }
        assert_eq!(date("1970-01-01"), Some(0));
        assert_eq!(date("2026-11-01 12:34:56"), Some(1_793_536_496));
        assert_eq!(date("2026-11-01T12:34:56Z"), Some(1_793_536_496));
        assert_eq!(date("2026-11-01 12:34"), None);
        assert!(identifier("open-movie_1.2"));
        assert!(!identifier("a/b") && !identifier("") && !identifier(&"a".repeat(101)));
        assert!(mediatype("movies") && !mediatype("Movies") && !mediatype(""));
        assert_eq!(hex4("+fff"), None);
        let v2 = "a".repeat(64);
        assert_eq!(btih(&v2), None, "btih is a v1 hash");
        assert!(btih(" 0123456789abcdef0123456789abcdef01234567 ").is_some());
    }
}
