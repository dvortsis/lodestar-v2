//! Internet Archive JSON (architecture §18.2, source #6): `advancedsearch`
//! result pages, scrape API pages and item `metadata` responses.
//!
//! The safety rules are [`torznab`](crate::torznab)'s. It's a pure
//! function from bytes to items with no I/O. It reads one item at a time,
//! stops at a structural cap with a [`Stop`] reason and keeps the items it
//! finished, and skips (and counts) an item that breaks a field cap or has
//! no usable hash. A half-read item is never used. It never panics.
//!
//! The JSON reader is written here rather than taken from `serde_json`,
//! so every cap holds while reading, not after a whole tree is built:
//! nesting is walked without recursion past the few levels the shapes
//! have, a string is copied only when it's a field the parser keeps, and
//! its raw length is checked against [`Limits::max_text`] first.
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
//! of its `<identifier>_archive.torrent` file entry, else of its first
//! `.torrent` entry with one, else the `metadata` object's `btih`. Only v1
//! hashes count (`btih` is a v1 hash by name); a malformed one skips the
//! item (`bad_hash`), a missing one too (`no_hash`).

use crate::common::{Bad, clean_title, hash, prepare, rfc3339};
pub use crate::common::{Hashes, MAX_REF, MAX_TITLE, Skipped, Stop, Summary};

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
    max_name_bytes: 1024 * 1024,
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
    /// A metadata item's files, without its `.torrent` entries. Empty for
    /// a search doc.
    pub files: Vec<File>,
    /// Some files were left out: over [`Limits::max_files`] or
    /// [`Limits::max_name_bytes`], or a path over [`MAX_PATH`].
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
    let mut p = Parser {
        s: text,
        i: 0,
        l: limits,
        depth: 0,
        values: 0,
        items: 0,
        page,
    };
    if let Err(stop) = p.document(&mut emit) {
        p.page.summary.stop.get_or_insert(stop);
    }
    p.page
}

type R<T> = Result<T, Stop>;

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
    /// `.torrent` entries with a `btih`: the name (when short) and hash.
    torrents: Vec<(Option<String>, Option<Hashes>)>,
    files: Vec<File>,
    name_bytes: usize,
    truncated: bool,
}

/// A `files` entry being read.
#[derive(Default)]
struct FileDraft {
    name: Option<String>,
    size: Option<u64>,
    btih: Option<Option<Hashes>>,
}

struct Parser<'a, 'l> {
    s: &'a str,
    i: usize,
    l: &'l Limits,
    depth: usize,
    values: usize,
    items: usize,
    page: Page,
}

