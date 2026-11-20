//! AniDB's two XML answers (ENR-anidb; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-16-ENR-anidb.md`):
//!
//! - **the titles dump** (`anime-titles.xml.gz`, [`parse_titles_each`]):
//!   every anime's id and titles, fetched at most once a day, so a title is
//!   matched on the install and never sent;
//! - **one anime's record** (the HTTP API's `request=anime`,
//!   [`parse_anime`]): its type, dates, titles and adult marker.
//!
//! Both are untrusted and arrive gzip-compressed, so the parser:
//!
//! - **inflates within a cap** ([`inflate`]): at most
//!   [`Limits::max_compressed`] bytes in and [`XmlLimits::max_bytes`] out,
//!   so a gzip bomb stops at the cap ([`Stop::Inflated`]). A body that
//!   isn't gzip is read as it is, within the same cap;
//! - **refuses DTDs** (a `<!DOCTYPE` stops the parse) and resolves only
//!   character references and the five XML entities, so nothing expands;
//! - **stops at a structural cap** (bytes, depth, elements, anime, titles)
//!   with a [`Stop`], and spoils an anime that breaks a field cap or reads
//!   wrong. [`Page::whole`] is the one rule a plugin applies: an answer is
//!   used only when it was read to its end with nothing spoiled, so a
//!   broken answer is never mistaken for a miss;
//! - **keeps only what the plugin needs:** ids, titles (with their type
//!   and language), the anime's type, its start and end years and its
//!   `restricted` marker. Never a creator, character, description, tag,
//!   rating, episode or picture: those are passed over unread.
//!
//! A pure function from bytes to values: no I/O and no panics.

use std::borrow::Cow;
use std::io::Read;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

pub use crate::common::Limits as XmlLimits;
pub use crate::common::MAX_TITLE;
use crate::common::{Bad, attrs, resolve_ref, title};

/// Bytes in a title's `xml:lang` as AniDB writes it (`x-jat`, `zh-Hans`).
pub const MAX_LANG: usize = 16;

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of the body as received, compressed or not.
    pub max_compressed: usize,
    /// The XML caps, on the inflated text: `max_bytes` is the inflated
    /// size, `max_items` the anime read.
    pub xml: XmlLimits,
    /// Titles kept per anime; the rest are passed over
    /// ([`Anime::truncated`]).
    pub max_titles: usize,
    /// Titles kept in the whole answer ([`Stop::Titles`]).
    pub max_all_titles: usize,
}

/// The titles dump: about 15,000 anime and 100,000 titles, a few MiB
/// compressed and well under 32 MiB inflated.
pub const DUMP_LIMITS: Limits = Limits {
    max_compressed: 16 * 1024 * 1024,
    xml: XmlLimits {
        max_bytes: 64 * 1024 * 1024,
        max_depth: 4,
        max_elements: 2_000_000,
        max_items: 200_000,
        max_attrs: 4,
        max_attr_len: 64,
        max_text: 1024,
    },
    max_titles: 128,
    max_all_titles: 1_000_000,
};

/// One anime's record: a few KiB to a few MiB for a long series (its
/// episodes, characters and creators are passed over, but counted).
pub const ANIME_LIMITS: Limits = Limits {
    max_compressed: 4 * 1024 * 1024,
    xml: XmlLimits {
        max_bytes: 16 * 1024 * 1024,
        max_depth: 8,
        max_elements: 500_000,
        max_items: 1,
        max_attrs: 8,
        max_attr_len: 64,
        max_text: 1024,
    },
    max_titles: 128,
    max_all_titles: 128,
};

/// Why a parse ended before the end of a well-formed document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The body is over [`Limits::max_compressed`].
    Bytes,
    /// Inflated, it's over [`XmlLimits::max_bytes`].
    Inflated,
    /// It starts as gzip but doesn't inflate.
    Gzip,
    NotUtf8,
    Doctype,
    Depth,
    Elements,
    /// Over [`XmlLimits::max_items`] anime.
    Items,
    /// Over [`Limits::max_all_titles`].
    Titles,
    /// Not well-formed XML, including a document cut short, or a root
    /// that isn't the one asked for.
    Malformed,
}

