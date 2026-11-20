//! AniList GraphQL answers (ENR-anilist; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-15-ENR-anilist.md`): the media a fixed search
//! query found.
//!
//! Built like [`wikidata`](crate::wikidata) and
//! [`openlibrary`](crate::openlibrary): a pure function from bytes to
//! values, on the bounded [`json`](crate::json) reader, with no I/O and no
//! panics. Every cap is a named field of [`Limits`]. It reads only the
//! shape below, and anything else in a part it reads spoils that media,
//! counted in [`Skipped`]. [`Page::whole`] is the one rule a plugin
//! applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The shape, the answer to [`SEARCH_ANIME`]: `{"data": {"Page":
//! {"media": [{"id": 1, "format": "TV", "isAdult": false,
//! "countryOfOrigin": "JP", "seasonYear": 1998, "startDate": {"year":
//! 1998}, "title": {"romaji": text, "english": text, "native": text},
//! "synonyms": [text, …]}, …]}}}`. Every field of a media but `id` may be
//! `null` or left out. An error answer has an `errors` key at the root
//! ([`Page::error`]; its text is never read), and its `data` is usually
//! `null`.
//!
//! **What it keeps:** the media's id, format, adult flag, country of
//! origin, years and titles. Never a description, a staff or character
//! name, a cover or any other field: those are passed over unread, even
//! when the service sends them unasked.
//!
//! Keys are compared as written. In a part that's read, a key with an
//! escape spoils it, and so does a repeat of a key it reads. Repeats are
//! found with a bitmask over the known keys, never a list of every key
//! seen, so a body of a million unknown keys is read in linear time.

use crate::common::{Bad, prepare, title};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{R, Reader, TextError, Walk};

/// The one GraphQL document the plugin sends, fixed at compile time: a
/// page of anime whose title matches `$search`, never adult when
/// `$isAdult` is `false`, with the fields [`parse_search`] reads and no
/// others. Item text never goes in it; it travels only in the JSON
/// `variables` object.
pub const SEARCH_ANIME: &str = "query ($search: String, $perPage: Int, $isAdult: Boolean) { \
     Page(page: 1, perPage: $perPage) { \
     media(search: $search, type: ANIME, isAdult: $isAdult) { \
     id format isAdult countryOfOrigin seasonYear startDate { year } \
     title { romaji english native } synonyms } } }";

/// Caps on one parse. Why each value: the decisions file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array, read or passed over; over it the media is
    /// skipped.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the media is
    /// skipped.
    pub max_text: usize,
    /// Media, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Titles kept per media, its three own and its synonyms; the rest
    /// are passed over undecoded ([`Media::truncated`]).
    pub max_texts: usize,
}

/// The caps for a page of at most ten media with nine fields each: a few
/// KiB in practice.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    max_depth: 8,
    max_values: 20_000,
    max_array: 500,
    max_text: 4 * 1024,
    max_items: 10,
    max_texts: 64,
};

/// An AniList media id: a positive integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaId(pub u64);

impl MediaId {
    /// 1–20 ASCII digits with no leading zero that fit a `u64`. Nothing
    /// else: no sign, fraction, exponent or space.
    pub fn parse(s: &str) -> Option<MediaId> {
        if !digits(s) {
            return None;
        }
        s.parse().ok().map(MediaId)
    }
}

impl std::fmt::Display for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A media's `format`, AniList's `MediaFormat` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Tv,
    TvShort,
    Movie,
    Special,
    Ova,
    Ona,
    Music,
    Manga,
    Novel,
    OneShot,
    /// A value this parser doesn't know, which a later API may add. It
    /// never matches a kind.
    Other,
}

impl Format {
    fn parse(s: &str) -> Format {
        match s {
            "TV" => Format::Tv,
            "TV_SHORT" => Format::TvShort,
            "MOVIE" => Format::Movie,
            "SPECIAL" => Format::Special,
            "OVA" => Format::Ova,
            "ONA" => Format::Ona,
            "MUSIC" => Format::Music,
            "MANGA" => Format::Manga,
            "NOVEL" => Format::Novel,
            "ONE_SHOT" => Format::OneShot,
            _ => Format::Other,
        }
    }
}

/// One media. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub id: MediaId,
    /// `None` when `null` or left out.
    pub format: Option<Format>,
    /// `isAdult`; `None` when `null` or left out, which a plugin must
    /// treat as not known to be safe.
    pub adult: Option<bool>,
    /// `countryOfOrigin` when it's two upper-case ASCII letters.
    pub country: Option<String>,
    /// `seasonYear`, when it's 1–9999.
    pub season_year: Option<u16>,
    /// `startDate.year`, when it's 1–9999.
    pub start_year: Option<u16>,
    /// `title.romaji`, `title.english` and `title.native`, each cleaned
    /// as [`MAX_TITLE`] says.
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub native: Option<String>,
    /// Its `synonyms`, cleaned the same way, in order.
    pub synonyms: Vec<String>,
    /// Titles past [`Limits::max_texts`] were passed over.
    pub truncated: bool,
}

