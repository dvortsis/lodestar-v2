//! RSS 2.0 items and Atom entries that name torrents (architecture §18.2):
//! Nyaa's RSS with its `nyaa:` namespace, AnimeTosho's feeds, and any feed
//! whose items carry an info hash or a magnet.
//!
//! The safety rules are [`torznab`](crate::torznab)'s, with the same
//! [`Limits`]: it streams one item at a time, refuses DTDs, resolves only
//! character references and the five XML entities, stops at a structural
//! cap with a [`Stop`] reason, and skips (and counts) an item that breaks a
//! field cap or has no usable hash. It never panics.
//!
//! Unlike `torznab`, element names are matched by **namespace URI**, not
//! by prefix: `<x:infoHash xmlns:x="https://nyaa.si/xmlns/nyaa">` is
//! Nyaa's, and `<nyaa:infoHash>` with `nyaa` bound elsewhere, or not bound
//! at all, is ignored. RSS elements are in no namespace and Atom's in
//! [`ATOM_NS`]. Only the bindings that can matter are tracked (a known
//! URI, the default namespace, or a prefix that shadows one); more than
//! [`MAX_NS`] in scope stops the parse ([`Stop::Namespaces`]).

use crate::common::{
    Bad, attrs, clean_title, hash, is_magnet, magnet_hashes, merge, prepare, resolve_ref, rfc2822,
    rfc3339,
};
pub use crate::common::{
    Hashes, LIMITS, Limits, MAX_REF, MAX_TITLE, Skipped, Stop, Summary, external_ref,
};
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// The namespace of Nyaa's RSS elements (`nyaa:infoHash`, `nyaa:seeders`, …).
pub const NYAA_NS: &str = "https://nyaa.si/xmlns/nyaa";
/// The Atom 1.0 namespace.
pub const ATOM_NS: &str = "http://www.w3.org/2005/Atom";
/// Tracked namespace bindings in scope at once. A real feed has a few.
pub const MAX_NS: usize = 32;
/// Bytes in a kept category id (Nyaa's `1_2`); longer ones are ignored.
pub const MAX_CATEGORY: usize = 16;

/// One RSS item or Atom entry, as the feed stated it. Every field is a claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub hashes: Hashes,
    /// At most [`MAX_TITLE`] bytes, with control characters and the bidi
    /// controls `torznab` strips removed (U+202A–202E, U+2066–2069, U+200E/F, U+061C).
    pub title: Option<String>,
    /// Content bytes (`nyaa:size`, `1.2 GiB`). An enclosure's `length` is
    /// the size of the linked file, so it isn't used.
    pub size: Option<u64>,
    /// `pubDate`, or Atom's `published` else `updated`, in Unix seconds.
    pub published: Option<i64>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    /// Completed downloads (`nyaa:downloads`).
    pub downloads: Option<u32>,
    /// Nyaa's category id (`1_2`): ASCII digits and `_`.
    pub category: Option<String>,
    /// Nyaa's trusted and remake flags.
    pub trusted: Option<bool>,
    pub remake: Option<bool>,
    /// The external reference: RSS `guid` or Atom `id`, and RSS `link` or
    /// Atom's alternate link, through [`external_ref`].
    pub guid: Option<String>,
    pub link: Option<String>,
}

/// Every kept item and the summary. `summary.error` is always `None`.
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
        ns: Vec::new(),
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

/// The namespaces the parser tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ns {
    None,
    Atom,
    Nyaa,
    Other,
}

impl Ns {
    fn of(uri: &str) -> Ns {
        match uri {
            "" => Ns::None,
            ATOM_NS => Ns::Atom,
            NYAA_NS => Ns::Nyaa,
            _ => Ns::Other,
        }
    }
}

/// `xmlns:prefix` (or `xmlns` when `prefix` is empty) declared at `depth`.
struct Binding {
    depth: usize,
    prefix: Box<str>,
    ns: Ns,
}