/// An anime's id: 1–10 digits, no leading zero, fitting a `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Aid(pub u32);

impl Aid {
    pub fn parse(s: &str) -> Option<Aid> {
        let ok = (1..=10).contains(&s.len())
            && s.bytes().all(|b| b.is_ascii_digit())
            && !s.starts_with('0');
        ok.then(|| s.parse().ok().map(Aid)).flatten()
    }
}

impl std::fmt::Display for Aid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A title's `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleKind {
    Main,
    Official,
    Synonym,
    Short,
    /// A type the parser doesn't know (`card`, `kana`): kept, never
    /// matched.
    Other,
}

impl TitleKind {
    fn parse(s: &str) -> TitleKind {
        match s {
            "main" => TitleKind::Main,
            "official" => TitleKind::Official,
            "syn" | "synonym" => TitleKind::Synonym,
            "short" => TitleKind::Short,
            _ => TitleKind::Other,
        }
    }
}

/// One title. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub kind: TitleKind,
    /// Its `xml:lang` as written, at most [`MAX_LANG`] bytes of ASCII
    /// letters, digits and `-`; empty when it has none.
    pub lang: String,
    /// Cleaned as [`MAX_TITLE`] says; never empty.
    pub text: String,
}

/// The anime's `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimeType {
    TvSeries,
    TvSpecial,
    Movie,
    Ova,
    Web,
    MusicVideo,
    Other,
    /// A type the parser doesn't know, or `unknown`.
    Unknown,
}

impl AnimeType {
    fn parse(s: &str) -> AnimeType {
        match s {
            "TV Series" => AnimeType::TvSeries,
            "TV Special" => AnimeType::TvSpecial,
            "Movie" => AnimeType::Movie,
            "OVA" => AnimeType::Ova,
            "Web" => AnimeType::Web,
            "Music Video" => AnimeType::MusicVideo,
            "Other" => AnimeType::Other,
            _ => AnimeType::Unknown,
        }
    }
}

/// One anime: a dump entry (id and titles only) or a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anime {
    pub aid: Aid,
    pub titles: Vec<Title>,
    /// The record's `restricted` marker (AniDB's 18+ flag); `None` when
    /// it has none, and always in a dump entry.
    pub restricted: Option<bool>,
    pub kind: Option<AnimeType>,
    /// The years of `startdate` and `enddate` (`YYYY`, `YYYY-MM` or
    /// `YYYY-MM-DD`), 1–9999; `None` when absent or unreadable.
    pub start_year: Option<u16>,
    pub end_year: Option<u16>,
    /// Titles past [`Limits::max_titles`] were passed over.
    pub truncated: bool,
}

/// Anime spoiled, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't an [`Aid`].
    pub bad_id: u32,
    /// A field or attribute over a cap.
    pub over_cap: u32,
    /// Anything outside the shape the parser reads: a repeated field, a
    /// title without a type, an element inside a field, an unknown entity.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.bad_id + self.over_cap + self.malformed
    }
}

/// The error a 200 can carry: `<error>…</error>` as the root. Its text is
/// compared, never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// `Banned`: the client or address is banned for now.
    Banned,
    Other,
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    /// Anime kept.
    pub kept: u32,
    /// Anime spoiled: a bad id, a field over a cap, or a wrong shape.
    pub skipped: Skipped,
    pub stop: Option<Stop>,
    /// The root was the one asked for.
    pub seen: bool,
    pub error: Option<ApiError>,
}

impl Page {
    /// Read to its end, the root asked for, not an error, nothing
    /// spoiled. Only such an answer may be used, as a match or a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none() && self.seen && self.error.is_none() && self.skipped.total() == 0
    }
}

