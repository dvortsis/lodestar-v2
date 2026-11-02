//! Torznab and Newznab search and RSS results (architecture §18.2): the
//! RSS 2.0 `<item>`s Prowlarr and Jackett return, with `torznab:attr` or
//! `newznab:attr` children.
//!
//! The body is untrusted, so the parser:
//!
//! - **streams** over the body with `quick-xml`, holding one item at a
//!   time, and copies text only for the fields it keeps;
//! - **refuses DTDs** (a `<!DOCTYPE` stops the parse) and resolves only
//!   character references and the five XML entities, so nothing expands;
//! - **stops at a structural cap** ([`Limits`]: bytes, depth, elements,
//!   items) with a [`Stop`] reason, keeping the items it finished;
//! - **skips an item** that breaks a field cap (text or attribute length,
//!   attributes per element), uses an unknown entity, or has no usable
//!   info hash, and counts it in [`Skipped`]. An item is used whole or
//!   not at all.
//!
//! Element names are matched literally (`torznab:attr`, `newznab:attr`),
//! the prefixes every indexer manager writes; namespace URIs aren't
//! resolved. Atom and other RSS dialects are SRC-8's.

use lodestar_metainfo::{InfoHash, InfoHashV2};
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// Bytes in a kept title: the claims' name limit.
pub const MAX_TITLE: usize = 1024;
/// Bytes in a kept GUID or link: the claims' external-reference limit.
/// A longer one is dropped, since a cut URL points somewhere else.
pub const MAX_REF: usize = 2048;
/// Category ids kept per item; more are ignored.
pub const MAX_CATEGORIES: usize = 16;
/// Indexer tags (`freeleech`, `internal`, …) kept per item, and the
/// bytes in each; longer tags are ignored.
pub const MAX_TAGS: usize = 8;
pub const MAX_TAG: usize = 64;
/// Bytes in an external ID's value (`tt0111161`); longer are ignored.
pub const MAX_ID: usize = 32;

/// Caps on one parse. The defaults are [`LIMITS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open elements at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Elements in the document ([`Stop::Elements`]).
    pub max_elements: usize,
    /// `<item>`s in the document, kept or skipped ([`Stop::Items`]).
    pub max_items: usize,
    /// Attributes on an element the parser reads.
    pub max_attrs: usize,
    /// Bytes in an attribute value the parser reads.
    pub max_attr_len: usize,
    /// Bytes of text in a field the parser reads.
    pub max_text: usize,
}

/// The caps for a real feed. Why each value: `docs/decisions/2026-11-01-SRC-6.md`.
pub const LIMITS: Limits = Limits {
    max_bytes: 16 * 1024 * 1024,
    max_depth: 16,
    max_elements: 1_000_000,
    max_items: 5_000,
    max_attrs: 16,
    max_attr_len: 16 * 1024,
    max_text: 64 * 1024,
};

/// Both forms of a torrent's identity; at least one is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashes {
    pub v1: Option<InfoHash>,
    pub v2: Option<InfoHashV2>,
}

impl Hashes {
    /// The DHT key: the v1 hash, or the v2 hash truncated (BEP 52).
    pub fn key(&self) -> Option<InfoHash> {
        self.v1.or(self.v2.map(|h| h.truncated()))
    }
}

/// What an item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Torrent(Hashes),
    /// A Newznab (Usenet) item. Parsed for v0.7; nothing uses it yet.
    Usenet,
}