impl Media {
    /// Every title kept: romaji, English, native, then the synonyms.
    pub fn titles(&self) -> impl Iterator<Item = &String> {
        self.romaji
            .iter()
            .chain(&self.english)
            .chain(&self.native)
            .chain(&self.synonyms)
    }
}

/// Media skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `id`, or one that isn't a positive integer.
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
    /// The root has an `errors` key.
    pub error: bool,
    /// The `data.Page.media` array was read.
    pub seen: bool,
    /// A key on the way to `media` was written twice, with an escape, or
    /// held the wrong type.
    pub bad_root: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// The answer was read to its end, is the shape asked for, isn't an
    /// error and skipped nothing. Only such an answer may be used, as a
    /// match or as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none()
            && !self.trailing
            && !self.error
            && self.seen
            && !self.bad_root
            && self.skipped.total() == 0
    }
}

/// A search answer's media, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub media: Vec<Media>,
    pub page: Page,
}

/// Parses the answer to [`SEARCH_ANIME`].
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
    let mut media = Vec::new();
    let read_all = |p: &mut Parser<'_, '_>, media: &mut Vec<Media>| -> R<()> {
        p.within(&["data", "errors"], |p, k| match k {
            "data" => p.within(&["Page"], |p, _| {
                p.within(&["media"], |p, _| {
                    if p.j.peek() != Some(b'[') {
                        p.page.bad_root = true;
                        return p.j.skip().map(drop);
                    }
                    p.page.seen = true;
                    let mut over = false;
                    let max = p.l.max_array;
                    p.array(|p, n| {
                        if n >= max {
                            over = true;
                            return p.j.skip().map(drop);
                        }
                        media.extend(p.media()?);
                        Ok(())
                    })?;
                    if over {
                        p.page.stop.get_or_insert(Stop::Items);
                    }
                    Ok(())
                })
            }),
            _ => {
                p.page.error = true;
                p.j.skip().map(drop)
            }
        })?;
        let end = p.j.end();
        p.page.trailing = end.is_err();
        end
    };
    if let Err(stop) = read_all(&mut p, &mut media) {
        p.page.stop.get_or_insert(stop);
    }
    Search {
        media,
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

/// The media being read: its first fault.
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

/// The keys of a media the parser reads.
const MEDIA_KEYS: &[&str] = &[
    "id",
    "format",
    "isAdult",
    "countryOfOrigin",
    "seasonYear",
    "startDate",
    "title",
    "synonyms",
];

/// The keys of a media's `title`.
const TITLE_KEYS: &[&str] = &["romaji", "english", "native"];

impl<'a> Parser<'a, '_> {
    /// On the way to `media`: an object whose `known` keys are handed to
    /// `f`, once each; other keys are passed over. `null` is passed over
    /// (an error answer's `data`); any other type, or a repeated or
    /// escaped known key, is [`Page::bad_root`].
    fn within(&mut self, known: &[&str], mut f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<()> {
        match self.j.peek() {
            Some(b'{') => {}
            Some(b'n') => return self.j.skip().map(drop),
            _ => {
                self.page.bad_root = true;
                return self.j.skip().map(drop);
            }
        }
        let mut keys = Keys::default();
        self.object(|p, k| {
            if keys.repeat(k, known) {
                p.page.bad_root = true;
                return p.j.skip().map(drop);
            }
            if known.contains(&k) {
                f(p, k)
            } else {
                p.j.skip().map(drop)
            }
        })
    }

    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// `null`: passed over, true. Anything else: false, nothing read.
    fn null(&mut self) -> R<bool> {
        if self.j.peek() == Some(b'n') {
            self.j.skip()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// A string or `null`; anything else spoils `s` and is skipped.
    fn string(&mut self, s: &mut Spoil) -> R<Option<String>> {
        if self.null()? {
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

    /// A title: a string or `null`, cleaned; a title past
    /// [`Limits::max_texts`] is passed over undecoded and sets `truncated`.
    fn title(
        &mut self,
        s: &mut Spoil,
        texts: &mut usize,
        truncated: &mut bool,
    ) -> R<Option<String>> {
        if *texts >= self.l.max_texts && self.j.peek() == Some(b'"') {
            *truncated = true;
            self.skip_in(s)?;
            return Ok(None);
        }
        let t = self.string(s)?.as_deref().and_then(title);
        *texts += usize::from(t.is_some());
        Ok(t)
    }

    /// `true`, `false` or `null`; anything else spoils `s`.
    fn boolean(&mut self, s: &mut Spoil) -> R<Option<bool>> {
        let v = match self.j.peek() {
            Some(b't') => Some(true),
            Some(b'f') => Some(false),
            Some(b'n') => None,
            _ => {
                s.set(Bad::Malformed);
                return self.skip_in(s).map(|()| None);
            }
        };
        self.j.skip()?;
        Ok(v)
    }

    /// A number that's a year: `Some(year)` for an integer 1–9999, `None`
    /// for `null` or any other number, and anything else spoils `s`.
    fn year(&mut self, s: &mut Spoil) -> R<Option<u16>> {
        if self.null()? {
            return Ok(None);
        }
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let n = self.j.num()?;
        Ok(n.parse::<u16>().ok().filter(|y| (1..=9999).contains(y)))
    }

    /// An object that may be `null`, its keys read once each by `f`; any
    /// other type, or a repeated or escaped known key, spoils `s`.
    fn inner(
        &mut self,
        s: &mut Spoil,
        known: &[&str],
        mut f: impl FnMut(&mut Self, &mut Spoil, &'a str) -> R<()>,
    ) -> R<()> {
        if self.null()? {
            return Ok(());
        }
        if self.j.peek() != Some(b'{') {
            s.set(Bad::Malformed);
            return self.skip_in(s);
        }
        let mut keys = Keys::default();
        self.object(|p, k| {
            if keys.repeat(k, known) {
                s.set(Bad::Malformed);
            }
            if known.contains(&k) {
                f(p, s, k)
            } else {
                p.skip_in(s)
            }
        })
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

    /// One media.
    fn media(&mut self) -> R<Option<Media>> {
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
        let mut id = None;
        let (mut format, mut adult, mut country) = (None, None, None);
        let (mut season_year, mut start_year) = (None, None);
        let (mut romaji, mut english, mut native) = (None, None, None);
        let (mut synonyms, mut truncated, mut texts) = (Vec::new(), false, 0usize);
        self.object(|p, k| {
            if keys.repeat(k, MEDIA_KEYS) {
                s.set(Bad::Malformed);
            }
            match k {
                "id" => {
                    if matches!(p.j.peek(), Some(b'-' | b'0'..=b'9')) {
                        id = MediaId::parse(p.j.num()?);
                    } else {
                        p.skip_in(&mut s)?;
                    }
                }
                "format" => format = p.string(&mut s)?.as_deref().map(Format::parse),
                "isAdult" => adult = p.boolean(&mut s)?,
                "countryOfOrigin" => {
                    country = p
                        .string(&mut s)?
                        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()));
                }
                "seasonYear" => season_year = p.year(&mut s)?,
                "startDate" => p.inner(&mut s, &["year"], |p, s, _| {
                    start_year = p.year(s)?;
                    Ok(())
                })?,
                "title" => p.inner(&mut s, TITLE_KEYS, |p, s, k| {
                    let t = p.title(s, &mut texts, &mut truncated)?;
                    match k {
                        "romaji" => romaji = t,
                        "english" => english = t,
                        _ => native = t,
                    }
                    Ok(())
                })?,
                "synonyms" => {
                    if p.null()? {
                        return Ok(());
                    }
                    if p.j.peek() != Some(b'[') {
                        s.set(Bad::Malformed);
                        return p.skip_in(&mut s);
                    }
                    let max = p.l.max_array;
                    p.array(|p, n| {
                        if n >= max {
                            s.set(Bad::OverCap);
                            return p.j.skip().map(drop);
                        }
                        if p.j.peek() != Some(b'"') {
                            s.set(Bad::Malformed);
                            return p.skip_in(&mut s);
                        }
                        synonyms.extend(p.title(&mut s, &mut texts, &mut truncated)?);
                        Ok(())
                    })?;
                }
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        if !self.tally(&s) {
            return Ok(None);
        }
        let Some(id) = id else {
            self.page.skipped.bad_id = self.page.skipped.bad_id.saturating_add(1);
            return Ok(None);
        };
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(Media {
            id,
            format,
            adult,
            country,
            season_year,
            start_year,
            romaji,
            english,
            native,
            synonyms,
            truncated,
        }))
    }
}

/// 1–20 ASCII digits, no leading zero.
fn digits(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_ids_are_strict() {
        assert_eq!(MediaId::parse("1"), Some(MediaId(1)));
        assert_eq!(
            MediaId::parse(&u64::MAX.to_string()),
            Some(MediaId(u64::MAX))
        );
        for bad in [
            "",
            "0",
            "01",
            "-1",
            "+1",
            "1.0",
            "1e3",
            " 1",
            "18446744073709551616",
        ] {
            assert_eq!(MediaId::parse(bad), None, "{bad}");
        }
        assert_eq!(MediaId(42).to_string(), "42");
    }

    #[test]
    fn the_query_is_one_line_with_no_variable_but_the_three() {
        assert!(!SEARCH_ANIME.contains('\n'));
        let vars: Vec<&str> = SEARCH_ANIME
            .split(|c: char| !(c.is_alphanumeric() || c == '$'))
            .filter(|w| w.starts_with('$'))
            .collect();
        assert_eq!(
            vars,
            [
                "$search", "$perPage", "$isAdult", "$perPage", "$search", "$isAdult"
            ]
        );
        assert!(SEARCH_ANIME.contains("type: ANIME"));
    }
}