/// `body` inflated when it's gzip (`1f 8b`), as it is otherwise, within
/// `limits`. A gzip bomb stops at [`XmlLimits::max_bytes`] inflated
/// bytes: no more is ever produced.
pub fn inflate<'b>(body: &'b [u8], limits: &Limits) -> Result<Cow<'b, [u8]>, Stop> {
    if body.len() > limits.max_compressed {
        return Err(Stop::Bytes);
    }
    let max = limits.xml.max_bytes;
    if !body.starts_with(&[0x1f, 0x8b]) {
        return if body.len() > max {
            Err(Stop::Inflated)
        } else {
            Ok(Cow::Borrowed(body))
        };
    }
    let mut out = Vec::new();
    let cap = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    flate2::read::GzDecoder::new(body)
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|_| Stop::Gzip)?;
    if out.len() > max {
        return Err(Stop::Inflated);
    }
    Ok(Cow::Owned(out))
}

/// Gzip of `data`, for tests, fixtures and fuzz seeds.
#[doc(hidden)]
pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    // Writing to a Vec can't fail.
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// The titles dump, `<animetitles><anime aid="1"><title type="main"
/// xml:lang="x-jat">…</title>…</anime>…</animetitles>`, handing each kept
/// anime to `emit` as soon as it ends, so the whole dump is never held as
/// values. Only ids and titles are read.
pub fn parse_titles_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Anime)) -> Page {
    run(body, limits, Shape::Dump, &mut emit)
}

/// [`parse_titles_each`], collected.
pub fn parse_titles(body: &[u8], limits: &Limits) -> (Vec<Anime>, Page) {
    let mut all = Vec::new();
    let page = parse_titles_each(body, limits, |a| all.push(a));
    (all, page)
}

/// One anime's record, `<anime id="1" restricted="false"><type>…</type>
/// <startdate>…</startdate><titles><title …>…</title></titles>…</anime>`,
/// or `<error>…</error>`.
pub fn parse_anime(body: &[u8], limits: &Limits) -> (Option<Anime>, Page) {
    let mut one = None;
    let page = run(body, limits, Shape::Record, &mut |a| one = Some(a));
    (one, page)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Dump,
    Record,
}

/// A field whose text is being collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title(TitleKind),
    Type,
    Start,
    End,
    /// The root `<error>`.
    Error,
}

/// The anime being read.
#[derive(Debug)]
struct Draft {
    anime: Anime,
    bad: Option<Bad>,
    /// Its id is missing or bad.
    bad_id: bool,
    /// The record's fields already read, so a repeat spoils it.
    seen: u8,
    /// Inside `<titles>` (record) at this depth.
    titles_at: Option<usize>,
}

struct Parser<'l> {
    l: &'l Limits,
    shape: Shape,
    page: Page,
    depth: usize,
    elements: usize,
    items: usize,
    all_titles: usize,
    draft: Option<Draft>,
    /// The field and the depth of its element.
    field: Option<(Field, usize)>,
    /// The field's language (titles).
    lang: String,
    buf: String,
    /// The field holds an element: it's malformed.
    nested: bool,
    /// A field's text over the cap.
    over: bool,
    /// The root `<error>`'s text matched so far.
    error_text: Option<String>,
}