/// One item, as the indexer stated it. Every field is a claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// At most [`MAX_TITLE`] bytes, control characters removed.
    pub title: Option<String>,
    pub size: Option<u64>,
    /// Newznab category ids (`2000` movies, `5070` anime, …), deduplicated.
    pub categories: Vec<u32>,
    /// `pubDate`, in Unix seconds.
    pub published: Option<i64>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub peers: Option<u32>,
    pub files: Option<u32>,
    /// Ratio factors: `0` is freeleech on download, `2` double upload.
    pub download_factor: Option<f64>,
    pub upload_factor: Option<f64>,
    pub tags: Vec<String>,
    /// External IDs: (`imdb`, `tt0111161`), (`tmdb`, `278`), (`tvdb`, `81189`).
    pub ids: Vec<(&'static str, String)>,
    /// The external reference: GUID and link, with credentials taken out
    /// of http(s) URLs (see [`external_ref`]) and magnets left out.
    pub guid: Option<String>,
    pub link: Option<String>,
}

/// Items skipped, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No info hash and no magnet (and not a Newznab item).
    pub no_hash: u32,
    /// An info hash or magnet that isn't a v1 or v2 hash.
    pub bad_hash: u32,
    /// A field over a cap in [`Limits`].
    pub over_cap: u32,
    /// An unknown entity or a malformed attribute.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.no_hash + self.bad_hash + self.over_cap + self.malformed
    }
}

/// Why a parse ended before the end of a well-formed document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Bytes,
    /// The body isn't UTF-8 past some point; the prefix was parsed.
    NotUtf8,
    Doctype,
    Depth,
    Elements,
    Items,
    /// Not well-formed XML, including a document cut short.
    Malformed,
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub kept: u32,
    pub skipped: Skipped,
    pub stop: Option<Stop>,
    /// The code of a Torznab `<error code="…">` answer. Its description
    /// is the server's text and isn't kept.
    pub error: Option<u32>,
}

/// Every kept item and the summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    pub items: Vec<Item>,
    pub summary: Summary,
}

/// Parses `body` with [`LIMITS`].
pub fn parse(body: &[u8]) -> Feed {
    let mut items = Vec::new();
    let summary = parse_each(body, &LIMITS, |i| items.push(i));
    Feed { items, summary }
}

/// Parses `body`, handing each kept item to `emit` as soon as it ends.
pub fn parse_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Item)) -> Summary {
    let mut out = Summary::default();
    let mut body = body;
    if body.len() > limits.max_bytes {
        body = &body[..limits.max_bytes];
        out.stop = Some(Stop::Bytes);
    }
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            out.stop.get_or_insert(Stop::NotUtf8);
            std::str::from_utf8(&body[..e.valid_up_to()]).unwrap_or_default()
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = Parser {
        limits,
        out,
        depth: 0,
        elements: 0,
        items: 0,
        draft: None,
    };
    let mut reader = Reader::from_str(text);
    loop {
        let flow = match reader.read_event() {
            Ok(Event::Eof) if p.depth == 0 => break,
            Ok(Event::Eof) | Err(_) => Err(Stop::Malformed),
            Ok(ev) => p.event(ev, &mut emit),
        };
        if let Err(stop) = flow {
            p.out.stop.get_or_insert(stop);
            break;
        }
    }
    p.out
}

/// A field whose text is being collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Guid,
    Link,
    Size,
    PubDate,
    Category,
}

/// Why an item is skipped, when a field breaks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bad {
    OverCap,
    Malformed,
}

/// The item being read.
#[derive(Debug, Default)]
struct Draft {
    depth: usize,
    bad: Option<Bad>,
    field: Option<Field>,
    buf: String,
    torznab: bool,
    newznab: bool,
    /// The `infohash` attribute: `Some(None)` when present but invalid.
    infohash: Option<Option<Hashes>>,
    magnet: Option<String>,
    enclosure: Option<String>,
    size_attr: Option<u64>,
    size_el: Option<u64>,
    size_enc: Option<u64>,
    item: Fields,
}

/// The fields copied straight into the [`Item`].
#[derive(Debug, Default)]
struct Fields {
    title: Option<String>,
    categories: Vec<u32>,
    published: Option<i64>,
    seeders: Option<u32>,
    leechers: Option<u32>,
    peers: Option<u32>,
    files: Option<u32>,
    download_factor: Option<f64>,
    upload_factor: Option<f64>,
    tags: Vec<String>,
    ids: Vec<(&'static str, String)>,
    guid: Option<String>,
    link: Option<String>,
}

struct Parser<'l> {
    limits: &'l Limits,
    out: Summary,
    depth: usize,
    elements: usize,
    items: usize,
    draft: Option<Draft>,
}