/// A field whose text is being collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Guid,
    Link,
    PubDate,
    Published,
    Updated,
    InfoHash,
    Seeders,
    Leechers,
    Downloads,
    Size,
    Category,
    Trusted,
    Remake,
}

/// The item being read.
#[derive(Debug, Default)]
struct Draft {
    depth: usize,
    bad: Option<Bad>,
    field: Option<Field>,
    buf: String,
    /// `nyaa:infoHash`: `Some(None)` when present but invalid.
    infohash: Option<Option<Hashes>>,
    /// The first magnet in an Atom link.
    magnet: Option<String>,
    /// The first enclosure URL (RSS, or Atom `rel="enclosure"`).
    enclosure: Option<String>,
    updated: Option<i64>,
    it: Fields,
}

#[derive(Debug, Default)]
struct Fields {
    title: Option<String>,
    guid: Option<String>,
    link: Option<String>,
    size: Option<u64>,
    published: Option<i64>,
    seeders: Option<u32>,
    leechers: Option<u32>,
    downloads: Option<u32>,
    category: Option<String>,
    trusted: Option<bool>,
    remake: Option<bool>,
}

struct Parser<'l> {
    limits: &'l Limits,
    out: Summary,
    depth: usize,
    elements: usize,
    items: usize,
    ns: Vec<Binding>,
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
                match resolve_ref(&r) {
                    Some(s) => self.text(&s),
                    None => self.spoil(Bad::Malformed),
                }
                Ok(())
            }
            Event::DocType(_) => Err(Stop::Doctype),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::Eof => Ok(()),
        }
    }

    /// Records the element's namespace declarations that can matter.
    fn declare(&mut self, e: &BytesStart<'_>) -> Result<(), Stop> {
        // Without checks the scan is linear in the attributes; duplicate
        // declarations just shadow each other.
        for a in e.attributes().with_checks(false) {
            let a = a.map_err(|_| Stop::Malformed)?;
            let key = a.key.as_ref();
            let prefix = match key.strip_prefix("xmlns") {
                Some("") => "",
                Some(p) => match p.strip_prefix(':') {
                    Some(p) => p,
                    None => continue,
                },
                _ => continue,
            };
            // An over-long URI can't be one we know, so isn't decoded.
            let ns = if a.value.len() > self.limits.max_attr_len {
                Ns::Other
            } else {
                let v = a
                    .normalized_value_with(XmlVersion::Implicit1_0, 1, resolve_xml_entity)
                    .map_err(|_| Stop::Malformed)?;
                Ns::of(&v)
            };
            let shadows = self.ns.iter().any(|b| *b.prefix == *prefix);
            if ns != Ns::Other || prefix.is_empty() || shadows {
                if self.ns.len() >= MAX_NS {
                    return Err(Stop::Namespaces);
                }
                self.ns.push(Binding {
                    depth: self.depth,
                    prefix: prefix.into(),
                    ns,
                });
            }
        }
        Ok(())
    }

    /// The namespace a prefix is bound to here. An unbound prefix is
    /// [`Ns::Other`]; no prefix, unless a default is declared, is none.
    fn resolve(&self, prefix: &str) -> Ns {
        match self.ns.iter().rev().find(|b| *b.prefix == *prefix) {
            Some(b) => b.ns,
            None if prefix.is_empty() => Ns::None,
            None => Ns::Other,
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
        self.declare(e)?;
        let name = e.name();
        let (local, prefix) = name.decompose();
        let ns = self.resolve(prefix.as_ref().map_or("", |p| p.as_ref()));
        let local = local.as_ref();
        let Some(d) = self.draft.as_mut() else {
            if matches!((ns, local), (Ns::None, "item") | (Ns::Atom, "entry")) {
                self.items += 1;
                if self.items > self.limits.max_items {
                    return Err(Stop::Items);
                }
                self.draft = Some(Draft {
                    depth: self.depth,
                    ..Draft::default()
                });
            }
            return Ok(());
        };
        if self.depth != d.depth + 1 || d.bad.is_some() {
            return Ok(());
        }
        let field = match (ns, local) {
            (Ns::None | Ns::Atom, "title") => Some(Field::Title),
            (Ns::None, "guid") | (Ns::Atom, "id") => Some(Field::Guid),
            (Ns::None, "link") => Some(Field::Link),
            (Ns::None, "pubDate") => Some(Field::PubDate),
            (Ns::Atom, "published") => Some(Field::Published),
            (Ns::Atom, "updated") => Some(Field::Updated),
            (Ns::Nyaa, "infoHash") => Some(Field::InfoHash),
            (Ns::Nyaa, "seeders") => Some(Field::Seeders),
            (Ns::Nyaa, "leechers") => Some(Field::Leechers),
            (Ns::Nyaa, "downloads") => Some(Field::Downloads),
            (Ns::Nyaa, "size") => Some(Field::Size),
            (Ns::Nyaa, "categoryId") => Some(Field::Category),
            (Ns::Nyaa, "trusted") => Some(Field::Trusted),
            (Ns::Nyaa, "remake") => Some(Field::Remake),
            (Ns::None, "enclosure") => {
                let r = attrs(e, self.limits, |k, v| {
                    if k == "url" && d.enclosure.is_none() {
                        d.enclosure = Some(v.trim().to_owned());
                    }
                });
                if let Err(bad) = r {
                    d.bad = Some(bad);
                }
                None
            }
            (Ns::Atom, "link") => {
                let (mut href, mut rel) = (None, None);
                let r = attrs(e, self.limits, |k, v| match k {
                    "href" => href = Some(v.trim().to_owned()),
                    "rel" => rel = Some(v.trim().to_ascii_lowercase()),
                    _ => {}
                });
                match (r, href) {
                    (Err(bad), _) => d.bad = Some(bad),
                    (Ok(()), Some(h)) if is_magnet(&h) => {
                        d.magnet.get_or_insert(h);
                    }
                    (Ok(()), Some(h)) => match rel.as_deref() {
                        None | Some("alternate") => {
                            d.it.link.get_or_insert(h);
                        }
                        Some("enclosure") => {
                            d.enclosure.get_or_insert(h);
                        }
                        _ => {}
                    },
                    _ => {}
                }
                None
            }
            _ => None,
        };
        // A field is collected only while its slot is empty: a repeat
        // costs nothing and can't spoil the item.
        d.field = field.filter(|&f| d.empty(f));
        d.buf.clear();
        Ok(())
    }

    fn close(&mut self, emit: &mut impl FnMut(Item)) {
        let depth = self.depth;
        while self.ns.last().is_some_and(|b| b.depth >= depth) {
            self.ns.pop();
        }
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
    /// Whether the field's slot is still empty; the first value wins.
    fn empty(&self, f: Field) -> bool {
        let it = &self.it;
        match f {
            Field::Title => it.title.is_none(),
            Field::Guid => it.guid.is_none(),
            Field::Link => it.link.is_none(),
            Field::PubDate | Field::Published => it.published.is_none(),
            Field::Updated => self.updated.is_none(),
            Field::InfoHash => self.infohash.is_none(),
            Field::Seeders => it.seeders.is_none(),
            Field::Leechers => it.leechers.is_none(),
            Field::Downloads => it.downloads.is_none(),
            Field::Size => it.size.is_none(),
            Field::Category => it.category.is_none(),
            Field::Trusted => it.trusted.is_none(),
            Field::Remake => it.remake.is_none(),
        }
    }

    fn commit(&mut self, f: Field) {
        let s = std::mem::take(&mut self.buf);
        let t = s.trim();
        if t.is_empty() || !self.empty(f) {
            return;
        }
        let it = &mut self.it;
        match f {
            Field::Title => it.title = Some(clean_title(t)).filter(|t| !t.is_empty()),
            Field::Guid => it.guid = Some(t.to_owned()),
            Field::Link => it.link = Some(t.to_owned()),
            Field::PubDate => it.published = rfc2822(t),
            Field::Published => it.published = rfc3339(t),
            Field::Updated => self.updated = rfc3339(t),
            Field::InfoHash => self.infohash = Some(hash(t)),
            Field::Seeders => it.seeders = t.parse().ok(),
            Field::Leechers => it.leechers = t.parse().ok(),
            Field::Downloads => it.downloads = t.parse().ok(),
            Field::Size => it.size = size(t),
            Field::Category => {
                let ok =
                    t.len() <= MAX_CATEGORY && t.bytes().all(|b| b.is_ascii_digit() || b == b'_');
                it.category = ok.then(|| t.to_owned());
            }
            Field::Trusted => it.trusted = yes_no(t),
            Field::Remake => it.remake = yes_no(t),
        }
    }

    /// The finished item, or the counter it's skipped under.
    fn finish(self) -> Result<Item, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap += 1),
            Some(Bad::Malformed) => return Err(|s| s.malformed += 1),
            None => {}
        }
        let it = self.it;
        let magnet = [&self.magnet, &it.link, &self.enclosure, &it.guid]
            .into_iter()
            .flatten()
            .find(|s| is_magnet(s));
        let from_magnet = magnet.and_then(|m| magnet_hashes(m));
        let hashes = match (self.infohash, magnet) {
            (Some(h), _) => h.map(|h| merge(h, from_magnet)),
            (None, Some(_)) => from_magnet,
            (None, None) => return Err(|s| s.no_hash += 1),
        };
        let Some(hashes) = hashes else {
            return Err(|s| s.bad_hash += 1);
        };
        Ok(Item {
            hashes,
            title: it.title,
            size: it.size,
            published: it.published.or(self.updated),
            seeders: it.seeders,
            leechers: it.leechers,
            downloads: it.downloads,
            category: it.category,
            trusted: it.trusted,
            remake: it.remake,
            guid: it.guid.as_deref().and_then(external_ref),
            link: it.link.as_deref().and_then(external_ref),
        })
    }
}

