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

use crate::common::{
    Bad, attrs, clean_title, hash, is_magnet, magnet_hashes, merge, prepare, resolve_ref, rfc2822,
    unsafe_char,
};
pub use crate::common::{
    Hashes, LIMITS, Limits, MAX_REF, MAX_TITLE, Skipped, Stop, Summary, external_ref,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// Category ids kept per item; more are ignored.
pub const MAX_CATEGORIES: usize = 16;
/// Indexer tags (`freeleech`, `internal`, …) kept per item, and the
/// bytes in each; longer tags are ignored.
pub const MAX_TAGS: usize = 8;
pub const MAX_TAG: usize = 64;
/// Bytes in an external ID's value (`tt0111161`); longer are ignored.
pub const MAX_ID: usize = 32;

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
    let text = prepare(body, limits, &mut out);
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
            // The raw length is checked first: normalising only shrinks
            // text, and it copies.
            Event::Text(t) if self.collecting() => {
                if self.fits(t.len()) {
                    self.text(&t.xml10_content());
                }
                Ok(())
            }
            Event::CData(t) if self.collecting() => {
                if self.fits(t.len()) {
                    self.text(&t.xml10_content());
                }
                Ok(())
            }
            Event::Text(_) | Event::CData(_) => Ok(()),
            Event::GeneralRef(_) if self.draft.is_none() => Ok(()),
            Event::GeneralRef(r) => {
                let resolved = resolve_ref(&r);
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
        // A field is collected only while its slot is empty: a repeat
        // costs nothing and can't spoil the item.
        let it = &d.item;
        d.field = match name {
            "title" if it.title.is_none() => Some(Field::Title),
            "guid" if it.guid.is_none() => Some(Field::Guid),
            "link" if it.link.is_none() => Some(Field::Link),
            "size" if d.size_el.is_none() => Some(Field::Size),
            "pubDate" if it.published.is_none() => Some(Field::PubDate),
            "category" if it.categories.len() < MAX_CATEGORIES => Some(Field::Category),
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
        if !self.collecting() || !self.fits(s.len()) {
            return;
        }
        if let Some(d) = self.draft.as_mut() {
            d.buf.push_str(s);
        }
    }

    /// Whether `n` more bytes fit the field; if not, the item is spoiled.
    fn fits(&mut self, n: usize) -> bool {
        let max = self.limits.max_text;
        let Some(d) = self.draft.as_mut() else {
            return false;
        };
        if d.buf.len() + n <= max {
            return true;
        }
        d.bad = Some(Bad::OverCap);
        d.field = None;
        d.buf = String::new();
        false
    }

    fn spoil(&mut self, bad: Bad) {
        if let Some(d) = self.draft.as_mut() {
            d.bad.get_or_insert(bad);
            d.field = None;
        }
    }
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
                if fits && !v.chars().any(unsafe_char) && !it.tags.iter().any(|t| t == v) {
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