impl Parser<'_> {
    fn event(&mut self, ev: Event<'_>, emit: &mut impl FnMut(Item)) -> Result<(), Stop> {
        match ev {
            Event::Start(e) => self.open(&e),
            Event::Empty(e) => {
                self.open(&e)?;
                self.close(emit);
                Ok(())
            }
            Event::End(_) => {
                self.close(emit);
                Ok(())
            }
            Event::Text(t) if self.collecting() => {
                self.text(&t.xml10_content());
                Ok(())
            }
            Event::CData(t) if self.collecting() => {
                self.text(&t.xml10_content());
                Ok(())
            }
            Event::Text(_) | Event::CData(_) => Ok(()),
            Event::GeneralRef(_) if self.draft.is_none() => Ok(()),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c.encode_utf8(&mut [0; 4]).to_owned()),
                    Ok(None) => resolve_xml_entity(&r).map(str::to_owned),
                    Err(_) => None,
                };
                match resolved {
                    Some(s) => self.text(&s),
                    None => self.spoil(Bad::Malformed),
                }
                Ok(())
            }
            Event::DocType(_) => Err(Stop::Doctype),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::Eof => Ok(()),
        }
    }

    fn open(&mut self, e: &BytesStart<'_>) -> Result<(), Stop> {
        self.elements += 1;
        if self.elements > self.limits.max_elements {
            return Err(Stop::Elements);
        }
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            return Err(Stop::Depth);
        }
        let name = e.name();
        let name = name.as_ref();
        let Some(d) = self.draft.as_mut() else {
            if name == "item" {
                self.items += 1;
                if self.items > self.limits.max_items {
                    return Err(Stop::Items);
                }
                self.draft = Some(Draft {
                    depth: self.depth,
                    ..Draft::default()
                });
            } else if self.depth == 1 && name == "error" {
                let mut code = None;
                let _ = attrs(e, self.limits, |k, v| {
                    if k == "code" {
                        code = v.trim().parse().ok();
                    }
                });
                self.out.error = code;
            }
            return Ok(());
        };
        if self.depth != d.depth + 1 || d.bad.is_some() {
            return Ok(());
        }
        d.field = match name {
            "title" => Some(Field::Title),
            "guid" => Some(Field::Guid),
            "link" => Some(Field::Link),
            "size" => Some(Field::Size),
            "pubDate" => Some(Field::PubDate),
            "category" => Some(Field::Category),
            "torznab:attr" | "newznab:attr" => {
                let torznab = name.starts_with('t');
                d.torznab |= torznab;
                d.newznab |= !torznab;
                let (mut key, mut val) = (None, None);
                let r = attrs(e, self.limits, |k, v| match k {
                    "name" => key = Some(v.to_ascii_lowercase()),
                    "value" => val = Some(v.to_owned()),
                    _ => {}
                });
                match (r, key, val) {
                    (Err(bad), ..) => d.bad = Some(bad),
                    (Ok(()), Some(k), Some(v)) => d.attr(&k, v.trim()),
                    _ => {}
                }
                None
            }
            "enclosure" => {
                let r = attrs(e, self.limits, |k, v| match k {
                    "url" if d.enclosure.is_none() => d.enclosure = Some(v.trim().to_owned()),
                    "length" => d.size_enc = d.size_enc.or(v.trim().parse().ok()),
                    _ => {}
                });
                if let Err(bad) = r {
                    d.bad = Some(bad);
                }
                None
            }
            _ => None,
        };
        d.buf.clear();
        Ok(())
    }

    fn close(&mut self, emit: &mut impl FnMut(Item)) {
        let depth = self.depth;
        self.depth = depth.saturating_sub(1);
        let Some(d) = self.draft.as_mut() else { return };
        if depth == d.depth + 1 {
            if let Some(f) = d.field.take() {
                d.commit(f);
            }
            return;
        }
        if depth != d.depth {
            return;
        }
        if let Some(d) = self.draft.take() {
            match d.finish() {
                Ok(item) => {
                    self.out.kept += 1;
                    emit(item);
                }
                Err(skip) => skip(&mut self.out.skipped),
            }
        }
    }

    /// True when text here is kept: inside a field, not deeper.
    fn collecting(&self) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|d| d.field.is_some() && self.depth == d.depth + 1)
    }

    /// Text at the current position: kept only inside a field.
    fn text(&mut self, s: &str) {
        let max = self.limits.max_text;
        let Some(d) = self.draft.as_mut() else { return };
        if d.field.is_none() || self.depth != d.depth + 1 {
            return;
        }
        if d.buf.len() + s.len() > max {
            d.bad = Some(Bad::OverCap);
            d.field = None;
            d.buf = String::new();
        } else {
            d.buf.push_str(s);
        }
    }

    fn spoil(&mut self, bad: Bad) {
        if let Some(d) = self.draft.as_mut() {
            d.bad.get_or_insert(bad);
            d.field = None;
        }
    }
}