/// `Yes` or `No`, as Nyaa writes its flags.
fn yes_no(s: &str) -> Option<bool> {
    match s {
        _ if s.eq_ignore_ascii_case("yes") => Some(true),
        _ if s.eq_ignore_ascii_case("no") => Some(false),
        _ => None,
    }
}

/// Nyaa's size: `1.2 GiB`, `512 KiB`, `734 Bytes`, or a bare byte count.
/// Binary units only, as Nyaa writes them; the result is rounded.
fn size(s: &str) -> Option<u64> {
    let (num, unit) = s.split_once(' ').unwrap_or((s, "B"));
    let mult: f64 = match unit.trim() {
        "B" | "Bytes" | "bytes" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "PiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    // Digits and one point only: no `inf`, `NaN`, signs or exponents.
    let plain = !num.is_empty()
        && num.len() <= 24
        && num.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && num.bytes().filter(|&b| b == b'.').count() <= 1;
    if !plain {
        return None;
    }
    if mult == 1.0 {
        return num.parse().ok();
    }
    let bytes = (num.parse::<f64>().ok()? * mult).round();
    // 2^63 is exact in f64, so the cast below can't saturate silently.
    (bytes.is_finite() && bytes < 2f64.powi(63)).then_some(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size("1.5 KiB"), Some(1536));
        assert_eq!(size("1.0 GiB"), Some(1 << 30));
        assert_eq!(size("734 Bytes"), Some(734));
        assert_eq!(size("734"), Some(734));
        assert_eq!(size("1 PiB"), Some(1 << 50));
        for bad in [
            "",
            "inf GiB",
            "NaN MiB",
            "-1 KiB",
            "1e3 KiB",
            "1.2.3 MiB",
            "1 GB",
            "1.5",
            "99999999 PiB",
        ] {
            assert_eq!(size(bad), None, "{bad}");
        }
    }

    #[test]
    fn flags() {
        assert_eq!(yes_no("Yes"), Some(true));
        assert_eq!(yes_no("NO"), Some(false));
        assert_eq!(yes_no("maybe"), None);
    }
}