fn run(body: &[u8], limits: &Limits, shape: Shape, emit: &mut dyn FnMut(Anime)) -> Page {
    let mut p = Parser {
        l: limits,
        shape,
        page: Page::default(),
        depth: 0,
        elements: 0,
        items: 0,
        all_titles: 0,
        draft: None,
        field: None,
        lang: String::new(),
        buf: String::new(),
        nested: false,
        over: false,
        error_text: None,
    };
    let inflated = match inflate(body, limits) {
        Ok(b) => b,
        Err(stop) => {
            p.page.stop = Some(stop);
            return p.page;
        }
    };
    let Ok(text) = std::str::from_utf8(&inflated) else {
        p.page.stop = Some(Stop::NotUtf8);
        return p.page;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    loop {
        let flow = match reader.read_event() {
            Ok(Event::Eof) if p.depth == 0 => break,
            Ok(Event::Eof) | Err(_) => Err(Stop::Malformed),
            Ok(ev) => p.event(ev, emit),
        };
        if let Err(stop) = flow {
            p.page.stop.get_or_insert(stop);
            break;
        }
    }
    if p.error_text.is_some() {
        p.page.error = Some(match p.error_text.as_deref().map(str::trim) {
            Some(t) if t.eq_ignore_ascii_case("banned") => ApiError::Banned,
            _ => ApiError::Other,
        });
    }
    p.page
}

const SEEN_TYPE: u8 = 1;
const SEEN_START: u8 = 2;
const SEEN_END: u8 = 4;
const SEEN_TITLES: u8 = 8;

impl Parser<'_> {
    fn event(&mut self, ev: Event<'_>, emit: &mut dyn FnMut(Anime)) -> Result<(), Stop> {
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
            Event::Text(t) => {
                if self.collecting() && self.fits(t.len()) {
                    self.text(&t.xml10_content());
                }
                Ok(())
            }
            Event::CData(t) => {
                if self.collecting() && self.fits(t.len()) {
                    self.text(&t.xml10_content());
                }
                Ok(())
            }
            Event::GeneralRef(r) => {
                if self.collecting() {
                    match resolve_ref(&r) {
                        Some(s) => self.text(&s),
                        None => self.spoil(Bad::Malformed),
                    }
                }
                Ok(())
            }
            Event::DocType(_) => Err(Stop::Doctype),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::Eof => Ok(()),
        }
    }

    fn open(&mut self, e: &BytesStart<'_>) -> Result<(), Stop> {
        self.elements += 1;
        if self.elements > self.l.xml.max_elements {
            return Err(Stop::Elements);
        }
        self.depth += 1;
        if self.depth > self.l.xml.max_depth {
            return Err(Stop::Depth);
        }
        // An element inside a field: the field is malformed.
        if self.field.is_some() {
            self.nested = true;
            return Ok(());
        }
        let name = e.name();
        let name = name.as_ref();
        if self.depth == 1 {
            return self.root(name, e);
        }
        let anime_depth = match self.shape {
            Shape::Dump => 2,
            Shape::Record => 1,
        };
        if self.shape == Shape::Dump && self.depth == 2 && name == "anime" {
            return self.begin(e, "aid");
        }
        let depth = self.depth;
        let Some(d) = self.draft.as_mut() else {
            return Ok(());
        };
        if d.bad.is_some() {
            return Ok(());
        }
        let title_depth = match (self.shape, d.titles_at) {
            (Shape::Dump, _) => Some(anime_depth + 1),
            (Shape::Record, Some(t)) => Some(t + 1),
            (Shape::Record, None) => None,
        };
        if Some(depth) == title_depth && name == "title" {
            return self.title(e);
        }
        if self.shape == Shape::Record && depth == anime_depth + 1 {
            let (bit, field) = match name {
                "type" => (SEEN_TYPE, Some(Field::Type)),
                "startdate" => (SEEN_START, Some(Field::Start)),
                "enddate" => (SEEN_END, Some(Field::End)),
                "titles" => (SEEN_TITLES, None),
                _ => return Ok(()),
            };
            if d.seen & bit != 0 {
                d.bad = Some(Bad::Malformed);
                return Ok(());
            }
            d.seen |= bit;
            match field {
                Some(f) => self.start_field(f),
                None => d.titles_at = Some(depth),
            }
        }
        Ok(())
    }

    /// The document's root: the one the shape asks for, or a record's
    /// `<error>`. Anything else is malformed.
    fn root(&mut self, name: &str, e: &BytesStart<'_>) -> Result<(), Stop> {
        // One root only.
        if self.page.seen || self.error_text.is_some() {
            return Err(Stop::Malformed);
        }
        match (self.shape, name) {
            (Shape::Dump, "animetitles") => {
                self.page.seen = true;
                Ok(())
            }
            (Shape::Record, "anime") => {
                self.page.seen = true;
                self.begin(e, "id")
            }
            (Shape::Record, "error") => {
                self.error_text = Some(String::new());
                self.start_field(Field::Error);
                Ok(())
            }
            _ => Err(Stop::Malformed),
        }
    }

    /// An `<anime>`: its id from `key`, and for a record its marker.
    fn begin(&mut self, e: &BytesStart<'_>, key: &str) -> Result<(), Stop> {
        self.items += 1;
        if self.items > self.l.xml.max_items {
            return Err(Stop::Items);
        }
        let (mut id, mut restricted, mut bad) = (None, None, None);
        if let Err(b) = attrs(e, &self.l.xml, |k, v| {
            if k == key {
                id = Some(Aid::parse(v));
            } else if k == "restricted" && self.shape == Shape::Record {
                restricted = Some(match v {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                });
            }
        }) {
            bad = Some(b);
        }
        let restricted = match restricted {
            Some(None) => {
                bad.get_or_insert(Bad::Malformed);
                None
            }
            Some(r) => r,
            None => None,
        };
        let aid = match id {
            Some(Some(aid)) => aid,
            // No id, or one that isn't: the anime can't be used.
            _ => {
                self.draft = Some(Draft {
                    anime: blank(Aid(0)),
                    bad: Some(Bad::Malformed),
                    bad_id: true,
                    seen: 0,
                    titles_at: None,
                });
                return Ok(());
            }
        };
        self.draft = Some(Draft {
            anime: Anime {
                restricted,
                ..blank(aid)
            },
            bad,
            bad_id: false,
            seen: 0,
            titles_at: None,
        });
        Ok(())
    }

    fn title(&mut self, e: &BytesStart<'_>) -> Result<(), Stop> {
        let (mut kind, mut lang) = (None, String::new());
        let mut lang_bad = false;
        let r = attrs(e, &self.l.xml, |k, v| match k {
            "type" => kind = Some(TitleKind::parse(v)),
            "xml:lang" => {
                lang_bad = v.len() > MAX_LANG
                    || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
                v.clone_into(&mut lang);
            }
            _ => {}
        });
        let Some(d) = self.draft.as_mut() else {
            return Ok(());
        };
        match (r, kind) {
            (Err(b), _) => d.bad = Some(b),
            (Ok(()), None) => d.bad = Some(Bad::Malformed),
            _ if lang_bad => d.bad = Some(Bad::Malformed),
            (Ok(()), Some(kind)) => {
                if d.anime.titles.len() >= self.l.max_titles {
                    d.anime.truncated = true;
                    return Ok(());
                }
                self.all_titles += 1;
                if self.all_titles > self.l.max_all_titles {
                    return Err(Stop::Titles);
                }
                self.lang = lang;
                self.start_field(Field::Title(kind));
            }
        }
        Ok(())
    }

    fn start_field(&mut self, f: Field) {
        self.field = Some((f, self.depth));
        self.buf.clear();
        self.nested = false;
        self.over = false;
    }

    fn close(&mut self, emit: &mut dyn FnMut(Anime)) {
        let depth = self.depth;
        self.depth = depth.saturating_sub(1);
        if let Some((f, at)) = self.field
            && at == depth
        {
            self.field = None;
            self.commit(f);
            return;
        }
        let Some(d) = self.draft.as_mut() else { return };
        if d.titles_at == Some(depth) {
            d.titles_at = None;
            return;
        }
        let anime_depth = match self.shape {
            Shape::Dump => 2,
            Shape::Record => 1,
        };
        if depth != anime_depth {
            return;
        }
        if let Some(d) = self.draft.take() {
            let s = &mut self.page.skipped;
            match d.bad {
                _ if d.bad_id => s.bad_id += 1,
                None => {
                    self.page.kept += 1;
                    emit(d.anime);
                }
                Some(Bad::OverCap) => s.over_cap += 1,
                Some(Bad::Malformed) => s.malformed += 1,
            }
        }
    }

    fn commit(&mut self, f: Field) {
        let text = std::mem::take(&mut self.buf);
        if f == Field::Error {
            // Only compared, never kept; past the cap it's no match.
            self.error_text = Some(if self.over { String::new() } else { text });
            return;
        }
        let (nested, over) = (self.nested, self.over);
        let Some(d) = self.draft.as_mut() else { return };
        if nested {
            d.bad.get_or_insert(Bad::Malformed);
            return;
        }
        if over {
            d.bad.get_or_insert(Bad::OverCap);
            return;
        }
        match f {
            Field::Title(kind) => match title(&text) {
                Some(text) => d.anime.titles.push(Title {
                    kind,
                    lang: std::mem::take(&mut self.lang),
                    text,
                }),
                None => {
                    d.bad.get_or_insert(Bad::Malformed);
                }
            },
            Field::Type => d.anime.kind = Some(AnimeType::parse(text.trim())),
            Field::Start => d.anime.start_year = year(text.trim()),
            Field::End => d.anime.end_year = year(text.trim()),
            Field::Error => {}
        }
    }

    /// Text here is kept: directly inside a field.
    fn collecting(&self) -> bool {
        self.field.is_some_and(|(_, at)| at == self.depth)
            && self.draft.as_ref().is_none_or(|d| d.bad.is_none())
    }

    /// Whether `n` more bytes fit the field; over the cap marks it.
    fn fits(&mut self, n: usize) -> bool {
        if self.buf.len().saturating_add(n) > self.l.xml.max_text {
            self.over = true;
            self.buf.clear();
            return false;
        }
        true
    }

    fn text(&mut self, s: &str) {
        if !self.over && self.fits(s.len()) {
            self.buf.push_str(s);
        }
    }

    fn spoil(&mut self, bad: Bad) {
        match self.draft.as_mut() {
            Some(d) => {
                d.bad.get_or_insert(bad);
            }
            // An unknown entity in the error's text: no match.
            None => self.over = true,
        }
    }
}