/// Reads `e`'s attributes within the caps, handing each to `f`.
fn attrs(e: &BytesStart<'_>, limits: &Limits, mut f: impl FnMut(&str, &str)) -> Result<(), Bad> {
    for (i, a) in e.attributes().enumerate() {
        if i >= limits.max_attrs {
            return Err(Bad::OverCap);
        }
        let a = a.map_err(|_| Bad::Malformed)?;
        if a.value.len() > limits.max_attr_len {
            return Err(Bad::OverCap);
        }
        let v = a
            .normalized_value_with(XmlVersion::Implicit1_0, 1, resolve_xml_entity)
            .map_err(|_| Bad::Malformed)?;
        f(a.key.as_ref(), &v);
    }
    Ok(())
}

impl Draft {
    fn commit(&mut self, f: Field) {
        let s = std::mem::take(&mut self.buf);
        let t = s.trim();
        let it = &mut self.item;
        match f {
            _ if t.is_empty() => {}
            Field::Title if it.title.is_none() => it.title = Some(clean_title(t)),
            Field::Guid if it.guid.is_none() => it.guid = Some(t.to_owned()),
            Field::Link if it.link.is_none() => it.link = Some(t.to_owned()),
            Field::Size => self.size_el = self.size_el.or(t.parse().ok()),
            Field::PubDate => it.published = it.published.or(rfc2822(t)),
            Field::Category => push_category(&mut it.categories, t),
            _ => {}
        }
    }

    /// One `torznab:attr`/`newznab:attr`; the first of each name wins.
    fn attr(&mut self, name: &str, v: &str) {
        let it = &mut self.item;
        let num = |slot: &mut Option<u32>| *slot = slot.or(v.parse().ok());
        let factor = |slot: &mut Option<f64>| {
            let f = v.parse::<f64>().ok().filter(|f| (0.0..=1000.0).contains(f));
            *slot = slot.or(f);
        };
        match name {
            "infohash" => {
                self.infohash.get_or_insert_with(|| hash(v));
            }
            "magneturl" => {
                self.magnet.get_or_insert_with(|| v.to_owned());
            }
            "size" => self.size_attr = self.size_attr.or(v.parse().ok()),
            "category" => push_category(&mut it.categories, v),
            "seeders" => num(&mut it.seeders),
            "leechers" => num(&mut it.leechers),
            "peers" => num(&mut it.peers),
            "files" => num(&mut it.files),
            "downloadvolumefactor" => factor(&mut it.download_factor),
            "uploadvolumefactor" => factor(&mut it.upload_factor),
            "tag" => {
                let fits = it.tags.len() < MAX_TAGS && !v.is_empty() && v.len() <= MAX_TAG;
                if fits && !v.chars().any(char::is_control) && !it.tags.iter().any(|t| t == v) {
                    it.tags.push(v.to_owned());
                }
            }
            "imdb" | "imdbid" => push_id(&mut it.ids, "imdb", v),
            "tmdbid" => push_id(&mut it.ids, "tmdb", v),
            "tvdbid" => push_id(&mut it.ids, "tvdb", v),
            _ => {}
        }
    }