impl<'a> Parser<'a, '_> {
    // --- The JSON reader ---------------------------------------------

    fn peek(&mut self) -> Option<u8> {
        let b = self.s.as_bytes();
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = b.get(self.i) {
            self.i += 1;
        }
        b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> R<()> {
        if self.peek() != Some(c) {
            return Err(Stop::Malformed);
        }
        self.i += 1;
        Ok(())
    }

    /// Counts one value.
    fn value(&mut self) -> R<()> {
        self.values += 1;
        if self.values > self.l.max_values {
            return Err(Stop::Elements);
        }
        Ok(())
    }

    /// Opens a container whose `c` is next.
    fn open(&mut self, c: u8) -> R<()> {
        self.eat(c)?;
        self.depth += 1;
        if self.depth > self.l.max_depth {
            return Err(Stop::Depth);
        }
        Ok(())
    }

    /// Closes the container if `c` is next.
    fn close(&mut self, c: u8) -> bool {
        if self.peek() != Some(c) {
            return false;
        }
        self.i += 1;
        self.depth -= 1;
        true
    }

    /// After an element: `,` (true), or the closing `c` (false).
    fn next(&mut self, c: u8) -> R<bool> {
        if self.peek() == Some(b',') {
            self.i += 1;
            return Ok(true);
        }
        if self.close(c) {
            return Ok(false);
        }
        Err(Stop::Malformed)
    }

    /// An object that is next, handing each key to `f`, which reads the
    /// value. A key is compared as written; IA doesn't escape keys.
    fn object(&mut self, mut f: impl FnMut(&mut Self, &str) -> R<()>) -> R<()> {
        self.value()?;
        self.open(b'{')?;
        if self.close(b'}') {
            return Ok(());
        }
        loop {
            let key = self.string()?;
            self.eat(b':')?;
            f(self, key)?;
            if !self.next(b'}')? {
                return Ok(());
            }
        }
    }

    /// An array that is next, handing each element's index to `f`, which
    /// reads the element.
    fn array(&mut self, mut f: impl FnMut(&mut Self, usize) -> R<()>) -> R<()> {
        self.value()?;
        self.open(b'[')?;
        if self.close(b']') {
            return Ok(());
        }
        for n in 0.. {
            f(self, n)?;
            if !self.next(b']')? {
                break;
            }
        }
        Ok(())
    }

    /// A string that is next, still escaped, without its quotes. Escapes
    /// are checked; the text is valid UTF-8 already.
    fn string(&mut self) -> R<&'a str> {
        self.eat(b'"')?;
        let (s, start) = (self.s, self.i);
        let b = s.as_bytes();
        loop {
            match b.get(self.i) {
                Some(b'"') => {
                    let raw = s.get(start..self.i).ok_or(Stop::Malformed)?;
                    self.i += 1;
                    return Ok(raw);
                }
                Some(b'\\') => match b.get(self.i + 1) {
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 2,
                    Some(b'u')
                        if b.get(self.i + 2..self.i + 6)
                            .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) =>
                    {
                        self.i += 6
                    }
                    _ => return Err(Stop::Malformed),
                },
                Some(&c) if c >= 0x20 => self.i += 1,
                _ => return Err(Stop::Malformed),
            }
        }
    }

    /// A number that is next, as written (RFC 8259 grammar).
    fn number(&mut self) -> R<&'a str> {
        let (s, start) = (self.s, self.i);
        let b = s.as_bytes();
        let digits = |i: &mut usize| {
            let from = *i;
            while b.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            *i > from
        };
        let mut i = start;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                digits(&mut i);
            }
            _ => return Err(Stop::Malformed),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if !digits(&mut i) {
                return Err(Stop::Malformed);
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if !digits(&mut i) {
                return Err(Stop::Malformed);
            }
        }
        self.i = i;
        s.get(start..i).ok_or(Stop::Malformed)
    }

    fn literal(&mut self, word: &str) -> R<()> {
        if !self.s.get(self.i..).is_some_and(|r| r.starts_with(word)) {
            return Err(Stop::Malformed);
        }
        self.i += word.len();
        Ok(())
    }

    /// Reads past the next value without keeping it. True when an array
    /// in it is longer than [`Limits::max_array`]. Iterative: its stack
    /// is bounded by [`Limits::max_depth`].
    fn skip(&mut self) -> R<bool> {
        let mut over = false;
        // One entry per open container: whether it's an array, and its
        // elements so far.
        let mut stack: Vec<(bool, usize)> = Vec::new();
        loop {
            self.value()?;
            let opened = match self.peek() {
                Some(b'{') => {
                    self.open(b'{')?;
                    !self.close(b'}') && {
                        stack.push((false, 1));
                        true
                    }
                }
                Some(b'[') => {
                    self.open(b'[')?;
                    !self.close(b']') && {
                        stack.push((true, 1));
                        over |= self.l.max_array == 0;
                        true
                    }
                }
                Some(b'"') => self.string().map(|_| false)?,
                Some(b't') => self.literal("true").map(|_| false)?,
                Some(b'f') => self.literal("false").map(|_| false)?,
                Some(b'n') => self.literal("null").map(|_| false)?,
                Some(b'-' | b'0'..=b'9') => self.number().map(|_| false)?,
                _ => return Err(Stop::Malformed),
            };
            if opened {
                if stack.last().is_some_and(|t| !t.0) {
                    self.string()?;
                    self.eat(b':')?;
                }
                continue;
            }
            // A value ended: close what ends with it, then go on to the
            // next element of the innermost open container.
            loop {
                let Some(&(is_array, n)) = stack.last() else {
                    return Ok(over);
                };
                let close = if is_array { b']' } else { b'}' };
                if !self.next(close)? {
                    stack.pop();
                    continue;
                }
                if is_array {
                    over |= n >= self.l.max_array;
                } else {
                    self.string()?;
                    self.eat(b':')?;
                }
                if let Some(t) = stack.last_mut() {
                    t.1 = n + 1;
                }
                break;
            }
        }
    }

    // --- Values the parser keeps -------------------------------------

    /// Skips the next value inside an item; an over-long array spoils it.
    fn skip_in(&mut self, d: &mut Draft) -> R<()> {
        if self.skip()? {
            d.spoil(Bad::OverCap);
        }
        Ok(())
    }

    /// A string within [`Limits::max_text`], decoded; one over the cap or
    /// with a bad escape spoils the item.
    fn text(&mut self, d: &mut Draft) -> R<Option<String>> {
        self.value()?;
        let raw = self.string()?;
        if raw.len() > self.l.max_text {
            d.spoil(Bad::OverCap);
            return Ok(None);
        }
        let s = decode(raw);
        if s.is_none() {
            d.spoil(Bad::Malformed);
        }
        Ok(s)
    }

    /// Up to `keep` strings: a string value, or the first strings of an
    /// array (IA writes a field either way). Anything else is skipped.
    fn strings(&mut self, d: &mut Draft, keep: usize) -> R<Vec<String>> {
        let mut out = Vec::new();
        match self.peek() {
            Some(b'"') => out.extend(self.text(d)?),
            Some(b'[') => {
                let max = self.l.max_array;
                self.array(|p, n| {
                    if n >= max {
                        d.spoil(Bad::OverCap);
                    }
                    if out.len() < keep && p.peek() == Some(b'"') {
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
        match self.peek() {
            Some(b'-' | b'0'..=b'9') => {
                self.value()?;
                Ok(digits(self.number()?))
            }
            Some(b'"') => Ok(self.text(d)?.as_deref().map(str::trim).and_then(digits)),
            _ => self.skip_in(d).map(|()| None),
        }
    }

    // --- The shapes --------------------------------------------------

    fn document(&mut self, emit: &mut dyn FnMut(Item)) -> R<()> {
        if self.peek() == Some(b'{') {
            let mut root = Draft::default();
            let mut is_item = false;
            self.object(|p, k| p.root(k, &mut root, &mut is_item, emit))?;
            if is_item {
                self.finish(root, emit);
            }
        } else {
            self.skip()?;
        }
        if self.peek().is_some() {
            return Err(Stop::Malformed);
        }
        Ok(())
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
            "response" if self.peek() == Some(b'{') => self.object(|p, k| match k {
                "docs" => p.docs(emit),
                "numFound" if p.page.total.is_none() => {
                    p.page.total = p.count(&mut scratch)?;
                    Ok(())
                }
                _ => p.skip().map(drop),
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
                    "metadata" if self.peek() == Some(b'{') => self.object(|p, k| p.field(d, k)),
                    "files" if self.peek() == Some(b'[') => self.array(|p, _| p.file(d)),
                    "item_size" => self.field(d, key),
                    _ => self.skip_in(d),
                }
            }
            _ => self.skip().map(drop),
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
        if self.peek() != Some(b'[') {
            return self.skip().map(drop);
        }
        self.array(|p, _| {
            p.start_item()?;
            if p.peek() != Some(b'{') {
                p.skip()?;
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
        if self.peek() != Some(b'{') {
            return self.skip_in(d);
        }
        let mut f = FileDraft::default();
        self.object(|p, k| {
            match k {
                "name" if f.name.is_none() => f.name = p.first(d)?,
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

    fn add_file(&mut self, f: FileDraft, l: &Limits) {
        let Some(name) = f.name else { return };
        let torrent = name.len() > 8
            && name
                .get(name.len() - 8..)
                .is_some_and(|e| e.eq_ignore_ascii_case(".torrent"));
        if let (true, Some(h)) = (torrent, f.btih) {
            if self.torrents.len() < MAX_TORRENTS {
                let short = (name.len() <= MAX_IDENTIFIER + 16).then_some(name);
                self.torrents.push((short, h));
            }
            return;
        }
        let path: String = name
            .chars()
            .filter(|&c| !crate::common::unsafe_char(c))
            .collect();
        if path.is_empty() {
            return;
        }
        if path.len() > MAX_PATH
            || self.files.len() >= l.max_files
            || self.name_bytes + path.len() > l.max_name_bytes
        {
            self.truncated = true;
            return;
        }
        self.name_bytes += path.len();
        self.files.push(File { path, size: f.size });
    }

    /// The finished item, or the counter it's skipped under.
    fn finish(self) -> Result<Item, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap += 1),
            Some(Bad::Malformed) => return Err(|s| s.malformed += 1),
            None => {}
        }
        let archive = self
            .identifier
            .as_ref()
            .map(|i| format!("{i}_archive.torrent"));
        let from_files = self
            .torrents
            .iter()
            .find(|t| archive.is_some() && t.0 == archive)
            .or(self.torrents.first())
            .map(|t| t.1);
        let hashes = match from_files.or(self.btih) {
            None => return Err(|s| s.no_hash += 1),
            Some(None) => return Err(|s| s.bad_hash += 1),
            Some(Some(h)) => h,
        };
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

/// A JSON string's text without its escapes; `None` for a lone surrogate.
/// The escapes were checked by the reader. Never longer than `raw`.
fn decode(raw: &str) -> Option<String> {
    if !raw.contains('\\') {
        return Some(raw.to_owned());
    }
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let (mut i, mut run) = (0, 0);
    while let Some(&c) = b.get(i) {
        if c != b'\\' {
            i += 1;
            continue;
        }
        out.push_str(raw.get(run..i)?);
        let ch = match b.get(i + 1)? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = hex4(raw.get(i + 2..i + 6)?)?;
                i += 6;
                let cp = match hi {
                    0xD800..=0xDBFF => {
                        if raw.get(i..i + 2)? != "\\u" {
                            return None;
                        }
                        let lo = hex4(raw.get(i + 2..i + 6)?)?;
                        if !(0xDC00..=0xDFFF).contains(&lo) {
                            return None;
                        }
                        i += 6;
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    }
                    0xDC00..=0xDFFF => return None,
                    _ => hi,
                };
                out.push(char::from_u32(cp)?);
                run = i;
                continue;
            }
            _ => return None,
        };
        out.push(ch);
        i += 2;
        run = i;
    }
    out.push_str(raw.get(run..)?);
    Some(out)
}

fn hex4(s: &str) -> Option<u32> {
    (s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
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