fn blank(aid: Aid) -> Anime {
    Anime {
        aid,
        titles: Vec::new(),
        restricted: None,
        kind: None,
        start_year: None,
        end_year: None,
        truncated: false,
    }
}

/// The year of `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, 1–9999.
fn year(s: &str) -> Option<u16> {
    let y = s.get(..4)?;
    let rest = &s[4..];
    let shape = rest.is_empty()
        || (rest.len() == 3 || rest.len() == 6)
            && rest.bytes().enumerate().all(|(i, b)| {
                if i % 3 == 0 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            });
    if !shape || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    y.parse().ok().filter(|y| (1..=9999).contains(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn years_read_only_their_shapes() {
        assert_eq!(year("1999-01-03"), Some(1999));
        assert_eq!(year("1999-01"), Some(1999));
        assert_eq!(year("1999"), Some(1999));
        for bad in [
            "",
            "199",
            "0000",
            "1999-1-3",
            "1999/01/03",
            "99-01-03",
            "１９９９",
        ] {
            assert_eq!(year(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_are_strict() {
        assert_eq!(Aid::parse("1"), Some(Aid(1)));
        assert_eq!(Aid::parse("4294967295"), Some(Aid(u32::MAX)));
        for bad in ["", "0", "01", "4294967296", "+1", " 1", "1a", "12345678901"] {
            assert_eq!(Aid::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_gzip_bomb_stops_at_the_cap() {
        let limits = Limits {
            xml: XmlLimits {
                max_bytes: 1024,
                ..ANIME_LIMITS.xml
            },
            ..ANIME_LIMITS
        };
        let bomb = gzip(&vec![b'a'; 1 << 20]);
        assert!(bomb.len() < 4096);
        assert_eq!(inflate(&bomb, &limits), Err(Stop::Inflated));
        let small = gzip(b"<x/>");
        assert_eq!(inflate(&small, &limits).unwrap().as_ref(), b"<x/>");
        assert_eq!(inflate(&small[..small.len() - 4], &limits), Err(Stop::Gzip));
        assert_eq!(inflate(b"<x/>", &limits).unwrap().as_ref(), b"<x/>");
    }
}