    /// The finished item, or the counter it's skipped under.
    fn finish(self) -> Result<Item, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap += 1),
            Some(Bad::Malformed) => return Err(|s| s.malformed += 1),
            None => {}
        }
        let it = self.item;
        let magnet = [&self.magnet, &it.link, &self.enclosure, &it.guid]
            .into_iter()
            .flatten()
            .find(|s| is_magnet(s));
        let from_magnet = magnet.and_then(|m| magnet_hashes(m));
        let kind = match (self.infohash, magnet) {
            (Some(h), _) => h.map(|h| Kind::Torrent(merge(h, from_magnet))),
            (None, Some(_)) => from_magnet.map(Kind::Torrent),
            (None, None) if self.newznab && !self.torznab => Some(Kind::Usenet),
            (None, None) => return Err(|s| s.no_hash += 1),
        };
        let Some(kind) = kind else {
            return Err(|s| s.bad_hash += 1);
        };
        Ok(Item {
            kind,
            title: it.title.filter(|t| !t.is_empty()),
            size: self.size_attr.or(self.size_el).or(self.size_enc),
            categories: it.categories,
            published: it.published,
            seeders: it.seeders,
            leechers: it.leechers,
            peers: it.peers,
            files: it.files,
            download_factor: it.download_factor,
            upload_factor: it.upload_factor,
            tags: it.tags,
            ids: it.ids,
            guid: it.guid.as_deref().and_then(external_ref),
            link: it.link.as_deref().and_then(external_ref),
        })
    }
}

/// The attribute's hash, with the magnet's other version when the two
/// agree on the version they share: Prowlarr sends `infohash` (v1) and a
/// hybrid's v2 only in the magnet.
fn merge(mut h: Hashes, magnet: Option<Hashes>) -> Hashes {
    let Some(m) = magnet else { return h };
    let v1 = h.v1.is_none() || m.v1.is_none() || h.v1 == m.v1;
    let v2 = h.v2.is_none() || m.v2.is_none() || h.v2 == m.v2;
    if v1 && v2 {
        h.v1 = h.v1.or(m.v1);
        h.v2 = h.v2.or(m.v2);
    }
    h
}

/// A title with control characters removed, cut to [`MAX_TITLE`] bytes
/// on a character boundary.
fn clean_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TITLE));
    for c in s.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_TITLE {
            break;
        }
        out.push(c);
    }
    out
}

fn push_category(cats: &mut Vec<u32>, v: &str) {
    if let Ok(c) = v.trim().parse::<u32>()
        && cats.len() < MAX_CATEGORIES
        && !cats.contains(&c)
    {
        cats.push(c);
    }
}

/// An external ID: ASCII alphanumerics, not all zeros (Jackett writes
/// `0` for unknown). A bare IMDb number gets its `tt`.
fn push_id(ids: &mut Vec<(&'static str, String)>, scheme: &'static str, v: &str) {
    let ok = !v.is_empty()
        && v.len() <= MAX_ID
        && v.bytes().all(|b| b.is_ascii_alphanumeric())
        && v.bytes().any(|b| b.is_ascii_alphanumeric() && b != b'0');
    if !ok || ids.iter().any(|(s, _)| *s == scheme) {
        return;
    }
    let v = if scheme == "imdb" && v.bytes().all(|b| b.is_ascii_digit()) {
        format!("tt{v:0>7}")
    } else {
        v.to_ascii_lowercase()
    };
    ids.push((scheme, v));
}

fn is_magnet(s: &str) -> bool {
    s.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
}

/// An `infohash` value: 40 hex digits or 32 base32 characters (v1), or
/// 64 hex digits (v2).
fn hash(s: &str) -> Option<Hashes> {
    let (v1, v2) = match s.len() {
        40 => (Some(s.parse().ok()?), None),
        32 => (Some(InfoHash(base32(s)?)), None),
        64 => (None, Some(s.parse().ok()?)),
        _ => return None,
    };
    Some(Hashes { v1, v2 })
}

/// The hashes in a magnet's `xt` parameters: `urn:btih:` (v1, hex or
/// base32) and `urn:btmh:1220…` (v2, a SHA-256 multihash).
fn magnet_hashes(m: &str) -> Option<Hashes> {
    let query = m.split_once('?')?.1;
    let mut h = Hashes { v1: None, v2: None };
    for part in query.split('&') {
        let Some(xt) = part
            .get(..3)
            .filter(|p| p.eq_ignore_ascii_case("xt="))
            .map(|_| &part[3..])
        else {
            continue;
        };
        let Some(xt) = percent_decode(xt) else {
            continue;
        };
        let urn = |p: &str| xt.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p));
        if urn("urn:btih:") && h.v1.is_none() {
            let v = &xt[9..];
            h.v1 = match v.len() {
                40 => v.parse().ok(),
                32 => base32(v).map(InfoHash),
                _ => None,
            };
        } else if urn("urn:btmh:1220") && h.v2.is_none() {
            h.v2 = xt[13..].parse().ok();
        }
    }
    (h.v1.is_some() || h.v2.is_some()).then_some(h)
}

/// RFC 4648 base32 (case-insensitive, no padding) of exactly 20 bytes.
fn base32(s: &str) -> Option<[u8; 20]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 20];
    let (mut acc, mut bits, mut n) = (0u64, 0u32, 0usize);
    for b in s.bytes() {
        let v = match b.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[n] = (acc >> bits) as u8;
            n += 1;
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_owned());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A GUID or link fit to store as a provenance reference, or `None`.
///
/// Magnets are left out (the hash is the identity, and private trackers
/// put passkeys in their `tr` URLs), as are references over [`MAX_REF`]
/// bytes and ones with control characters. In an http(s) URL the user
/// info and every query parameter whose name looks like a credential
/// (`apikey`, `passkey`, `torrent_pass`, `token`, `auth`, …) are removed:
/// Prowlarr and Jackett download links carry the manager's API key.
/// Parameters split on `&` and `;`, and names are percent-decoded first.
/// A reference that isn't http(s) is kept only when it's an opaque id of
/// ASCII letters, digits and `.:_-`. Credentials in a path can't be told
/// apart and stay; private indexers' items are `private_source` and
/// never leave the machine (§18.7).
pub fn external_ref(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_REF || is_magnet(s) || s.chars().any(char::is_control) {
        return None;
    }
    let scheme_len = ["http://", "https://"]
        .into_iter()
        .find(|p| s.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p)))
        .map(str::len);
    let Some(scheme_len) = scheme_len else {
        // Anything else is kept only as an opaque id (`urn:uuid:…`, a
        // number): no query to hide a key in, nothing a page could run.
        let opaque = s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(&b));
        return opaque.then(|| s.to_owned());
    };
    let (scheme, rest) = s.split_at(scheme_len);
    let rest = rest.split('#').next().unwrap_or_default();
    // The authority first: a password may hold `&` or `=`.
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(auth_end);
    let host = authority.rsplit('@').next().unwrap_or_default();
    // nZEDb-style links start the query with `&` (`/getnzb/x.nzb&i=1&r=key`).
    let (path, query) = match rest.find(['?', '&']) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let mut out = format!("{scheme}{host}{path}");
    let kept: Vec<&str> = query
        .into_iter()
        .flat_map(|q| q.split(['&', ';']))
        .filter(|p| {
            let name = p.split('=').next().unwrap_or_default();
            // A name that doesn't decode can't be checked, so it goes.
            !p.is_empty() && percent_decode(name).is_some_and(|n| !is_credential(&n))
        })
        .collect();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    Some(out)
}

fn is_credential(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "key", "token", "auth", "pass", "secret", "sign", "session", "cookie",
    ]
    .iter()
    .any(|w| n.contains(w))
        || matches!(
            n.as_str(),
            "sid" | "uid" | "sig" | "pid" | "user" | "u" | "i" | "r" | "k" | "pk" | "h" | "hash"
        )
}

/// An RFC 822/2822 date (`Sun, 01 Nov 2026 12:34:56 +0000`) in Unix
/// seconds. The weekday is optional and unchecked; seconds are optional;
/// a two-digit year is 19xx or 20xx; zones are numeric or `GMT`/`UT`/
/// `UTC`/`Z` (indexers don't write the obsolete letter zones). Anything
/// else is `None`.
fn rfc2822(s: &str) -> Option<i64> {
    let s = s.split_once(',').map_or(s, |(_, rest)| rest);
    let mut it = s.split_ascii_whitespace();
    let day: i64 = small(it.next()?, 2)?;
    let month = month(it.next()?)?;
    let y = it.next()?;
    let year: i64 = match y.len() {
        4 => small(y, 4)?,
        2 => small(y, 2).map(|y: i64| if y < 50 { 2000 + y } else { 1900 + y })?,
        _ => return None,
    };
    let mut hms = it.next()?.split(':');
    let h: i64 = small(hms.next()?, 2)?;
    let m: i64 = small(hms.next()?, 2)?;
    let sec: i64 = hms.next().map_or(Some(0), |x| small(x, 2))?;
    if hms.next().is_some() || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    let offset = zone(it.next().unwrap_or("GMT"))?;
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + h * 3600 + m * 60 + sec.min(59) - offset)
}

/// Up to `max` ASCII digits.
fn small(s: &str, max: usize) -> Option<i64> {
    if s.is_empty() || s.len() > max || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn month(s: &str) -> Option<i64> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let p = M.iter().position(|m| s.eq_ignore_ascii_case(m))?;
    Some(p as i64 + 1)
}

/// A zone's offset east of UTC, in seconds.
fn zone(z: &str) -> Option<i64> {
    if ["GMT", "UT", "UTC", "Z"]
        .iter()
        .any(|g| z.eq_ignore_ascii_case(g))
    {
        return Some(0);
    }
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    let hh = small(digits.get(..2)?, 2)?;
    let mm = small(digits.get(2..)?, 2)?;
    (digits.len() == 4 && hh <= 23 && mm <= 59).then_some(sign * (hh * 3600 + mm * 60))
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(rfc2822("Thu, 01 Jan 1970 00:00:00 +0000"), Some(0));
        assert_eq!(
            rfc2822("Sun, 01 Nov 2026 12:34:56 +0000"),
            Some(1_793_536_496)
        );
        assert_eq!(rfc2822("1 Nov 2026 13:34:56 +0100"), Some(1_793_536_496));
        assert_eq!(rfc2822("Sun, 01 Nov 2026 07:34 -0500"), Some(1_793_536_440));
        assert_eq!(rfc2822("29 Feb 2023 00:00:00 GMT"), None);
        assert!(rfc2822("29 Feb 2024 00:00:00 GMT").is_some());
        assert_eq!(rfc2822("01 Nov 2026 24:00:00 GMT"), None);
        assert_eq!(rfc2822("2026-11-01T12:00:00Z"), None);
        assert_eq!(rfc2822("01 Nov 2026 12:00:00 +99999"), None);
        assert_eq!(rfc2822(""), None);
    }

    #[test]
    fn hashes() {
        let hex = "c9e15763f722f23e98a29decdfae341b98d53056";
        let h: InfoHash = hex.parse().unwrap();
        let v1 = Some(Hashes {
            v1: Some(h),
            v2: None,
        });
        assert_eq!(hash(hex), v1);
        assert_eq!(hash("ZHQVOY7XELZD5GFCTXWN7LRUDOMNKMCW"), v1);
        assert_eq!(hash("zhqvoy7xelzd5gfctxwn7lrudomnkmcw"), v1);
        assert_eq!(hash("ZHQVOY7XELZD5GFCTXWN7LRUDOMNKMC1"), None);
        assert_eq!(hash(&hex[1..]), None);
        let v2 = "a".repeat(64);
        let only_v2 = hash(&v2).unwrap();
        assert_eq!(
            (only_v2.v1, only_v2.v2.map(|h| h.0)),
            (None, Some([0xaa; 32]))
        );
        assert_eq!(only_v2.key(), Some(InfoHash([0xaa; 20])));
        let m = format!("magnet:?xt=urn%3Abtih%3A{hex}&dn=x&xt=urn:btmh:1220{v2}");
        let got = magnet_hashes(&m).unwrap();
        assert_eq!(got.v1, Some(h));
        assert_eq!(got.v2.map(|h| h.0), Some([0xaa; 32]));
        assert_eq!(magnet_hashes("magnet:?xt=urn:btih:nope"), None);
        assert_eq!(magnet_hashes("magnet:?xt=urn:btih:%zz"), None);
        assert_eq!(percent_decode("%+f"), None);
        let v2h: InfoHashV2 = v2.parse().unwrap();
        let attr = Hashes {
            v1: Some(h),
            v2: None,
        };
        let both = Hashes {
            v1: Some(h),
            v2: Some(v2h),
        };
        assert_eq!(merge(attr, Some(both)), both);
        let other = Some(Hashes {
            v1: Some(InfoHash([1; 20])),
            v2: Some(v2h),
        });
        assert_eq!(
            merge(attr, other),
            attr,
            "a disagreeing magnet adds nothing"
        );
    }

    #[test]
    fn references_lose_credentials() {
        assert_eq!(
            external_ref(
                "http://u:p@prowlarr.local:9696/1/download?apikey=SECRET&link=abc&file=X#f"
            )
            .as_deref(),
            Some("http://prowlarr.local:9696/1/download?link=abc&file=X")
        );
        assert_eq!(
            external_ref(
                "https://t.example/details.php?id=7&passkey=S&torrent_pass=S&jackett_apikey=S"
            )
            .as_deref(),
            Some("https://t.example/details.php?id=7")
        );
        assert_eq!(
            external_ref("HTTP://a/?token=x").as_deref(),
            Some("HTTP://a/")
        );
        assert_eq!(
            external_ref("https://nzb.example/getnzb/ab.nzb&i=1&r=KEY").as_deref(),
            Some("https://nzb.example/getnzb/ab.nzb")
        );
        assert_eq!(
            external_ref("http://user:p&ss=x@h.example/d?id=1").as_deref(),
            Some("http://h.example/d?id=1")
        );
        assert_eq!(
            external_ref("https://t.example/d?id=1;passkey=S&api%6Bey=S&hash=S").as_deref(),
            Some("https://t.example/d?id=1")
        );
        assert_eq!(external_ref("//h.example/dl?apikey=K"), None);
        assert_eq!(external_ref("javascript:alert(1)"), None);
        assert_eq!(
            external_ref("urn:uuid:1234").as_deref(),
            Some("urn:uuid:1234")
        );
        assert_eq!(external_ref("magnet:?xt=urn:btih:00"), None);
        assert_eq!(external_ref(&"a".repeat(MAX_REF + 1)), None);
        assert_eq!(external_ref("a\u{1}b"), None);
    }
}
