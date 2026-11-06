//! Claims as stored (V14, architecture §18.5): what a source asserted
//! about a key, kept in `unverified_items.claims` and `torrent_claims.data`.
//!
//! The model mirrors `lodestar_sources::Claims` and `Provenance` (that
//! crate depends on this one, so it can't be shared), with the same
//! limits. SRC-2 maps one onto the other. The adult-enrichment section
//! (ENR-0, [`AdultEnrichment`]) is shared instead: `lodestar_sources`
//! re-exports it, so there's one definition of what it can hold.
//!
//! **The encoding is an on-disk format and an untrusted-input parser.** A
//! store file can be corrupt or crafted, so [`decode`] trusts nothing:
//!
//! ```text
//! blob   = version:u8 (1, 2 or 3) · codec:u8 (0 raw, 1 zstd) · body
//! plain  = present:u8 · confidence:u8
//!          [name:str] [size:uvar] [description:str] [publisher:str]
//!          [external_ref:str] [published_at:svar]
//!          files:uvar  (path:str0 · has_size:u8 · [size:uvar])*
//!          trackers:uvar (str)*
//!          ids:uvar (scheme:str · value:str)*
//!          [adult] [details]
//! adult  = kind:u8 (0 none, 1 scene, 2 movie, 3 VR) · flags:u8
//!          scene_id:str [title:str] [studio:str] [released:date]
//!          performers:uvar (name:str)*
//! date   = year:u16 big-endian (1–9999) · month:u8 (0 none, 1–12)
//!          · day:u8 (0 none, else a day of that month; only with a month)
//! details = flags:u8 [source_category:str] [posted_at:svar (0–MAX_POSTED_AT)]
//!          [canonical_title:str] [year:u16 big-endian (1–9999)]
//!          [match_confidence:u8 (0–100)]
//!          newsgroups:uvar (str)*
//!          alt_titles:uvar (language:str · title:str)*
//!          genres:uvar (str)*
//! str    = len:uvar (≥ 1) · UTF-8 bytes;  str0 allows len 0 (paths)
//! uvar   = LEB128, at most 10 bytes, minimal (no trailing zero groups)
//! svar   = uvar of the zigzag value
//! ```
//!
//! - `present` has one bit per optional field, in the order above, then
//!   bit 6 for the adult section and bit 7 for the details section. Every
//!   bit is spoken for: a new top-level field takes a new layout.
//! - Claims with an adult section hold no description, publisher or
//!   external ids (`present` bits 2 and 3 clear, `ids` 0): those fields
//!   could otherwise carry a scene description (plan Q3) or a performer id
//!   (Q4) beside the section.
//! - The adult section's `flags` has bit 0 for the title, 1 the studio, 2
//!   the release date and 3 `performers_truncated`; bits 4–7 are zero.
//! - **Version 2 is version 1 plus the adult section, and only that.** A
//!   version 1 blob has bit 6 clear, a version 2 blob has it set, and
//!   [`encode`] writes version 1 whenever there's no section, so a value
//!   has one blob version and claims without enrichment keep the bytes
//!   (and hashes) they had before ENR-0. The plain layer is the same for
//!   both: bit 6 says whether the section follows.
//! - **Version 3 is version 1 plus the details section ([`Details`],
//!   ENR-9), and only that.** It's written exactly when there's a details
//!   section, so claims without one keep their version 1 bytes. A row holds
//!   at most one section: claims with an adult section have no details
//!   section. The section's `flags` has bit 0 for the category, 1 the
//!   posting time, 2 the canonical title, 3 the year and 4 the match
//!   confidence; bits 5–7 are zero. A section claiming nothing is refused.
//! - **Beside a details section there's no description or publisher**:
//!   a plugin's overview could name the cast, and a usenet poster is often
//!   an email address. The row says what the work is, never who made or
//!   posted it.
//! - **Beside a details section, `ids` hold only enrichment ids**: the
//!   schemes in [`ENRICHMENT_SCHEMES`], each value in its one spelling
//!   ([`id_ok`]), no duplicates. IDs stay in `ids`; the section has none.
//! - Details entries are in stored form: language tags lower case
//!   ([`language_ok`]), newsgroups as RFC 5536 names ([`newsgroup_ok`]),
//!   genres normalised ([`genre_ok`]), no duplicate entry in any list.
//!   The section counts toward [`MAX_TOTAL`] at [`Details::charged_len`]:
//!   its text plus a fixed charge for itself and each entry, so the
//!   in-memory size `unverified::MAX_ROW` bounds still holds.
//! - Every length and count is checked against its limit before anything
//!   is allocated, the text total (the adult section's included) against
//!   [`MAX_TOTAL`], the body against
//!   [`MAX_PLAIN`] (zstd is asked for at most that much), and nothing may
//!   follow the last field.
//! - The plain layer is canonical: a body [`decode_plain`] accepts
//!   re-encodes to the same bytes. The fuzz target checks that.
//! - A new layout takes a new version byte; [`decode`] refuses versions it
//!   doesn't know rather than guessing.
//! - **The adult section holds no personal data beyond names** (owner
//!   §2.37, plan Q4 and Q5): performers are bare names on the item, with
//!   no id that could link one across items, and there's no field for a
//!   birth date, a description or anything else about a person. The type
//!   has nowhere to put one.
//! - **The details section describes a work, never a person**: no cast,
//!   crew, author, uploader or poster address, and the id rules refuse
//!   person ids (IMDb `nm`, Open Library authors, Discogs artists).

use std::fmt;

/// Bytes in a claimed name.
pub const MAX_NAME: usize = 1024;
/// Claimed files per key.
pub const MAX_FILES: usize = 10_000;
/// Bytes in one claimed file path.
pub const MAX_PATH: usize = 4096;
/// Claimed trackers per key.
pub const MAX_TRACKERS: usize = 64;
/// Bytes in one tracker URL.
pub const MAX_TRACKER: usize = 2048;
/// External IDs per key.
pub const MAX_IDS: usize = 32;
/// Bytes in an ID's scheme or value.
pub const MAX_ID_PART: usize = 128;
/// Bytes in a claimed description.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Bytes in a publisher identity.
pub const MAX_PUBLISHER: usize = 256;
/// Bytes in a provenance external reference.
pub const MAX_EXTERNAL_REF: usize = 2048;
/// Bytes in an adult-enrichment scene id: the service's own, opaque. They
/// are UUIDs (36 bytes) or integers; ENR-2b keeps up to 64.
pub const MAX_SCENE_ID: usize = 64;
/// Bytes in an adult-enrichment scene title: the parsers' `MAX_TITLE`.
pub const MAX_SCENE_TITLE: usize = 1024;
/// Bytes in an adult-enrichment studio name: the parsers' `MAX_NAME`.
pub const MAX_STUDIO: usize = 256;
/// Performer names in one adult-enrichment section: the parsers' default
/// `max_performers`. A caller with more keeps the first and sets
/// [`AdultEnrichment::performers_truncated`].
pub const MAX_PERFORMERS: usize = 64;
/// Bytes in one performer name: the parsers' `MAX_NAME`.
pub const MAX_PERFORMER: usize = 256;

// `unverified::MAX_ROW` bounds a row's `decoded_size` with the ids at
// their count limit and no performers. A row with an adult section holds
// no ids (`Claims::check`), so its performer list must take no more room
// than the ids it can't have: 64 × 24 ≤ 32 × 48 today.
const _: () = assert!(
    MAX_PERFORMERS * std::mem::size_of::<String>() <= MAX_IDS * std::mem::size_of::<ExternalId>()
);
/// Bytes of claim text in all (provenance aside), as `lodestar_sources`
/// clamps it. The adult section's text counts too (at most ~17 KiB).
pub const MAX_TOTAL: usize = 1024 * 1024;
/// The largest plain body: the text, the external reference, and the
/// framing: at most 13 bytes a file (a 2-byte path length, the size flag
/// and a 10-byte size) and under 3 KiB for the rest, either section's
/// framing included (the adult section's ~140 bytes, the details
/// section's ~150).
pub const MAX_PLAIN: usize = MAX_TOTAL + MAX_EXTERNAL_REF + MAX_FILES * 13 + 4096;
/// The largest stored blob: a body is stored raw when zstd doesn't make it
/// smaller. The schema's `CHECK` holds the same number.
pub const MAX_STORED: usize = MAX_PLAIN + 2;

/// The layout without an adult section, and the only one before ENR-0.
pub const VERSION: u8 = 1;
/// Version 1 plus the adult section.
pub const VERSION_ADULT: u8 = 2;
/// Version 1 plus the details section (ENR-9).
pub const VERSION_DETAILS: u8 = 3;
const RAW: u8 = 0;
const ZSTD: u8 = 1;

const NAME: u8 = 1 << 0;
const SIZE: u8 = 1 << 1;
const DESCRIPTION: u8 = 1 << 2;
const PUBLISHER: u8 = 1 << 3;
const EXTERNAL_REF: u8 = 1 << 4;
const PUBLISHED_AT: u8 = 1 << 5;
const ADULT: u8 = 1 << 6;
const DETAILS: u8 = 1 << 7;

const CATEGORY: u8 = 1 << 0;
const POSTED_AT: u8 = 1 << 1;
const CANONICAL: u8 = 1 << 2;
const YEAR: u8 = 1 << 3;
const MATCH_CONFIDENCE: u8 = 1 << 4;
const DETAILS_KNOWN: u8 = (1 << 5) - 1;

const TITLE: u8 = 1 << 0;
const STUDIO: u8 = 1 << 1;
const RELEASED: u8 = 1 << 2;
const TRUNCATED: u8 = 1 << 3;
const ADULT_KNOWN: u8 = (1 << 4) - 1;

/// One claimed file. The size is what the source says, not what's true.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimedFile {
    /// May be empty: the filter pipeline judges it (SRC-0).
    pub path: String,
    pub size: Option<u64>,
}

/// An external ID such as `imdb:tt0111161`. The scheme is lower case.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalId {
    pub scheme: String,
    pub value: String,
}

/// The path's length only, as for [`Claims`].
impl fmt::Debug for ClaimedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimedFile")
            .field("path_len", &self.path.len())
            .field("size", &self.size)
            .finish()
    }
}

/// The scheme, but the value's length only.
impl fmt::Debug for ExternalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalId")
            .field("scheme_len", &self.scheme.len())
            .field("value_len", &self.value.len())
            .finish()
    }
}

/// How much the source's word is worth, as its plugin judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Confidence {
    #[default]
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Where the claims came from, beyond the source id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Provenance {
    /// A feed item's GUID or page URL.
    pub external_ref: Option<String>,
    /// When the source says it was published, Unix seconds.
    pub published_at: Option<i64>,
    pub confidence: Confidence,
}

/// What an adult-enrichment service (ThePornDB, StashDB; architecture
/// §20.9) says an item is. The discriminants are the stored `kind` byte
/// (0 is no kind), so they are part of the format: never renumber them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AdultKind {
    Scene = 1,
    Movie = 2,
    Vr = 3,
}

/// A release date as a service states it: a year, a month, or a day.
/// Only real dates can be built, so a held one is always valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseDate {
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
}

impl ReleaseDate {
    /// A date, if it's a real one: year 1–9999, month 1–12, a day of
    /// that month, and a day only with a month.
    pub fn new(year: u16, month: Option<u8>, day: Option<u8>) -> Option<Self> {
        let ok = (1..=9999).contains(&year)
            && match (month, day) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(m), d) => {
                    (1..=12).contains(&m)
                        && d.is_none_or(|d| (1..=days_in_month(year, m)).contains(&d))
                }
            };
        ok.then_some(ReleaseDate { year, month, day })
    }

    /// Reads `YYYY`, `YYYY-MM` or `YYYY-MM-DD` exactly, digits only.
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        let num = |r: std::ops::Range<usize>| -> Option<u16> {
            let d = b.get(r)?;
            d.iter()
                .all(u8::is_ascii_digit)
                .then(|| d.iter().fold(0u16, |n, &c| n * 10 + u16::from(c - b'0')))
        };
        let year = num(0..4)?;
        match b.len() {
            4 => Self::new(year, None, None),
            7 if b[4] == b'-' => Self::new(year, Some(num(5..7)? as u8), None),
            10 if b[4] == b'-' && b[7] == b'-' => {
                Self::new(year, Some(num(5..7)? as u8), Some(num(8..10)? as u8))
            }
            _ => None,
        }
    }

    pub fn year(&self) -> u16 {
        self.year
    }

    pub fn month(&self) -> Option<u8> {
        self.month
    }

    pub fn day(&self) -> Option<u8> {
        self.day
    }
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, as [`ReleaseDate::parse`] reads.
impl fmt::Display for ReleaseDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.year)?;
        if let Some(m) = self.month {
            write!(f, "-{m:02}")?;
        }
        if let Some(d) = self.day {
            write!(f, "-{d:02}")?;
        }
        Ok(())
    }
}

fn days_in_month(y: u16, m: u8) -> u8 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// What an adult-enrichment service matched an item to (ENR-0;
/// architecture §20.9). Every field is the service's claim.
///
/// **Names only, on this item only** (owner §2.37, plan Q4 and Q5): a
/// performer is a bare name, with no id that could link one person across
/// items, and there's no field for a birth date, a description or any
/// other personal detail. Keep it that way: a new field here needs the
/// owner.
#[derive(Clone, PartialEq, Eq)]
pub struct AdultEnrichment {
    /// The service's id for the scene or movie: 1–[`MAX_SCENE_ID`] ASCII
    /// letters, digits and `-` (UUIDs or integers). Opaque, but narrow
    /// enough that a link built from it (ENR-7) can't leave the scene's
    /// page: no `/`, `?`, `#`, `%` or `.`.
    pub scene_id: String,
    pub kind: Option<AdultKind>,
    pub title: Option<String>,
    /// The studio's name; no studio id is kept.
    pub studio: Option<String>,
    pub released: Option<ReleaseDate>,
    /// Performer names, in the service's order. Duplicates are allowed:
    /// two people can share a name.
    pub performers: Vec<String>,
    /// The service listed more performers than are here: over
    /// [`MAX_PERFORMERS`], or entries the parser couldn't use.
    pub performers_truncated: bool,
}

/// Lengths only: the section names people.
impl fmt::Debug for AdultEnrichment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdultEnrichment")
            .field("scene_id_len", &self.scene_id.len())
            .field("kind", &self.kind)
            .field("title_len", &self.title.as_ref().map(String::len))
            .field("studio_len", &self.studio.as_ref().map(String::len))
            .field("released", &self.released.is_some())
            .field("performers", &self.performers.len())
            .field("performers_truncated", &self.performers_truncated)
            .finish()
    }
}

impl AdultEnrichment {
    /// A section for `scene_id` with nothing else claimed.
    pub fn new(scene_id: impl Into<String>) -> Self {
        AdultEnrichment {
            scene_id: scene_id.into(),
            kind: None,
            title: None,
            studio: None,
            released: None,
            performers: Vec::new(),
            performers_truncated: false,
        }
    }

    /// Bytes of text held; part of [`Claims::text_len`].
    pub fn text_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        self.scene_id.len()
            + len(&self.title)
            + len(&self.studio)
            + self.performers.iter().map(String::len).sum::<usize>()
    }

    /// Whether the section is within every limit: a valid scene id, no
    /// empty strings, nothing over its cap.
    pub fn check(&self) -> Result<(), ClaimsError> {
        if !scene_id_ok(&self.scene_id) {
            return Err(ClaimsError::Limit("scene id"));
        }
        let opt = |s: &Option<String>, max, what| match s {
            Some(v) if v.is_empty() || v.len() > max => Err(ClaimsError::Limit(what)),
            _ => Ok(()),
        };
        opt(&self.title, MAX_SCENE_TITLE, "scene title")?;
        opt(&self.studio, MAX_STUDIO, "studio")?;
        if self.performers.len() > MAX_PERFORMERS
            || self
                .performers
                .iter()
                .any(|p| p.is_empty() || p.len() > MAX_PERFORMER)
        {
            return Err(ClaimsError::Limit("performers"));
        }
        Ok(())
    }
}

/// 1–[`MAX_SCENE_ID`] ASCII letters, digits and `-`.
pub fn scene_id_ok(s: &str) -> bool {
    (1..=MAX_SCENE_ID).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Bytes in a source's own category string (`TV > HD`, `5040`).
pub const MAX_CATEGORY: usize = 256;
/// Newsgroups one usenet item names.
pub const MAX_NEWSGROUPS: usize = 8;
/// Bytes in one newsgroup name.
pub const MAX_NEWSGROUP: usize = 128;
/// Alternative titles in one details section. ENR-11 indexes at most 8.
pub const MAX_ALT_TITLES: usize = 32;
/// Bytes in one alternative title.
pub const MAX_ALT_TITLE: usize = 512;
/// Bytes in a BCP 47 language tag. RFC 5646 §4.4.1 sizes buffers at 35
/// for every tag short of extensions and private use.
pub const MAX_LANGUAGE: usize = 35;
/// Bytes in a canonical title.
pub const MAX_CANONICAL_TITLE: usize = MAX_NAME;
/// Genres in one details section.
pub const MAX_GENRES: usize = 16;
/// Bytes in one genre.
pub const MAX_GENRE: usize = 64;
/// The latest posting time, in Unix seconds: the last second of year 9999
/// UTC. Posting times run from 0 (1970) to this.
pub const MAX_POSTED_AT: i64 = 253_402_300_799;
/// The highest match confidence: a percentage.
pub const MAX_MATCH_CONFIDENCE: u8 = 100;

/// What the details section counts toward [`MAX_TOTAL`] beyond its text:
/// the section itself, and each list entry. Fixed numbers rather than
/// `size_of`, so a blob one build accepts every build accepts; the asserts
/// below keep them at least the real sizes, so `unverified::MAX_ROW`
/// still bounds [`Claims::decoded_size`].
const DETAILS_COST: usize = 256;
const ALT_TITLE_COST: usize = 48;
const STRING_COST: usize = 24;
const _: () = assert!(std::mem::size_of::<Details>() <= DETAILS_COST);
const _: () = assert!(std::mem::size_of::<AltTitle>() <= ALT_TITLE_COST);
const _: () = assert!(std::mem::size_of::<String>() <= STRING_COST);

/// The external-ID schemes claims with a details section may carry, the
/// ones enrichment writes (plan §4.3 ENR-9). Each value is checked by
/// [`id_ok`]. None of them names a person: IMDb `nm` ids, Open Library
/// authors and Discogs artists are refused.
pub const ENRICHMENT_SCHEMES: [&str; 11] = [
    "imdb", "tmdb", "tvdb", "anidb", "anilist", "mbid", "olid", "igdb", "discogs", "steam",
    "wikidata",
];

/// Whether `value` is a well-formed id in `scheme`, one of
/// [`ENRICHMENT_SCHEMES`]:
///
/// - `imdb`: a title, `tt` and 7 digits (zero-padded), or 8–10 with no
///   leading zero;
/// - `tmdb`, `tvdb`, `anidb`, `anilist`, `igdb`, `steam`: 1–20 digits,
///   no leading zero;
/// - `mbid`: a lower-case UUID, `8-4-4-4-12` hex;
/// - `olid`: `OL`, digits, then `M` (an edition) or `W` (a work);
/// - `discogs`: `r` (a release) or `m` (a master) and digits;
/// - `wikidata`: an item, `Q` and digits.
///
/// Every form is ASCII, and digits never start with a zero unless the
/// scheme pads (IMDb does), so each id has one spelling.
pub fn id_ok(scheme: &str, value: &str) -> bool {
    fn num(s: &str) -> bool {
        (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
    }
    match scheme {
        // Seven digits padded with zeros, or more with no padding: the
        // form IMDb itself prints.
        "imdb" => value.strip_prefix("tt").is_some_and(|d| {
            d.bytes().all(|b| b.is_ascii_digit())
                && (d.len() == 7 || ((8..=10).contains(&d.len()) && !d.starts_with('0')))
        }),
        "tmdb" | "tvdb" | "anidb" | "anilist" | "igdb" | "steam" => num(value),
        "mbid" => {
            let b = value.as_bytes();
            b.len() == 36
                && b.iter().enumerate().all(|(i, &c)| match i {
                    8 | 13 | 18 | 23 => c == b'-',
                    _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
                })
        }
        "olid" => value
            .strip_prefix("OL")
            .and_then(|r| r.strip_suffix('M').or_else(|| r.strip_suffix('W')))
            .is_some_and(num),
        "discogs" => value
            .strip_prefix('r')
            .or_else(|| value.strip_prefix('m'))
            .is_some_and(num),
        "wikidata" => value.strip_prefix('Q').is_some_and(num),
        _ => false,
    }
}

/// Whether `s` is a well-formed BCP 47 tag as stored: 1–[`MAX_LANGUAGE`]
/// bytes of `-`-separated subtags of 1–8 lower-case ASCII letters and
/// digits, the first 2–8 letters (`en`, `zh-hant-tw`, `und` for not
/// known). It checks the shape, not the registry, so a tag a newer
/// registry adds still fits. Lower case is the stored form: tags compare
/// without case (RFC 5646 §2.1.1), so one spelling each keeps the body
/// canonical.
pub fn language_ok(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_LANGUAGE {
        return false;
    }
    s.split('-').enumerate().all(|(i, t)| {
        (1..=8).contains(&t.len())
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && (i > 0 || (t.len() >= 2 && t.bytes().all(|b| b.is_ascii_lowercase())))
    })
}

/// Whether `s` is a newsgroup name: 1–[`MAX_NEWSGROUP`] bytes of
/// `.`-separated components, each ASCII letters, digits, `+`, `-` or `_`
/// (RFC 5536 §3.1.4). Names are kept as the source spells them, never
/// cut or re-cased, since either could name another group.
pub fn newsgroup_ok(s: &str) -> bool {
    (1..=MAX_NEWSGROUP).contains(&s.len())
        && s.split('.').all(|c| {
            !c.is_empty()
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_'))
        })
}

/// Whether `s` is a genre in its normalised form: 1–[`MAX_GENRE`] bytes,
/// no ASCII upper case, no control characters (`Cc`, a fixed set), no
/// ASCII whitespace but the space, and words separated by one space with
/// none at either end. `lodestar_sources` normalises further, with full
/// Unicode lower-casing and whitespace folding; the check uses no Unicode
/// property that can change, so a Unicode update can never turn a stored
/// genre invalid.
pub fn genre_ok(s: &str) -> bool {
    (1..=MAX_GENRE).contains(&s.len())
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && !s.contains("  ")
        && s.chars().all(|c| {
            !c.is_ascii_uppercase() && !c.is_control() && (c == ' ' || !c.is_ascii_whitespace())
        })
}

/// One alternative title with its language.
#[derive(Clone, PartialEq, Eq)]
pub struct AltTitle {
    /// A BCP 47 tag in stored form ([`language_ok`]); `und` when the
    /// source doesn't say.
    pub language: String,
    /// 1–[`MAX_ALT_TITLE`] bytes.
    pub title: String,
}

/// The language, but the title's length only.
impl fmt::Debug for AltTitle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AltTitle")
            .field("language", &self.language)
            .field("title_len", &self.title.len())
            .finish()
    }
}

/// The version 3 section (ENR-9, plan §4.3): what an enrichment plugin
/// resolved an item to, and what a feed says beyond a name and files (a
/// usenet item's groups, an IRC announce's category). Every field is the
/// source's claim. External IDs stay in [`Claims::ids`], restricted to
/// [`ENRICHMENT_SCHEMES`] when this section is present, and the row then
/// has no description or publisher.
///
/// **It describes a work, never a person** (owner §2.37, plan §4.3): there
/// is no field for cast, crew, an author, an uploader or a poster's
/// address. A new field here is a new layout and needs the owner if it
/// could hold a personal detail.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Details {
    /// The source's own category string, as the feed states it
    /// (1–[`MAX_CATEGORY`] bytes).
    pub source_category: Option<String>,
    /// When a usenet item says it was posted, Unix seconds,
    /// 0–[`MAX_POSTED_AT`].
    pub posted_at: Option<i64>,
    /// The groups a usenet item was posted to, in the source's order, no
    /// duplicates ([`newsgroup_ok`]).
    pub newsgroups: Vec<String>,
    /// Other titles the work is known by, no duplicate pairs.
    pub alt_titles: Vec<AltTitle>,
    /// The work's own title as the plugin's database holds it.
    pub canonical_title: Option<String>,
    /// Normalised genres ([`genre_ok`]), in the source's order, no
    /// duplicates.
    pub genres: Vec<String>,
    /// The work's year, 1–9999.
    pub year: Option<u16>,
    /// How sure the plugin is that the match is right, 0–100 percent.
    /// The row's [`Provenance::confidence`] is how far the source is
    /// trusted at all; this is how well this one match fits.
    pub match_confidence: Option<u8>,
}

/// Counts and the year only: titles and groups can name what a rule
/// holds.
impl fmt::Debug for Details {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Details")
            .field("source_category", &self.source_category.is_some())
            .field("posted_at", &self.posted_at.is_some())
            .field("newsgroups", &self.newsgroups.len())
            .field("alt_titles", &self.alt_titles)
            .field("canonical_title", &self.canonical_title.is_some())
            .field("genres", &self.genres.len())
            .field("year", &self.year)
            .field("match_confidence", &self.match_confidence)
            .finish()
    }
}

impl Details {
    /// True when the section claims nothing; such a section isn't stored.
    pub fn is_empty(&self) -> bool {
        self.source_category.is_none()
            && self.posted_at.is_none()
            && self.newsgroups.is_empty()
            && self.alt_titles.is_empty()
            && self.canonical_title.is_none()
            && self.genres.is_empty()
            && self.year.is_none()
            && self.match_confidence.is_none()
    }

    /// Bytes of text held.
    pub fn text_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        len(&self.source_category)
            + len(&self.canonical_title)
            + self.newsgroups.iter().map(String::len).sum::<usize>()
            + self
                .alt_titles
                .iter()
                .map(|a| a.language.len() + a.title.len())
                .sum::<usize>()
            + self.genres.iter().map(String::len).sum::<usize>()
    }

    /// What the section counts toward [`MAX_TOTAL`]: its text plus a fixed
    /// charge for the section and each entry, so a row's
    /// [`Claims::decoded_size`] stays within `unverified::MAX_ROW`.
    pub fn charged_len(&self) -> usize {
        DETAILS_COST
            + self.text_len()
            + self.alt_titles.len() * ALT_TITLE_COST
            + (self.newsgroups.len() + self.genres.len()) * STRING_COST
    }

    /// Whether the section is within every limit, in its stored form.
    pub fn check(&self) -> Result<(), ClaimsError> {
        if self.is_empty() {
            return Err(ClaimsError::Limit("empty details"));
        }
        let opt = |s: &Option<String>, max, what| match s {
            Some(v) if v.is_empty() || v.len() > max => Err(ClaimsError::Limit(what)),
            _ => Ok(()),
        };
        opt(&self.source_category, MAX_CATEGORY, "source category")?;
        opt(
            &self.canonical_title,
            MAX_CANONICAL_TITLE,
            "canonical title",
        )?;
        if self.newsgroups.len() > MAX_NEWSGROUPS
            || !self.newsgroups.iter().all(|g| newsgroup_ok(g))
            || has_duplicates(&self.newsgroups)
        {
            return Err(ClaimsError::Limit("newsgroups"));
        }
        if self.alt_titles.len() > MAX_ALT_TITLES
            || !self
                .alt_titles
                .iter()
                .all(|a| language_ok(&a.language) && (1..=MAX_ALT_TITLE).contains(&a.title.len()))
            || has_duplicates(&self.alt_titles)
        {
            return Err(ClaimsError::Limit("alternative titles"));
        }
        if self.genres.len() > MAX_GENRES
            || !self.genres.iter().all(|g| genre_ok(g))
            || has_duplicates(&self.genres)
        {
            return Err(ClaimsError::Limit("genres"));
        }
        if self
            .posted_at
            .is_some_and(|t| !(0..=MAX_POSTED_AT).contains(&t))
        {
            return Err(ClaimsError::Limit("posted time"));
        }
        if self.year.is_some_and(|y| !(1..=9999).contains(&y)) {
            return Err(ClaimsError::Limit("year"));
        }
        if self
            .match_confidence
            .is_some_and(|c| c > MAX_MATCH_CONFIDENCE)
        {
            return Err(ClaimsError::Limit("match confidence"));
        }
        Ok(())
    }
}

/// Whether `v` repeats an entry. The lists are at most 32 long.
fn has_duplicates<T: PartialEq>(v: &[T]) -> bool {
    v.iter().enumerate().any(|(i, a)| v[..i].contains(a))
}

/// Everything one source asserted about one key.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Claims {
    pub name: Option<String>,
    pub size: Option<u64>,
    pub files: Vec<ClaimedFile>,
    pub trackers: Vec<String>,
    pub ids: Vec<ExternalId>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub provenance: Provenance,
    /// An adult-enrichment match (ENR-0). Claims with one are stored as
    /// version 2.
    pub adult: Option<AdultEnrichment>,
    /// The details section (ENR-9). Claims with one are stored as version
    /// 3. Boxed: most claims have none, and they're held by the page.
    pub details: Option<Box<Details>>,
}

/// No text: claims can hold a name a rule holds, and that mustn't reach a
/// log line.
impl fmt::Debug for Claims {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claims")
            .field("name_len", &self.name.as_ref().map(String::len))
            .field("files", &self.files.len())
            .field("text_len", &self.text_len())
            .field("adult", &self.adult)
            .field("details", &self.details)
            .finish_non_exhaustive()
    }
}

impl Claims {
    /// Bytes the decoded claims take in memory, near enough: their text
    /// plus every entry's own struct. A page of claims is bounded by this,
    /// not by text alone: 10,000 empty file paths hold no text but still
    /// take ~400 KB as a `Vec<ClaimedFile>`.
    pub fn decoded_size(&self) -> usize {
        use std::mem::size_of;
        size_of::<Claims>()
            + self.text_len()
            + self.provenance.external_ref.as_ref().map_or(0, String::len)
            + self.files.len() * size_of::<ClaimedFile>()
            + self.trackers.len() * size_of::<String>()
            + self.ids.len() * size_of::<ExternalId>()
            + self
                .adult
                .as_ref()
                .map_or(0, |a| a.performers.len() * size_of::<String>())
    }

    /// Bytes of text held, the measure [`MAX_TOTAL`] bounds. The details
    /// section counts at [`Details::charged_len`], its text plus a fixed
    /// charge per entry.
    pub fn text_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        self.details.as_ref().map_or(0, |d| d.charged_len())
            + len(&self.name)
            + len(&self.description)
            + len(&self.publisher)
            + self.trackers.iter().map(String::len).sum::<usize>()
            + self
                .ids
                .iter()
                .map(|i| i.scheme.len() + i.value.len())
                .sum::<usize>()
            + self.files.iter().map(|f| f.path.len()).sum::<usize>()
            + self.adult.as_ref().map_or(0, AdultEnrichment::text_len)
    }

    /// No description, publisher or external ids beside an adult section.
    /// Enrichment results are their own rows under the enrichment
    /// source's id (plan §3), and these fields are where a description
    /// (plan Q3) or a performer id (Q4) could otherwise ride along.
    fn enrichment_only(&self) -> bool {
        self.description.is_none() && self.publisher.is_none() && self.ids.is_empty()
    }

    /// The blob version these claims encode as: [`VERSION_DETAILS`] with a
    /// details section, [`VERSION_ADULT`] with an adult section, else
    /// [`VERSION`]. [`Claims::check`] refuses both sections at once.
    pub fn version(&self) -> u8 {
        if self.details.is_some() {
            VERSION_DETAILS
        } else if self.adult.is_some() {
            VERSION_ADULT
        } else {
            VERSION
        }
    }

    /// Whether the claims are within every limit, in the shape
    /// `lodestar_sources::Claims::clamp` leaves them: no empty optional
    /// strings, trackers or ID parts, and lower-case ID schemes.
    pub fn check(&self) -> Result<(), ClaimsError> {
        let opt = |s: &Option<String>, max, what| match s {
            Some(v) if v.is_empty() || v.len() > max => Err(ClaimsError::Limit(what)),
            _ => Ok(()),
        };
        opt(&self.name, MAX_NAME, "name")?;
        opt(&self.description, MAX_DESCRIPTION, "description")?;
        opt(&self.publisher, MAX_PUBLISHER, "publisher")?;
        opt(
            &self.provenance.external_ref,
            MAX_EXTERNAL_REF,
            "external reference",
        )?;
        if self.files.len() > MAX_FILES || self.files.iter().any(|f| f.path.len() > MAX_PATH) {
            return Err(ClaimsError::Limit("files"));
        }
        if self.trackers.len() > MAX_TRACKERS
            || self
                .trackers
                .iter()
                .any(|t| t.is_empty() || t.len() > MAX_TRACKER)
        {
            return Err(ClaimsError::Limit("trackers"));
        }
        let part = |s: &str| !s.is_empty() && s.len() <= MAX_ID_PART;
        if self.ids.len() > MAX_IDS
            || self.ids.iter().any(|i| {
                !part(&i.scheme)
                    || !part(&i.value)
                    || i.scheme.bytes().any(|b| b.is_ascii_uppercase())
            })
        {
            return Err(ClaimsError::Limit("ids"));
        }
        if let Some(a) = &self.adult {
            a.check()?;
            if !self.enrichment_only() || self.details.is_some() {
                return Err(ClaimsError::Limit("enrichment claims"));
            }
        }
        if let Some(d) = &self.details {
            d.check()?;
            if self.description.is_some() || self.publisher.is_some() {
                return Err(ClaimsError::Limit("details claims"));
            }
            if !self.ids.iter().all(|i| id_ok(&i.scheme, &i.value)) || has_duplicates(&self.ids) {
                return Err(ClaimsError::Limit("enrichment ids"));
            }
        }
        if self.text_len() > MAX_TOTAL {
            return Err(ClaimsError::Limit("total"));
        }
        Ok(())
    }
}

/// Why claims couldn't be encoded or decoded. Carries no claim text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimsError {
    /// A version byte this build doesn't know.
    Version(u8),
    /// A codec byte this build doesn't know.
    Codec(u8),
    /// zstd refused the body, or it decompresses past [`MAX_PLAIN`].
    Compression,
    /// The blob or body ended inside a field.
    Truncated,
    /// Bytes after the last field.
    Trailing,
    /// A value the layout forbids (a non-minimal varint, unknown bits, a
    /// bad flag byte, invalid UTF-8).
    Malformed(&'static str),
    /// A field over its limit, or empty where it can't be.
    Limit(&'static str),
}

impl fmt::Display for ClaimsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimsError::Version(v) => write!(f, "unknown claims version {v}"),
            ClaimsError::Codec(c) => write!(f, "unknown claims codec {c}"),
            ClaimsError::Compression => f.write_str("claims don't decompress within bounds"),
            ClaimsError::Truncated => f.write_str("claims end inside a field"),
            ClaimsError::Trailing => f.write_str("bytes after the claims"),
            ClaimsError::Malformed(w) => write!(f, "malformed claims: {w}"),
            ClaimsError::Limit(w) => write!(f, "claims over a limit: {w}"),
        }
    }
}

impl std::error::Error for ClaimsError {}

/// The stored blob for `claims`: checked, laid out, and compressed at
/// zstd `level` when that makes it smaller.
pub fn encode(claims: &Claims, level: i32) -> Result<Vec<u8>, ClaimsError> {
    let plain = encode_plain(claims)?;
    let mut out = Vec::with_capacity(plain.len() + 2);
    out.push(claims.version());
    match zstd::bulk::compress(&plain, level) {
        Ok(z) if z.len() < plain.len() => {
            out.push(ZSTD);
            out.extend_from_slice(&z);
        }
        _ => {
            out.push(RAW);
            out.extend_from_slice(&plain);
        }
    }
    Ok(out)
}

/// Reads a stored blob back, version 1, 2 or 3. Bounded: it never holds
/// more than [`MAX_PLAIN`] bytes of body.
pub fn decode(blob: &[u8]) -> Result<Claims, ClaimsError> {
    let (&version, rest) = blob.split_first().ok_or(ClaimsError::Truncated)?;
    if !matches!(version, VERSION | VERSION_ADULT | VERSION_DETAILS) {
        return Err(ClaimsError::Version(version));
    }
    let (&codec, body) = rest.split_first().ok_or(ClaimsError::Truncated)?;
    let claims = decode_body(codec, body)?;
    // One version per value: a section is what versions 2 and 3 add.
    if claims.version() != version {
        return Err(ClaimsError::Malformed("section and version disagree"));
    }
    Ok(claims)
}

fn decode_body(codec: u8, body: &[u8]) -> Result<Claims, ClaimsError> {
    match codec {
        RAW => {
            if body.len() > MAX_PLAIN {
                return Err(ClaimsError::Limit("body"));
            }
            decode_plain(body)
        }
        ZSTD => {
            if body.len() > MAX_PLAIN {
                return Err(ClaimsError::Limit("body"));
            }
            // Reserve what the frame declares, never more than the cap: a
            // larger body fails the decompression instead.
            let declared = zstd::zstd_safe::get_frame_content_size(body)
                .map_err(|_| ClaimsError::Compression)?
                .map_or(MAX_PLAIN, |n| n.min(MAX_PLAIN as u64) as usize);
            let plain =
                zstd::bulk::decompress(body, declared).map_err(|_| ClaimsError::Compression)?;
            decode_plain(&plain)
        }
        c => Err(ClaimsError::Codec(c)),
    }
}

/// The plain body of `claims` (no version or codec byte). The version is
/// [`Claims::version`].
pub fn encode_plain(claims: &Claims) -> Result<Vec<u8>, ClaimsError> {
    claims.check()?;
    let c = claims;
    let p = &c.provenance;
    let mut present = 0;
    for (on, bit) in [
        (c.name.is_some(), NAME),
        (c.size.is_some(), SIZE),
        (c.description.is_some(), DESCRIPTION),
        (c.publisher.is_some(), PUBLISHER),
        (p.external_ref.is_some(), EXTERNAL_REF),
        (p.published_at.is_some(), PUBLISHED_AT),
        (c.adult.is_some(), ADULT),
        (c.details.is_some(), DETAILS),
    ] {
        if on {
            present |= bit;
        }
    }
    let mut out = Vec::with_capacity(64 + c.text_len());
    out.push(present);
    out.push(p.confidence as u8);
    let put_str = |out: &mut Vec<u8>, s: &str| {
        put_uvar(out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    };
    if let Some(s) = &c.name {
        put_str(&mut out, s);
    }
    if let Some(n) = c.size {
        put_uvar(&mut out, n);
    }
    if let Some(s) = &c.description {
        put_str(&mut out, s);
    }
    if let Some(s) = &c.publisher {
        put_str(&mut out, s);
    }
    if let Some(s) = &p.external_ref {
        put_str(&mut out, s);
    }
    if let Some(t) = p.published_at {
        put_uvar(&mut out, ((t << 1) ^ (t >> 63)) as u64);
    }
    put_uvar(&mut out, c.files.len() as u64);
    for f in &c.files {
        put_str(&mut out, &f.path);
        match f.size {
            Some(n) => {
                out.push(1);
                put_uvar(&mut out, n);
            }
            None => out.push(0),
        }
    }
    put_uvar(&mut out, c.trackers.len() as u64);
    for t in &c.trackers {
        put_str(&mut out, t);
    }
    put_uvar(&mut out, c.ids.len() as u64);
    for i in &c.ids {
        put_str(&mut out, &i.scheme);
        put_str(&mut out, &i.value);
    }
    if let Some(a) = &c.adult {
        out.push(a.kind.map_or(0, |k| k as u8));
        let mut flags = 0;
        for (on, bit) in [
            (a.title.is_some(), TITLE),
            (a.studio.is_some(), STUDIO),
            (a.released.is_some(), RELEASED),
            (a.performers_truncated, TRUNCATED),
        ] {
            if on {
                flags |= bit;
            }
        }
        out.push(flags);
        put_str(&mut out, &a.scene_id);
        if let Some(s) = &a.title {
            put_str(&mut out, s);
        }
        if let Some(s) = &a.studio {
            put_str(&mut out, s);
        }
        if let Some(d) = a.released {
            out.extend_from_slice(&d.year.to_be_bytes());
            out.push(d.month.unwrap_or(0));
            out.push(d.day.unwrap_or(0));
        }
        put_uvar(&mut out, a.performers.len() as u64);
        for p in &a.performers {
            put_str(&mut out, p);
        }
    }
    if let Some(d) = &c.details {
        let mut flags = 0;
        for (on, bit) in [
            (d.source_category.is_some(), CATEGORY),
            (d.posted_at.is_some(), POSTED_AT),
            (d.canonical_title.is_some(), CANONICAL),
            (d.year.is_some(), YEAR),
            (d.match_confidence.is_some(), MATCH_CONFIDENCE),
        ] {
            if on {
                flags |= bit;
            }
        }
        out.push(flags);
        if let Some(s) = &d.source_category {
            put_str(&mut out, s);
        }
        if let Some(t) = d.posted_at {
            put_uvar(&mut out, ((t << 1) ^ (t >> 63)) as u64);
        }
        if let Some(s) = &d.canonical_title {
            put_str(&mut out, s);
        }
        if let Some(y) = d.year {
            out.extend_from_slice(&y.to_be_bytes());
        }
        if let Some(m) = d.match_confidence {
            out.push(m);
        }
        put_uvar(&mut out, d.newsgroups.len() as u64);
        for g in &d.newsgroups {
            put_str(&mut out, g);
        }
        put_uvar(&mut out, d.alt_titles.len() as u64);
        for a in &d.alt_titles {
            put_str(&mut out, &a.language);
            put_str(&mut out, &a.title);
        }
        put_uvar(&mut out, d.genres.len() as u64);
        for g in &d.genres {
            put_str(&mut out, g);
        }
    }
    if out.len() > MAX_PLAIN {
        return Err(ClaimsError::Limit("body"));
    }
    Ok(out)
}

/// Parses a plain body, with or without the adult section. See the module
/// docs for the layout and checks.
pub fn decode_plain(body: &[u8]) -> Result<Claims, ClaimsError> {
    if body.len() > MAX_PLAIN {
        return Err(ClaimsError::Limit("body"));
    }
    let mut r = Reader {
        rest: body,
        text: 0,
    };
    // Every bit of `present` is a field now; a new one takes a new byte.
    let present = r.byte()?;
    let confidence = match r.byte()? {
        0 => Confidence::Low,
        1 => Confidence::Medium,
        2 => Confidence::High,
        _ => return Err(ClaimsError::Malformed("confidence")),
    };
    let has = |bit| present & bit != 0;
    let name = has(NAME).then(|| r.text(1, MAX_NAME, "name")).transpose()?;
    let size = has(SIZE).then(|| r.uvar()).transpose()?;
    let description = has(DESCRIPTION)
        .then(|| r.text(1, MAX_DESCRIPTION, "description"))
        .transpose()?;
    let publisher = has(PUBLISHER)
        .then(|| r.text(1, MAX_PUBLISHER, "publisher"))
        .transpose()?;
    // Provenance isn't text the total counts (as in `lodestar_sources`).
    let before = r.text;
    let external_ref = has(EXTERNAL_REF)
        .then(|| r.text(1, MAX_EXTERNAL_REF, "external reference"))
        .transpose()?;
    r.text = before;
    let published_at = has(PUBLISHED_AT)
        .then(|| r.uvar().map(|z| ((z >> 1) as i64) ^ -((z & 1) as i64)))
        .transpose()?;

    // Each file takes at least two bytes (an empty path and its flag), so
    // a count past what's left fails before anything is reserved.
    let n = r.count(MAX_FILES, 2, "files")?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        let path = r.text(0, MAX_PATH, "file path")?;
        let size = match r.byte()? {
            0 => None,
            1 => Some(r.uvar()?),
            _ => return Err(ClaimsError::Malformed("file size flag")),
        };
        files.push(ClaimedFile { path, size });
    }
    let n = r.count(MAX_TRACKERS, 2, "trackers")?;
    let mut trackers = Vec::with_capacity(n);
    for _ in 0..n {
        trackers.push(r.text(1, MAX_TRACKER, "tracker")?);
    }
    let n = r.count(MAX_IDS, 4, "ids")?;
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let scheme = r.text(1, MAX_ID_PART, "id scheme")?;
        if scheme.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(ClaimsError::Malformed("id scheme case"));
        }
        let value = r.text(1, MAX_ID_PART, "id value")?;
        ids.push(ExternalId { scheme, value });
    }
    let adult = has(ADULT).then(|| r.adult()).transpose()?;
    if adult.is_some()
        && (description.is_some() || publisher.is_some() || !ids.is_empty() || has(DETAILS))
    {
        return Err(ClaimsError::Malformed("enrichment claims"));
    }
    if has(DETAILS) && (description.is_some() || publisher.is_some()) {
        return Err(ClaimsError::Malformed("details claims"));
    }
    let details = has(DETAILS).then(|| r.details()).transpose()?;
    if details.is_some()
        && (!ids.iter().all(|i| id_ok(&i.scheme, &i.value)) || has_duplicates(&ids))
    {
        return Err(ClaimsError::Malformed("enrichment ids"));
    }
    if !r.rest.is_empty() {
        return Err(ClaimsError::Trailing);
    }
    Ok(Claims {
        name,
        size,
        files,
        trackers,
        ids,
        description,
        publisher,
        provenance: Provenance {
            external_ref,
            published_at,
            confidence,
        },
        adult,
        details: details.map(Box::new),
    })
}

fn put_uvar(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    rest: &'a [u8],
    /// Text bytes read so far, against [`MAX_TOTAL`].
    text: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, ClaimsError> {
        let (&b, rest) = self.rest.split_first().ok_or(ClaimsError::Truncated)?;
        self.rest = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ClaimsError> {
        if n > self.rest.len() {
            return Err(ClaimsError::Truncated);
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(head)
    }

    /// LEB128, minimal, at most 64 bits.
    fn uvar(&mut self) -> Result<u64, ClaimsError> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.byte()?;
            let low = u64::from(b & 0x7f);
            if i == 9 && b > 1 {
                return Err(ClaimsError::Malformed("varint overflow"));
            }
            v |= low << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && b == 0 {
                    return Err(ClaimsError::Malformed("varint not minimal"));
                }
                return Ok(v);
            }
        }
        Err(ClaimsError::Malformed("varint too long"))
    }

    /// A count of at most `max` entries of at least `min_each` bytes.
    fn count(
        &mut self,
        max: usize,
        min_each: usize,
        what: &'static str,
    ) -> Result<usize, ClaimsError> {
        let n = self.uvar()?;
        if n > max as u64 {
            return Err(ClaimsError::Limit(what));
        }
        let n = n as usize;
        if n.saturating_mul(min_each) > self.rest.len() {
            return Err(ClaimsError::Truncated);
        }
        Ok(n)
    }

    /// The adult section, checked as [`AdultEnrichment::check`] would.
    fn adult(&mut self) -> Result<AdultEnrichment, ClaimsError> {
        let kind = match self.byte()? {
            0 => None,
            1 => Some(AdultKind::Scene),
            2 => Some(AdultKind::Movie),
            3 => Some(AdultKind::Vr),
            _ => return Err(ClaimsError::Malformed("adult kind")),
        };
        let flags = self.byte()?;
        if flags & !ADULT_KNOWN != 0 {
            return Err(ClaimsError::Malformed("unknown adult bits"));
        }
        let has = |bit| flags & bit != 0;
        let scene_id = self.text(1, MAX_SCENE_ID, "scene id")?;
        if !scene_id_ok(&scene_id) {
            return Err(ClaimsError::Malformed("scene id"));
        }
        let title = has(TITLE)
            .then(|| self.text(1, MAX_SCENE_TITLE, "scene title"))
            .transpose()?;
        let studio = has(STUDIO)
            .then(|| self.text(1, MAX_STUDIO, "studio"))
            .transpose()?;
        let released = has(RELEASED)
            .then(|| {
                let b = self.take(4)?;
                let opt = |v: u8| (v != 0).then_some(v);
                ReleaseDate::new(u16::from_be_bytes([b[0], b[1]]), opt(b[2]), opt(b[3]))
                    .ok_or(ClaimsError::Malformed("release date"))
            })
            .transpose()?;
        let n = self.count(MAX_PERFORMERS, 2, "performers")?;
        let mut performers = Vec::with_capacity(n);
        for _ in 0..n {
            performers.push(self.text(1, MAX_PERFORMER, "performer")?);
        }
        Ok(AdultEnrichment {
            scene_id,
            kind,
            title,
            studio,
            released,
            performers,
            performers_truncated: has(TRUNCATED),
        })
    }

    /// Counts `n` bytes toward [`MAX_TOTAL`] without reading them.
    fn charge(&mut self, n: usize) -> Result<(), ClaimsError> {
        self.text += n;
        if self.text > MAX_TOTAL {
            return Err(ClaimsError::Limit("total"));
        }
        Ok(())
    }

    /// The details section, checked as [`Details::check`] would. Each
    /// entry's fixed charge counts before its list is reserved.
    fn details(&mut self) -> Result<Details, ClaimsError> {
        self.charge(DETAILS_COST)?;
        let flags = self.byte()?;
        if flags & !DETAILS_KNOWN != 0 {
            return Err(ClaimsError::Malformed("unknown details bits"));
        }
        let has = |bit| flags & bit != 0;
        let source_category = has(CATEGORY)
            .then(|| self.text(1, MAX_CATEGORY, "source category"))
            .transpose()?;
        let posted_at = has(POSTED_AT)
            .then(|| {
                let t = self.uvar().map(|z| ((z >> 1) as i64) ^ -((z & 1) as i64))?;
                if (0..=MAX_POSTED_AT).contains(&t) {
                    Ok(t)
                } else {
                    Err(ClaimsError::Malformed("posted time"))
                }
            })
            .transpose()?;
        let canonical_title = has(CANONICAL)
            .then(|| self.text(1, MAX_CANONICAL_TITLE, "canonical title"))
            .transpose()?;
        let year = has(YEAR)
            .then(|| {
                let b = self.take(2)?;
                let y = u16::from_be_bytes([b[0], b[1]]);
                if (1..=9999).contains(&y) {
                    Ok(y)
                } else {
                    Err(ClaimsError::Malformed("year"))
                }
            })
            .transpose()?;
        let match_confidence = has(MATCH_CONFIDENCE)
            .then(|| match self.byte()? {
                m if m <= MAX_MATCH_CONFIDENCE => Ok(m),
                _ => Err(ClaimsError::Malformed("match confidence")),
            })
            .transpose()?;

        let n = self.count(MAX_NEWSGROUPS, 2, "newsgroups")?;
        self.charge(n * STRING_COST)?;
        let mut newsgroups = Vec::with_capacity(n);
        for _ in 0..n {
            let g = self.text(1, MAX_NEWSGROUP, "newsgroup")?;
            if !newsgroup_ok(&g) {
                return Err(ClaimsError::Malformed("newsgroup"));
            }
            if newsgroups.contains(&g) {
                return Err(ClaimsError::Malformed("duplicate newsgroup"));
            }
            newsgroups.push(g);
        }
        let n = self.count(MAX_ALT_TITLES, 4, "alternative titles")?;
        self.charge(n * ALT_TITLE_COST)?;
        let mut alt_titles = Vec::with_capacity(n);
        for _ in 0..n {
            let language = self.text(1, MAX_LANGUAGE, "language")?;
            if !language_ok(&language) {
                return Err(ClaimsError::Malformed("language"));
            }
            let title = self.text(1, MAX_ALT_TITLE, "alternative title")?;
            let a = AltTitle { language, title };
            if alt_titles.contains(&a) {
                return Err(ClaimsError::Malformed("duplicate alternative title"));
            }
            alt_titles.push(a);
        }
        let n = self.count(MAX_GENRES, 2, "genres")?;
        self.charge(n * STRING_COST)?;
        let mut genres = Vec::with_capacity(n);
        for _ in 0..n {
            let g = self.text(1, MAX_GENRE, "genre")?;
            if !genre_ok(&g) {
                return Err(ClaimsError::Malformed("genre"));
            }
            if genres.contains(&g) {
                return Err(ClaimsError::Malformed("duplicate genre"));
            }
            genres.push(g);
        }
        let d = Details {
            source_category,
            posted_at,
            newsgroups,
            alt_titles,
            canonical_title,
            genres,
            year,
            match_confidence,
        };
        if d.is_empty() {
            return Err(ClaimsError::Malformed("empty details"));
        }
        Ok(d)
    }

    fn text(&mut self, min: usize, max: usize, what: &'static str) -> Result<String, ClaimsError> {
        let n = self.uvar()?;
        if n < min as u64 || n > max as u64 {
            return Err(ClaimsError::Limit(what));
        }
        let n = n as usize;
        self.text += n;
        if self.text > MAX_TOTAL {
            return Err(ClaimsError::Limit("total"));
        }
        let bytes = self.take(n)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ClaimsError::Malformed("utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Claims {
        Claims {
            name: Some("Some.Show.S01E01.1080p".into()),
            size: Some(1_234_567_890),
            files: vec![
                ClaimedFile {
                    path: "Some.Show.S01E01.mkv".into(),
                    size: Some(1_234_000_000),
                },
                ClaimedFile {
                    path: String::new(),
                    size: None,
                },
            ],
            trackers: vec!["udp://tracker.example:1337/announce".into()],
            ids: vec![ExternalId {
                scheme: "imdb".into(),
                value: "tt0111161".into(),
            }],
            description: Some("é".repeat(100)),
            publisher: Some("uploader".into()),
            provenance: Provenance {
                external_ref: Some("https://feed.example/item/1".into()),
                published_at: Some(-5),
                confidence: Confidence::Medium,
            },
            adult: None,
            details: None,
        }
    }

    fn adult_sample() -> Claims {
        Claims {
            adult: Some(AdultEnrichment {
                scene_id: "0b9e4c1a-6c1f-4f7e-9d0a-2f3e4d5c6b7a".into(),
                kind: Some(AdultKind::Scene),
                title: Some("A Scene Title".into()),
                studio: Some("Studio Name".into()),
                released: ReleaseDate::new(2024, Some(2), Some(29)),
                performers: vec!["Performer One".into(), "Performer Two".into()],
                performers_truncated: true,
            }),
            description: None,
            publisher: None,
            ids: Vec::new(),
            ..sample()
        }
    }

    /// `encode(&sample(), 3)` as SRC-1's encoder wrote it, before ENR-0:
    /// version 1, zstd. Generated once from `origin/main` and committed.
    const V1_GOLDEN: &[u8] = &[
        1, 1, 40, 181, 47, 253, 96, 97, 0, 133, 4, 0, 36, 8, 63, 1, 22, 83, 111, 109, 101, 46, 83,
        104, 111, 119, 46, 83, 48, 49, 69, 48, 49, 46, 49, 48, 56, 48, 112, 210, 133, 216, 204, 4,
        200, 1, 195, 169, 8, 117, 112, 108, 111, 97, 100, 101, 114, 27, 104, 116, 116, 112, 115,
        58, 47, 47, 102, 101, 101, 100, 46, 101, 120, 97, 109, 112, 108, 101, 47, 105, 116, 101,
        109, 47, 49, 9, 2, 20, 109, 107, 118, 1, 128, 177, 181, 204, 4, 0, 0, 1, 35, 117, 100, 112,
        58, 47, 47, 116, 114, 97, 99, 107, 101, 114, 58, 49, 51, 51, 55, 47, 97, 110, 110, 111,
        117, 110, 99, 101, 1, 4, 105, 109, 100, 98, 9, 116, 116, 48, 49, 49, 49, 49, 54, 49, 3, 0,
        2, 65, 12, 64, 172, 89, 107, 24, 229, 200,
    ];

    #[test]
    fn a_v1_golden_blob_still_decodes() {
        let c = decode(V1_GOLDEN).unwrap();
        assert_eq!(c, sample());
        // The writer still lays version 1 out byte for byte.
        let body = zstd::bulk::decompress(&V1_GOLDEN[2..], MAX_PLAIN).unwrap();
        assert_eq!(encode_plain(&c).unwrap(), body);
        assert_eq!(encode(&c, 3).unwrap()[0], VERSION);
        let mut raw = vec![VERSION, RAW];
        raw.extend_from_slice(&body);
        assert_eq!(decode(&raw).unwrap(), c);
    }

    #[test]
    fn enrichment_round_trips_as_version_2() {
        let minimal = Claims {
            adult: Some(AdultEnrichment::new("1")),
            ..Claims::default()
        };
        for c in [adult_sample(), minimal] {
            let blob = encode(&c, 3).unwrap();
            assert_eq!(blob[0], VERSION_ADULT);
            assert_eq!(decode(&blob).unwrap(), c);
            let plain = encode_plain(&c).unwrap();
            assert_eq!(decode_plain(&plain).unwrap(), c);
            let mut raw = vec![VERSION_ADULT, RAW];
            raw.extend_from_slice(&plain);
            assert_eq!(decode(&raw).unwrap(), c);
        }
        // Only the section makes version 2.
        let mut c = adult_sample();
        c.adult = None;
        assert_eq!(encode(&c, 3).unwrap()[0], VERSION);
    }

    #[test]
    fn the_smallest_section() {
        let c = Claims {
            adult: Some(AdultEnrichment::new("7")),
            ..Claims::default()
        };
        assert_eq!(
            encode(&c, 3).unwrap(),
            [VERSION_ADULT, RAW, ADULT, 0, 0, 0, 0, 0, 0, 1, b'7', 0]
        );
    }

    #[test]
    fn version_and_section_must_agree() {
        let with = encode_plain(&adult_sample()).unwrap();
        let without = encode_plain(&sample()).unwrap();
        for (version, body) in [(VERSION, &with), (VERSION_ADULT, &without)] {
            let mut blob = vec![version, RAW];
            blob.extend_from_slice(body);
            assert_eq!(
                decode(&blob),
                Err(ClaimsError::Malformed("section and version disagree"))
            );
            let mut z = vec![version, ZSTD];
            z.extend_from_slice(&zstd::bulk::compress(body, 3).unwrap());
            assert!(decode(&z).is_err());
        }
        assert_eq!(decode(&[4, 0]), Err(ClaimsError::Version(4)));
        assert_eq!(decode(&[0, 0]), Err(ClaimsError::Version(0)));
        // Bit 7 is the details section now: an empty one is refused.
        assert_eq!(
            decode_plain(&[DETAILS, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(ClaimsError::Malformed("empty details"))
        );
    }

    /// An empty-claims body with an adult section of these bytes.
    fn section(bytes: &[u8]) -> Vec<u8> {
        let mut b = vec![ADULT, 0, 0, 0, 0];
        b.extend_from_slice(bytes);
        b
    }

    fn str_bytes(s: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        put_uvar(&mut b, s.len() as u64);
        b.extend_from_slice(s);
        b
    }

    #[test]
    fn malformed_sections_are_refused() {
        let id = str_bytes(b"abc");
        let cases: Vec<(Vec<u8>, ClaimsError)> = vec![
            // Kind and flags.
            (
                [&[4, 0][..], &id, &[0]].concat(),
                ClaimsError::Malformed("adult kind"),
            ),
            (
                [&[0xff, 0][..], &id, &[0]].concat(),
                ClaimsError::Malformed("adult kind"),
            ),
            (
                [&[0, 0x10][..], &id, &[0]].concat(),
                ClaimsError::Malformed("unknown adult bits"),
            ),
            (
                [&[0, 0x80][..], &id, &[0]].concat(),
                ClaimsError::Malformed("unknown adult bits"),
            ),
            // The scene id: empty, a space, a control byte, punctuation a
            // link could escape with, non-ASCII, bad UTF-8.
            (vec![0, 0, 0, 0], ClaimsError::Limit("scene id")),
            (
                [&[0, 0][..], &str_bytes(b"a b"), &[0]].concat(),
                ClaimsError::Malformed("scene id"),
            ),
            (
                [&[0, 0][..], &str_bytes(b"a\n"), &[0]].concat(),
                ClaimsError::Malformed("scene id"),
            ),
            (
                [&[0, 0][..], &str_bytes("é".as_bytes()), &[0]].concat(),
                ClaimsError::Malformed("scene id"),
            ),
            (
                [&[0, 0][..], &str_bytes(b"../x"), &[0]].concat(),
                ClaimsError::Malformed("scene id"),
            ),
            (
                [&[0, 0][..], &str_bytes(b"a?b#c%d_e"), &[0]].concat(),
                ClaimsError::Malformed("scene id"),
            ),
            (
                [&[0, 0][..], &str_bytes(&[0xff]), &[0]].concat(),
                ClaimsError::Malformed("utf-8"),
            ),
            // Empty strings where a flag says one is present.
            (
                [&[0, TITLE][..], &id, &[0, 0]].concat(),
                ClaimsError::Limit("scene title"),
            ),
            (
                [&[0, STUDIO][..], &id, &[0, 0]].concat(),
                ClaimsError::Limit("studio"),
            ),
            (
                [&[0, 0][..], &id, &[1, 0, 0]].concat(),
                ClaimsError::Limit("performer"),
            ),
            // Bad UTF-8 in the text fields.
            (
                [&[0, TITLE][..], &id, &str_bytes(&[0xc3]), &[0]].concat(),
                ClaimsError::Malformed("utf-8"),
            ),
            (
                [&[0, STUDIO][..], &id, &str_bytes(&[0xed, 0xa0, 0x80]), &[0]].concat(),
                ClaimsError::Malformed("utf-8"),
            ),
            (
                [&[0, 0][..], &id, &[1], &str_bytes(&[0xfe])].concat(),
                ClaimsError::Malformed("utf-8"),
            ),
            // A count past what's left, a non-minimal count, trailing bytes.
            (
                [&[0, 0][..], &id, &[5, 1, b'a']].concat(),
                ClaimsError::Truncated,
            ),
            (
                [&[0, 0][..], &id, &[0x80, 0]].concat(),
                ClaimsError::Malformed("varint not minimal"),
            ),
            ([&[0, 0][..], &id, &[0, 0]].concat(), ClaimsError::Trailing),
        ];
        for (bytes, want) in cases {
            assert_eq!(decode_plain(&section(&bytes)), Err(want), "{bytes:?}");
        }
    }

    #[test]
    fn bad_dates_are_refused() {
        let id = str_bytes(b"1");
        for date in [
            [0, 0, 0, 0],        // year 0
            [0x27, 0x10, 0, 0],  // year 10000
            [0xff, 0xff, 1, 1],  // year 65535
            [0x07, 0xe8, 13, 0], // month 13
            [0x07, 0xe8, 0, 5],  // a day without a month
            [0x07, 0xe8, 1, 32], // January 32nd
            [0x07, 0xe7, 2, 29], // 2023 isn't a leap year
            [0x07, 0x6c, 2, 29], // nor is 1900
            [0x07, 0xe8, 4, 31], // April 31st
        ] {
            let bytes = [&[0, RELEASED][..], &id, &date, &[0]].concat();
            assert_eq!(
                decode_plain(&section(&bytes)),
                Err(ClaimsError::Malformed("release date")),
                "{date:?}"
            );
        }
        // 2000 is a leap year; a year or a month alone is a date.
        for (date, want) in [
            ([0x07, 0xd0, 2, 29], "2000-02-29"),
            ([0x07, 0xe8, 0, 0], "2024"),
            ([0x07, 0xe8, 12, 0], "2024-12"),
            ([0, 1, 1, 1], "0001-01-01"),
            ([0x27, 0x0f, 12, 31], "9999-12-31"),
        ] {
            let bytes = [&[0, RELEASED][..], &id, &date, &[0]].concat();
            let c = decode_plain(&section(&bytes)).unwrap();
            let d = c.adult.unwrap().released.unwrap();
            assert_eq!(d.to_string(), want);
            assert_eq!(ReleaseDate::parse(want), Some(d));
        }
    }

    #[test]
    fn release_dates_parse_strictly() {
        for bad in [
            "",
            "202",
            "20245",
            "2024-1",
            "2024-1-01",
            "2024-01-1",
            "2024-13-01",
            "2024-02-30",
            "0000",
            "0000-01-01",
            "2024-00",
            "2024-01-00",
            "2024/01/01",
            "2024-01-01T00",
            " 2024",
            "+024",
            "2024-01-",
            "２０２４",
            "2024-0a-01",
        ] {
            assert_eq!(ReleaseDate::parse(bad), None, "{bad:?}");
        }
        assert_eq!(ReleaseDate::new(2024, None, Some(1)), None);
        assert_eq!(ReleaseDate::new(0, None, None), None);
        let d = ReleaseDate::parse("1999-12-31").unwrap();
        assert_eq!((d.year(), d.month(), d.day()), (1999, Some(12), Some(31)));
    }

    /// A cut blob or body is refused at every length, never half-decoded.
    #[test]
    fn a_cut_section_is_refused_at_every_byte() {
        let c = adult_sample();
        let plain = encode_plain(&c).unwrap();
        let mut blob = vec![VERSION_ADULT, RAW];
        blob.extend_from_slice(&plain);
        let start = encode_plain(&Claims {
            adult: None,
            ..c.clone()
        })
        .unwrap()
        .len();
        for n in 0..plain.len() {
            assert!(decode_plain(&plain[..n]).is_err(), "cut at {n}");
        }
        for n in 0..blob.len() {
            assert!(decode(&blob[..n]).is_err(), "cut at {n}");
        }
        // The section itself starts where version 1's fields end.
        assert!(start < plain.len());
        assert_eq!(decode(&blob).unwrap(), c);
    }

    /// Each cap: a value at it encodes and decodes; one over is refused by
    /// the encoder, and by the decoder when written by hand.
    #[test]
    fn every_adult_cap_holds_at_the_cap_and_one_over() {
        type Set = fn(&mut AdultEnrichment, usize);
        let caps: [(&str, usize, Set); 5] = [
            ("scene id", MAX_SCENE_ID, |a, n| a.scene_id = "i".repeat(n)),
            ("scene title", MAX_SCENE_TITLE, |a, n| {
                a.title = Some("t".repeat(n))
            }),
            ("studio", MAX_STUDIO, |a, n| a.studio = Some("s".repeat(n))),
            ("performers", MAX_PERFORMERS, |a, n| {
                a.performers = vec!["p".into(); n]
            }),
            ("performer", MAX_PERFORMER, |a, n| {
                a.performers = vec!["p".repeat(n)]
            }),
        ];
        for (what, cap, set) in caps {
            let mut a = AdultEnrichment::new("1");
            set(&mut a, cap);
            let mut c = Claims {
                adult: Some(a),
                ..Claims::default()
            };
            let plain = encode_plain(&c).unwrap_or_else(|e| panic!("{what} at cap: {e}"));
            assert_eq!(decode_plain(&plain).unwrap(), c, "{what}");
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c, "{what}");

            set(c.adult.as_mut().unwrap(), cap + 1);
            assert!(encode(&c, 3).is_err(), "{what} over cap encodes");
            // The decoder refuses the same bytes the encoder would write.
            let bytes = unchecked_plain(&c);
            assert!(decode_plain(&bytes).is_err(), "{what} over cap decodes");
            let mut blob = vec![VERSION_ADULT, RAW];
            blob.extend_from_slice(&bytes);
            assert!(decode(&blob).is_err(), "{what} over cap decodes");
        }
    }

    /// The plain body laid out without [`Claims::check`], to craft bodies
    /// the encoder refuses.
    fn unchecked_plain(c: &Claims) -> Vec<u8> {
        let a = c.adult.as_ref().unwrap();
        let mut out = unchecked_v1(c);
        out[0] |= ADULT;
        out.push(a.kind.map_or(0, |k| k as u8));
        out.push(
            (u8::from(a.title.is_some()) * TITLE)
                | (u8::from(a.studio.is_some()) * STUDIO)
                | (u8::from(a.released.is_some()) * RELEASED)
                | (u8::from(a.performers_truncated) * TRUNCATED),
        );
        out.extend(str_bytes(a.scene_id.as_bytes()));
        for s in [&a.title, &a.studio].into_iter().flatten() {
            out.extend(str_bytes(s.as_bytes()));
        }
        if let Some(d) = a.released {
            out.extend(d.year().to_be_bytes());
            out.extend([d.month().unwrap_or(0), d.day().unwrap_or(0)]);
        }
        put_uvar(&mut out, a.performers.len() as u64);
        for p in &a.performers {
            out.extend(str_bytes(p.as_bytes()));
        }
        out
    }

    #[test]
    fn unchecked_plain_matches_the_encoder() {
        let c = adult_sample();
        assert_eq!(unchecked_plain(&c), encode_plain(&c).unwrap());
    }

    /// A description, publisher or external id can't ride along beside a
    /// section, on either side.
    #[test]
    fn enrichment_claims_carry_no_description_publisher_or_ids() {
        let set: [fn(&mut Claims); 3] = [
            |c| c.description = Some("d".into()),
            |c| c.publisher = Some("p".into()),
            |c| {
                c.ids = vec![ExternalId {
                    scheme: "stashdb-performer".into(),
                    value: "0b9e4c1a".into(),
                }]
            },
        ];
        for f in set {
            let mut c = adult_sample();
            f(&mut c);
            assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("enrichment claims")));
            assert_eq!(
                decode_plain(&unchecked_plain(&c)),
                Err(ClaimsError::Malformed("enrichment claims"))
            );
            // Without the section, the same fields are fine.
            c.adult = None;
            assert!(encode(&c, 3).is_ok());
        }
    }

    /// Every version 1 cap: at it encodes and decodes; one over is refused
    /// by the encoder, and by the decoder when written by hand.
    #[test]
    fn every_v1_cap_holds_at_the_cap_and_one_over() {
        type Set = fn(&mut Claims, usize);
        let caps: [(&str, usize, Set); 10] = [
            ("name", MAX_NAME, |c, n| c.name = Some("n".repeat(n))),
            ("files", MAX_FILES, |c, n| {
                c.files = vec![
                    ClaimedFile {
                        path: String::new(),
                        size: None
                    };
                    n
                ]
            }),
            ("path", MAX_PATH, |c, n| {
                c.files = vec![ClaimedFile {
                    path: "p".repeat(n),
                    size: None,
                }]
            }),
            ("trackers", MAX_TRACKERS, |c, n| {
                c.trackers = vec!["t".into(); n]
            }),
            ("tracker", MAX_TRACKER, |c, n| {
                c.trackers = vec!["t".repeat(n)]
            }),
            ("ids", MAX_IDS, |c, n| {
                c.ids = vec![
                    ExternalId {
                        scheme: "s".into(),
                        value: "v".into()
                    };
                    n
                ]
            }),
            ("id part", MAX_ID_PART, |c, n| {
                c.ids = vec![ExternalId {
                    scheme: "s".repeat(n),
                    value: "v".repeat(n),
                }]
            }),
            ("description", MAX_DESCRIPTION, |c, n| {
                c.description = Some("d".repeat(n))
            }),
            ("publisher", MAX_PUBLISHER, |c, n| {
                c.publisher = Some("p".repeat(n))
            }),
            ("external reference", MAX_EXTERNAL_REF, |c, n| {
                c.provenance.external_ref = Some("e".repeat(n))
            }),
        ];
        for (what, cap, set) in caps {
            let mut c = Claims::default();
            set(&mut c, cap);
            let plain = encode_plain(&c).unwrap_or_else(|e| panic!("{what} at cap: {e}"));
            assert_eq!(decode_plain(&plain).unwrap(), c, "{what}");
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c, "{what}");

            set(&mut c, cap + 1);
            assert!(encode(&c, 3).is_err(), "{what} over cap encodes");
            let bytes = unchecked_v1(&c);
            assert!(decode_plain(&bytes).is_err(), "{what} over cap decodes");
            let mut blob = vec![VERSION, RAW];
            blob.extend_from_slice(&bytes);
            assert!(decode(&blob).is_err(), "{what} over cap decodes");
        }
    }

    /// A version 1 plain body laid out without [`Claims::check`].
    fn unchecked_v1(c: &Claims) -> Vec<u8> {
        let p = &c.provenance;
        let mut out = vec![
            (u8::from(c.name.is_some()) * NAME)
                | (u8::from(c.size.is_some()) * SIZE)
                | (u8::from(c.description.is_some()) * DESCRIPTION)
                | (u8::from(c.publisher.is_some()) * PUBLISHER)
                | (u8::from(p.external_ref.is_some()) * EXTERNAL_REF)
                | (u8::from(p.published_at.is_some()) * PUBLISHED_AT),
            p.confidence as u8,
        ];
        let s = |out: &mut Vec<u8>, v: &Option<String>| {
            if let Some(v) = v {
                out.extend(str_bytes(v.as_bytes()));
            }
        };
        s(&mut out, &c.name);
        if let Some(n) = c.size {
            put_uvar(&mut out, n);
        }
        s(&mut out, &c.description);
        s(&mut out, &c.publisher);
        s(&mut out, &p.external_ref);
        if let Some(t) = p.published_at {
            put_uvar(&mut out, ((t << 1) ^ (t >> 63)) as u64);
        }
        put_uvar(&mut out, c.files.len() as u64);
        for f in &c.files {
            out.extend(str_bytes(f.path.as_bytes()));
            match f.size {
                Some(n) => {
                    out.push(1);
                    put_uvar(&mut out, n);
                }
                None => out.push(0),
            }
        }
        put_uvar(&mut out, c.trackers.len() as u64);
        for t in &c.trackers {
            out.extend(str_bytes(t.as_bytes()));
        }
        put_uvar(&mut out, c.ids.len() as u64);
        for i in &c.ids {
            out.extend(str_bytes(i.scheme.as_bytes()));
            out.extend(str_bytes(i.value.as_bytes()));
        }
        out
    }

    #[test]
    fn unchecked_v1_matches_the_encoder() {
        let c = sample();
        assert_eq!(unchecked_v1(&c), encode_plain(&c).unwrap());
    }

    #[test]
    fn encode_refuses_a_bad_section() {
        let bad: [fn(&mut AdultEnrichment); 7] = [
            |a| a.scene_id = String::new(),
            |a| a.scene_id = "a/b".into(),
            |a| a.scene_id = "a b".into(),
            |a| a.scene_id = "ü".into(),
            |a| a.title = Some(String::new()),
            |a| a.studio = Some(String::new()),
            |a| a.performers = vec![String::new()],
        ];
        for f in bad {
            let mut c = adult_sample();
            f(c.adult.as_mut().unwrap());
            assert!(matches!(encode(&c, 3), Err(ClaimsError::Limit(_))), "{c:?}");
        }
    }

    /// The section's text counts toward [`MAX_TOTAL`], like every other
    /// claim, so the stored-blob bound (and V14's `CHECK`) still holds.
    #[test]
    fn the_section_counts_toward_the_total() {
        let mut c = Claims {
            adult: Some(AdultEnrichment {
                performers: vec!["p".repeat(MAX_PERFORMER); MAX_PERFORMERS],
                ..AdultEnrichment::new("1")
            }),
            ..Claims::default()
        };
        let left = MAX_TOTAL - c.text_len();
        c.files = vec![
            ClaimedFile {
                path: "f".repeat(MAX_PATH),
                size: None
            };
            left / MAX_PATH
        ];
        c.files.push(ClaimedFile {
            path: "f".repeat(left % MAX_PATH),
            size: None,
        });
        assert_eq!(c.text_len(), MAX_TOTAL);
        assert!(encode(&c, 3).is_ok());
        c.files.last_mut().unwrap().path.push('f');
        assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("total")));
        assert!(decode_plain(&unchecked_plain(&c)).is_err());
    }

    /// No performer id, birth date or other personal field can be held:
    /// this pattern names every field, so adding one fails to compile.
    #[test]
    fn the_section_has_nowhere_to_put_a_person() {
        let AdultEnrichment {
            scene_id: _,
            kind: _,
            title: _,
            studio: _,
            released: _,
            performers,
            performers_truncated: _,
        } = AdultEnrichment::new("1");
        // A performer is a bare name: no struct that could grow an id.
        let _: Vec<String> = performers;
    }

    #[test]
    fn round_trips_raw_and_compressed() {
        for c in [sample(), Claims::default()] {
            let blob = encode(&c, 3).unwrap();
            assert_eq!(blob[0], VERSION);
            assert_eq!(decode(&blob).unwrap(), c);
        }
        let big = Claims {
            description: Some("abc ".repeat(10_000)),
            ..Claims::default()
        };
        let blob = encode(&big, 3).unwrap();
        assert_eq!(blob[1], ZSTD, "repetitive text compresses");
        assert!(blob.len() < 1000);
        assert_eq!(decode(&blob).unwrap(), big);
    }

    #[test]
    fn the_empty_body_is_the_smallest() {
        assert_eq!(
            encode_plain(&Claims::default()).unwrap(),
            vec![0, 0, 0, 0, 0]
        );
        assert_eq!(
            encode(&Claims::default(), 3).unwrap(),
            vec![1, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        assert_eq!(decode(&[]), Err(ClaimsError::Truncated));
        for v in [0, 4, 5, 0xff] {
            assert_eq!(decode(&[v, 0]), Err(ClaimsError::Version(v)));
        }
        assert_eq!(decode(&[1]), Err(ClaimsError::Truncated));
        assert_eq!(decode(&[1, 9, 0]), Err(ClaimsError::Codec(9)));
        assert_eq!(decode(&[1, 1, 0, 1, 2]), Err(ClaimsError::Compression));
        assert_eq!(
            decode_plain(&[0, 3, 0, 0, 0]),
            Err(ClaimsError::Malformed("confidence"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0, 0, 0, 0]),
            Err(ClaimsError::Trailing)
        );
        // An empty name, a non-minimal count, a huge count, bad UTF-8.
        assert_eq!(
            decode_plain(&[NAME, 0, 0, 0, 0, 0]),
            Err(ClaimsError::Limit("name"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0x80, 0, 0, 0]),
            Err(ClaimsError::Malformed("varint not minimal"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0xff, 0xff, 0xff, 0x7f, 0, 0]),
            Err(ClaimsError::Limit("files"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 100, 0, 0]),
            Err(ClaimsError::Truncated)
        );
        assert_eq!(
            decode_plain(&[NAME, 0, 1, 0xff, 0, 0, 0]),
            Err(ClaimsError::Malformed("utf-8"))
        );
        assert_eq!(
            decode_plain(&[
                SIZE, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02
            ]),
            Err(ClaimsError::Malformed("varint overflow"))
        );
    }

    #[test]
    fn u64_and_i64_extremes_round_trip() {
        for (size, at) in [(u64::MAX, i64::MIN), (0, i64::MAX), (127, 0), (128, -1)] {
            let c = Claims {
                size: Some(size),
                files: vec![ClaimedFile {
                    path: "f".into(),
                    size: Some(size),
                }],
                provenance: Provenance {
                    published_at: Some(at),
                    ..Provenance::default()
                },
                ..Claims::default()
            };
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c);
        }
    }

    #[test]
    fn encode_refuses_unclamped_claims() {
        let cases = [
            Claims {
                name: Some(String::new()),
                ..Claims::default()
            },
            Claims {
                name: Some("x".repeat(MAX_NAME + 1)),
                ..Claims::default()
            },
            Claims {
                ids: vec![ExternalId {
                    scheme: "IMDB".into(),
                    value: "1".into(),
                }],
                ..Claims::default()
            },
            Claims {
                trackers: vec![String::new()],
                ..Claims::default()
            },
            Claims {
                files: vec![
                    ClaimedFile {
                        path: "p".repeat(MAX_PATH),
                        size: None
                    };
                    MAX_TOTAL / MAX_PATH + 1
                ],
                ..Claims::default()
            },
        ];
        for c in cases {
            assert!(encode(&c, 3).is_err(), "{c:?}");
        }
    }

    #[test]
    fn the_largest_claims_fit_the_bounds() {
        // Every field at its limit, then files up to the text total: once
        // with every version 1 field, once with the adult section (which
        // goes without a description, publisher or ids).
        let full = Claims {
            name: Some("n".repeat(MAX_NAME)),
            size: Some(u64::MAX),
            files: Vec::new(),
            trackers: vec!["t".repeat(MAX_TRACKER); MAX_TRACKERS],
            ids: vec![
                ExternalId {
                    scheme: "s".repeat(MAX_ID_PART),
                    value: "v".repeat(MAX_ID_PART)
                };
                MAX_IDS
            ],
            description: Some("d".repeat(MAX_DESCRIPTION)),
            publisher: Some("p".repeat(MAX_PUBLISHER)),
            provenance: Provenance {
                external_ref: Some("e".repeat(MAX_EXTERNAL_REF)),
                published_at: Some(i64::MIN),
                confidence: Confidence::High,
            },
            adult: Some(AdultEnrichment {
                scene_id: "i".repeat(MAX_SCENE_ID),
                kind: Some(AdultKind::Vr),
                title: Some("t".repeat(MAX_SCENE_TITLE)),
                studio: Some("s".repeat(MAX_STUDIO)),
                released: ReleaseDate::new(9999, Some(12), Some(31)),
                performers: vec!["p".repeat(MAX_PERFORMER); MAX_PERFORMERS],
                performers_truncated: true,
            }),
            details: None,
        };
        let v1 = Claims {
            adult: None,
            ..full.clone()
        };
        let v3 = Claims {
            adult: None,
            description: None,
            publisher: None,
            ids: v3::full_ids(),
            details: Some(Box::new(v3::full_details())),
            ..full.clone()
        };
        let v2 = Claims {
            ids: Vec::new(),
            description: None,
            publisher: None,
            ..full
        };
        for mut c in [v1, v2, v3] {
            let room = MAX_TOTAL - c.text_len();
            let short = room / MAX_FILES;
            c.files = vec![
                ClaimedFile {
                    path: "f".repeat(short),
                    size: Some(u64::MAX)
                };
                MAX_FILES
            ];
            let plain = encode_plain(&c).unwrap();
            assert!(plain.len() <= MAX_PLAIN, "{} > {MAX_PLAIN}", plain.len());
            assert_eq!(decode_plain(&plain).unwrap(), c);
            let blob = encode(&c, 3).unwrap();
            assert!(blob.len() <= MAX_STORED);
            assert_eq!(decode(&blob).unwrap(), c);
        }
    }

    /// Long paths need a 2-byte length: the worst framing per file still
    /// fits the bound, raw and compressed.
    #[test]
    fn the_worst_framing_fits_the_bounds() {
        for path in [
            "p".repeat(128),
            (0..128u8).map(|i| (b'!' + i % 90) as char).collect(),
        ] {
            let mut c = Claims {
                size: Some(u64::MAX),
                provenance: Provenance {
                    external_ref: Some("e".repeat(MAX_EXTERNAL_REF)),
                    published_at: Some(i64::MIN),
                    confidence: Confidence::High,
                },
                // The section's most framing: every flag, a 2-byte title
                // and studio length, the most performers, and the least
                // text for them.
                adult: Some(AdultEnrichment {
                    scene_id: "i".into(),
                    kind: Some(AdultKind::Movie),
                    title: Some("t".repeat(128)),
                    studio: Some("s".repeat(128)),
                    released: ReleaseDate::new(1, Some(1), Some(1)),
                    performers: vec!["p".into(); MAX_PERFORMERS],
                    performers_truncated: true,
                }),
                ..Claims::default()
            };
            for details in [None, Some(v3::most_framing())] {
                if let Some(d) = details {
                    c.adult = None;
                    c.details = Some(Box::new(d));
                }
                worst_framing(c.clone(), &path);
            }
        }
    }

    fn worst_framing(mut c: Claims, path: &str) {
        let long = (MAX_TOTAL - c.text_len()) / 128;
        c.files = (0..MAX_FILES)
            .map(|i| ClaimedFile {
                path: if i < long {
                    path.to_owned()
                } else {
                    String::new()
                },
                size: Some(u64::MAX),
            })
            .collect();
        assert!(c.check().is_ok());
        let plain = encode_plain(&c).unwrap();
        assert!(plain.len() <= MAX_PLAIN, "{} > {MAX_PLAIN}", plain.len());
        let blob = encode(&c, 3).unwrap();
        assert!(blob.len() <= MAX_STORED);
        assert_eq!(decode(&blob).unwrap(), c);
    }

    #[test]
    fn debug_shows_no_text() {
        let c = adult_sample();
        let s = format!("{c:?} {:?} {:?} {:?}", c.files, c.ids, c.adult);
        assert!(!s.contains("Some.Show"), "{s}");
        assert!(!s.contains("tt0111161"), "{s}");
        assert!(!s.contains("uploader"), "{s}");
        for t in ["0b9e4c1a", "Scene Title", "Studio", "Performer"] {
            assert!(!s.contains(t), "{s}");
        }
    }

    /// Version 3, the details section (ENR-9).
    mod v3 {
        use super::super::*;
        use super::{put_uvar, sample, str_bytes, unchecked_v1};

        pub(super) fn details() -> Details {
            Details {
                source_category: Some("TV > HD".into()),
                posted_at: Some(1_700_000_000),
                newsgroups: vec!["alt.binaries.example".into(), "alt.binaries.hdtv".into()],
                alt_titles: vec![
                    AltTitle {
                        language: "ja".into(),
                        title: "某番組".into(),
                    },
                    AltTitle {
                        language: "zh-hant-tw".into(),
                        title: "某節目".into(),
                    },
                    AltTitle {
                        language: "und".into(),
                        title: "Some Show".into(),
                    },
                ],
                canonical_title: Some("Some Show".into()),
                genres: vec!["drama".into(), "science fiction".into(), "ドラマ".into()],
                year: Some(2011),
                match_confidence: Some(87),
            }
        }

        /// A usenet item's claims with an enrichment-shaped section.
        pub(super) fn v3_sample() -> Claims {
            Claims {
                ids: vec![
                    ExternalId {
                        scheme: "imdb".into(),
                        value: "tt0111161".into(),
                    },
                    ExternalId {
                        scheme: "tmdb".into(),
                        value: "278".into(),
                    },
                ],
                description: None,
                publisher: None,
                details: Some(Box::new(details())),
                ..sample()
            }
        }

        /// 32 distinct valid ids, each as long as its scheme allows.
        pub(super) fn full_ids() -> Vec<ExternalId> {
            (0..MAX_IDS as u64)
                .map(|i| ExternalId {
                    scheme: "steam".into(),
                    value: format!("{}", 10_000_000_000_000_000_000u64 + i),
                })
                .collect()
        }

        /// Every field at its cap.
        pub(super) fn full_details() -> Details {
            Details {
                source_category: Some("c".repeat(MAX_CATEGORY)),
                posted_at: Some(MAX_POSTED_AT),
                newsgroups: (0..MAX_NEWSGROUPS)
                    .map(|i| format!("{i}{}", "g".repeat(MAX_NEWSGROUP - 1)))
                    .collect(),
                alt_titles: (0..MAX_ALT_TITLES)
                    .map(|i| AltTitle {
                        // 2 + 3 × 9 + 6 = 35 bytes.
                        language: format!("en{}-abcde", "-abcdefgh".repeat(3)),
                        title: format!("{i:02}{}", "t".repeat(MAX_ALT_TITLE - 2)),
                    })
                    .collect(),
                canonical_title: Some("n".repeat(MAX_CANONICAL_TITLE)),
                genres: (0..MAX_GENRES)
                    .map(|i| format!("{i:02}{}", "g".repeat(MAX_GENRE - 2)))
                    .collect(),
                year: Some(9999),
                match_confidence: Some(MAX_MATCH_CONFIDENCE),
            }
        }

        /// The most framing for the least text: every flag, 2-byte
        /// lengths for the strings, and every list at its count cap.
        pub(super) fn most_framing() -> Details {
            Details {
                source_category: Some("c".repeat(128)),
                posted_at: Some(MAX_POSTED_AT),
                newsgroups: (b'a'..b'a' + MAX_NEWSGROUPS as u8)
                    .map(|c| (c as char).to_string())
                    .collect(),
                alt_titles: (0..MAX_ALT_TITLES)
                    .map(|i| AltTitle {
                        language: "en".into(),
                        title: format!("{i:02}{}", "t".repeat(126)),
                    })
                    .collect(),
                canonical_title: Some("n".repeat(128)),
                genres: (0..MAX_GENRES).map(|i| format!("{i}")).collect(),
                year: Some(1),
                match_confidence: Some(0),
            }
        }

        /// The plain body laid out without [`Claims::check`], to craft
        /// bodies the encoder refuses.
        pub(super) fn unchecked(c: &Claims) -> Vec<u8> {
            let d = c.details.as_ref().unwrap();
            let mut out = unchecked_v1(c);
            out[0] |= DETAILS;
            out.push(
                (u8::from(d.source_category.is_some()) * CATEGORY)
                    | (u8::from(d.posted_at.is_some()) * POSTED_AT)
                    | (u8::from(d.canonical_title.is_some()) * CANONICAL)
                    | (u8::from(d.year.is_some()) * YEAR)
                    | (u8::from(d.match_confidence.is_some()) * MATCH_CONFIDENCE),
            );
            if let Some(s) = &d.source_category {
                out.extend(str_bytes(s.as_bytes()));
            }
            if let Some(t) = d.posted_at {
                put_uvar(&mut out, ((t << 1) ^ (t >> 63)) as u64);
            }
            if let Some(s) = &d.canonical_title {
                out.extend(str_bytes(s.as_bytes()));
            }
            if let Some(y) = d.year {
                out.extend(y.to_be_bytes());
            }
            if let Some(m) = d.match_confidence {
                out.push(m);
            }
            put_uvar(&mut out, d.newsgroups.len() as u64);
            for g in &d.newsgroups {
                out.extend(str_bytes(g.as_bytes()));
            }
            put_uvar(&mut out, d.alt_titles.len() as u64);
            for a in &d.alt_titles {
                out.extend(str_bytes(a.language.as_bytes()));
                out.extend(str_bytes(a.title.as_bytes()));
            }
            put_uvar(&mut out, d.genres.len() as u64);
            for g in &d.genres {
                out.extend(str_bytes(g.as_bytes()));
            }
            out
        }

        fn only(d: Details) -> Claims {
            Claims {
                details: Some(Box::new(d)),
                ..Claims::default()
            }
        }

        #[test]
        fn the_helpers_are_valid() {
            assert!(full_details().check().is_ok());
            assert!(most_framing().check().is_ok());
            assert!(full_ids().iter().all(|i| id_ok(&i.scheme, &i.value)));
            let d = full_details();
            assert!(
                d.alt_titles
                    .iter()
                    .all(|a| a.language.len() == MAX_LANGUAGE)
            );
            assert!(d.newsgroups.iter().all(|g| g.len() == MAX_NEWSGROUP));
            let c = v3_sample();
            assert_eq!(unchecked(&c), encode_plain(&c).unwrap());
            let c = Claims {
                ids: full_ids(),
                details: Some(Box::new(full_details())),
                ..Claims::default()
            };
            assert_eq!(unchecked(&c), encode_plain(&c).unwrap());
        }

        #[test]
        fn round_trips_as_version_3() {
            let minimal = only(Details {
                year: Some(1999),
                ..Details::default()
            });
            for c in [v3_sample(), minimal, only(full_details())] {
                let blob = encode(&c, 3).unwrap();
                assert_eq!(blob[0], VERSION_DETAILS);
                assert_eq!(decode(&blob).unwrap(), c);
                let plain = encode_plain(&c).unwrap();
                assert_eq!(decode_plain(&plain).unwrap(), c);
                let mut raw = vec![VERSION_DETAILS, RAW];
                raw.extend_from_slice(&plain);
                assert_eq!(decode(&raw).unwrap(), c);
            }
            // Without the section the same claims stay version 1, byte
            // for byte as before ENR-9.
            let mut c = v3_sample();
            c.details = None;
            let blob = encode(&c, 3).unwrap();
            assert_eq!(blob[0], VERSION);
            assert_eq!(decode(&blob).unwrap(), c);
        }

        #[test]
        fn the_smallest_section() {
            let c = only(Details {
                match_confidence: Some(5),
                ..Details::default()
            });
            assert_eq!(
                encode(&c, 3).unwrap(),
                [
                    VERSION_DETAILS,
                    RAW,
                    DETAILS,
                    0,
                    0,
                    0,
                    0,
                    MATCH_CONFIDENCE,
                    5,
                    0,
                    0,
                    0
                ]
            );
        }

        #[test]
        fn version_and_section_must_agree() {
            let v3 = encode_plain(&v3_sample()).unwrap();
            let v2 = encode_plain(&super::adult_sample()).unwrap();
            let v1 = encode_plain(&sample()).unwrap();
            for (version, body) in [
                (VERSION, &v3),
                (VERSION_ADULT, &v3),
                (VERSION_DETAILS, &v1),
                (VERSION_DETAILS, &v2),
            ] {
                let mut blob = vec![version, RAW];
                blob.extend_from_slice(body);
                assert_eq!(
                    decode(&blob),
                    Err(ClaimsError::Malformed("section and version disagree")),
                    "{version}"
                );
                let mut z = vec![version, ZSTD];
                z.extend_from_slice(&zstd::bulk::compress(body, 3).unwrap());
                assert!(decode(&z).is_err());
            }
        }

        /// One section a row: an adult match and a details section never
        /// share claims, on either side.
        #[test]
        fn no_adult_section_beside_details() {
            let c = Claims {
                details: Some(Box::new(details())),
                ..super::adult_sample()
            };
            assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("enrichment claims")));
            let mut body = super::unchecked_plain(&Claims {
                details: None,
                ..c.clone()
            });
            body[0] |= DETAILS;
            let tail = unchecked(&Claims {
                details: c.details.clone(),
                ..Claims::default()
            });
            body.extend_from_slice(&tail[5..]);
            assert_eq!(
                decode_plain(&body),
                Err(ClaimsError::Malformed("enrichment claims"))
            );
        }

        /// No overview or poster beside a details section, on either side:
        /// either could name a person (plan §4.3, owner §2.37).
        #[test]
        fn details_claims_carry_no_description_or_publisher() {
            let set: [fn(&mut Claims); 2] = [
                |c| c.description = Some("d".into()),
                |c| c.publisher = Some("poster@example.com".into()),
            ];
            for f in set {
                let mut c = v3_sample();
                f(&mut c);
                assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("details claims")));
                assert_eq!(
                    decode_plain(&unchecked(&c)),
                    Err(ClaimsError::Malformed("details claims"))
                );
                c.details = None;
                assert!(encode(&c, 3).is_ok(), "fine without the section");
            }
        }

        #[test]
        fn ids_beside_details_are_enrichment_ids() {
            let bad = [
                ("stashdb-performer", "0b9e4c1a"),
                ("imdb", "nm0000151"),
                ("imdb", "tt1"),
                ("tmdb", "0278"),
                ("olid", "OL23919A"),
                ("discogs", "a1"),
                ("wikidata", "P31"),
                ("mbid", "F27EC8DB-AF05-4F36-916E-3D57F91ECF5E"),
            ];
            for (scheme, value) in bad {
                let mut c = v3_sample();
                c.ids.push(ExternalId {
                    scheme: scheme.into(),
                    value: value.into(),
                });
                assert_eq!(
                    encode(&c, 3),
                    Err(ClaimsError::Limit("enrichment ids")),
                    "{scheme}:{value}"
                );
                assert_eq!(
                    decode_plain(&unchecked(&c)),
                    Err(ClaimsError::Malformed("enrichment ids")),
                    "{scheme}:{value}"
                );
                // Version 1 claims keep any scheme, as before.
                c.details = None;
                assert!(encode(&c, 3).is_ok());
            }
            let mut c = v3_sample();
            c.ids.push(c.ids[0].clone());
            assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("enrichment ids")));
            assert_eq!(
                decode_plain(&unchecked(&c)),
                Err(ClaimsError::Malformed("enrichment ids"))
            );
        }

        #[test]
        fn id_forms_are_one_spelling_each() {
            let good = [
                ("imdb", "tt0111161"),
                ("imdb", "tt1234567890"),
                ("imdb", "tt10872600"),
                ("tmdb", "278"),
                ("tvdb", "81189"),
                ("anidb", "1"),
                ("anilist", "21"),
                ("igdb", "1020"),
                ("steam", "570"),
                ("steam", "99999999999999999999"),
                ("mbid", "f27ec8db-af05-4f36-916e-3d57f91ecf5e"),
                ("olid", "OL7353617M"),
                ("olid", "OL45804W"),
                ("discogs", "r249504"),
                ("discogs", "m33876"),
                ("wikidata", "Q42"),
            ];
            for (s, v) in good {
                assert!(id_ok(s, v), "{s}:{v}");
                assert!(ENRICHMENT_SCHEMES.contains(&s));
            }
            let bad = [
                ("imdb", "tt123456"),
                ("imdb", "tt12345678901"),
                ("imdb", "tt00111161"),
                ("imdb", "tt0000111161"),
                ("imdb", "0111161"),
                ("imdb", "TT0111161"),
                ("imdb", "nm0000151"),
                ("imdb", "tt011116a"),
                ("tmdb", ""),
                ("tmdb", "0"),
                ("tmdb", "01"),
                ("tmdb", "-1"),
                ("tmdb", "+1"),
                ("tmdb", "1 "),
                ("tmdb", "١٢"),
                ("tmdb", "123456789012345678901"),
                ("mbid", "f27ec8db-af05-4f36-916e-3d57f91ecf5"),
                ("mbid", "f27ec8dbaaf05-4f36-916e-3d57f91ecf5e"),
                ("mbid", "g27ec8db-af05-4f36-916e-3d57f91ecf5e"),
                ("olid", "OL123A"),
                ("olid", "OLM"),
                ("olid", "ol123M"),
                ("discogs", "123"),
                ("discogs", "r"),
                ("discogs", "a123"),
                ("wikidata", "q42"),
                ("wikidata", "L1"),
                ("wikidata", "Q042"),
                ("tvrage", "1"),
                ("", "1"),
            ];
            for (s, v) in bad {
                assert!(!id_ok(s, v), "{s}:{v}");
            }
        }

        #[test]
        fn shapes_of_languages_groups_and_genres() {
            for t in [
                "en",
                "und",
                "zh-hant-tw",
                "sr-latn-rs",
                "es-419",
                "en-us-x-a",
            ] {
                assert!(language_ok(t), "{t}");
            }
            let long = format!("en{}", "-abcdefgh".repeat(4));
            for t in [
                "",
                "e",
                "EN",
                "en-US",
                "en_us",
                "en--us",
                "-en",
                "en-",
                "1a",
                "en-abcdefghi",
                "é",
                "en us",
                &long,
            ] {
                assert!(!language_ok(t), "{t}");
            }
            for g in ["alt.binaries.hdtv", "a", "a.b+c-d_e", "Alt.Binaries.X"] {
                assert!(newsgroup_ok(g), "{g}");
            }
            let long = "g".repeat(MAX_NEWSGROUP + 1);
            for g in [
                "", ".a", "a.", "a..b", "a b", "a,b", "a/b", "é", "a*", &long,
            ] {
                assert!(!newsgroup_ok(g), "{g}");
            }
            for g in [
                "drama",
                "science fiction",
                "ドラマ",
                "sci-fi & fantasy",
                "é",
                // Non-ASCII whitespace is the normaliser's to fold, not
                // the check's: its Unicode property can change.
                "a\u{a0}b",
            ] {
                assert!(genre_ok(g), "{g}");
            }
            let long = "g".repeat(MAX_GENRE + 1);
            for g in [
                "", " drama", "drama ", "sci  fi", "Drama", "a\tb", "a\nb", "a\u{0}", "a\u{85}b",
                &long,
            ] {
                assert!(!genre_ok(g), "{g}");
            }
        }

        /// Every cap: at it encodes and decodes; one over (or a bad
        /// entry) is refused by the encoder, and by the decoder when
        /// written by hand.
        #[test]
        fn every_v3_cap_holds_at_the_cap_and_one_over() {
            type Set = fn(&mut Details, bool);
            let caps: [(&str, Set); 12] = [
                ("category", |d, over| {
                    d.source_category = Some("c".repeat(MAX_CATEGORY + usize::from(over)))
                }),
                ("canonical", |d, over| {
                    d.canonical_title = Some("n".repeat(MAX_CANONICAL_TITLE + usize::from(over)))
                }),
                ("newsgroups", |d, over| {
                    d.newsgroups = (0..MAX_NEWSGROUPS + usize::from(over))
                        .map(|i| format!("g{i}"))
                        .collect()
                }),
                ("newsgroup", |d, over| {
                    d.newsgroups = vec!["g".repeat(MAX_NEWSGROUP + usize::from(over))]
                }),
                ("alt titles", |d, over| {
                    d.alt_titles = (0..MAX_ALT_TITLES + usize::from(over))
                        .map(|i| AltTitle {
                            language: "en".into(),
                            title: format!("t{i}"),
                        })
                        .collect()
                }),
                ("alt title", |d, over| {
                    d.alt_titles = vec![AltTitle {
                        language: "en".into(),
                        title: "t".repeat(MAX_ALT_TITLE + usize::from(over)),
                    }]
                }),
                ("language", |d, over| {
                    d.alt_titles = vec![AltTitle {
                        language: if over {
                            "en-".repeat(12) + "a"
                        } else {
                            "en-".repeat(11) + "ab"
                        },
                        title: "t".into(),
                    }]
                }),
                ("genres", |d, over| {
                    d.genres = (0..MAX_GENRES + usize::from(over))
                        .map(|i| format!("g{i}"))
                        .collect()
                }),
                ("genre", |d, over| {
                    d.genres = vec!["g".repeat(MAX_GENRE + usize::from(over))]
                }),
                ("year", |d, over| {
                    d.year = Some(if over { 10_000 } else { 9999 })
                }),
                ("confidence", |d, over| {
                    d.match_confidence = Some(MAX_MATCH_CONFIDENCE + u8::from(over))
                }),
                ("posted time", |d, over| {
                    d.posted_at = Some(MAX_POSTED_AT + i64::from(over))
                }),
            ];
            for (what, set) in caps {
                let mut d = Details::default();
                set(&mut d, false);
                let c = only(d.clone());
                let plain = encode_plain(&c).unwrap_or_else(|e| panic!("{what} at cap: {e}"));
                assert_eq!(decode_plain(&plain).unwrap(), c, "{what}");
                assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c, "{what}");

                set(&mut d, true);
                let c = only(d);
                assert!(encode(&c, 3).is_err(), "{what} over cap encodes");
                let bytes = unchecked(&c);
                assert!(decode_plain(&bytes).is_err(), "{what} over cap decodes");
                let mut blob = vec![VERSION_DETAILS, RAW];
                blob.extend_from_slice(&bytes);
                assert!(decode(&blob).is_err(), "{what} over cap decodes");
            }
            // Before 1970 is refused too, at -1 and at the far end.
            for t in [-1, i64::MIN] {
                let c = only(Details {
                    posted_at: Some(t),
                    ..Details::default()
                });
                assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("posted time")));
                assert_eq!(
                    decode_plain(&unchecked(&c)),
                    Err(ClaimsError::Malformed("posted time"))
                );
            }
            let c = only(Details {
                posted_at: Some(0),
                ..Details::default()
            });
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c);
            // Year 0 is no year, not a year.
            let c = only(Details {
                year: Some(0),
                ..Details::default()
            });
            assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("year")));
            assert_eq!(
                decode_plain(&unchecked(&c)),
                Err(ClaimsError::Malformed("year"))
            );
        }

        /// Refused entries and duplicates, on both sides.
        #[test]
        fn bad_entries_are_refused() {
            type Set = fn(&mut Details);
            let bad: [(Set, ClaimsError, ClaimsError); 12] = [
                (
                    |d| d.source_category = Some(String::new()),
                    ClaimsError::Limit("source category"),
                    ClaimsError::Limit("source category"),
                ),
                (
                    |d| d.canonical_title = Some(String::new()),
                    ClaimsError::Limit("canonical title"),
                    ClaimsError::Limit("canonical title"),
                ),
                (
                    |d| d.newsgroups = vec!["a b".into()],
                    ClaimsError::Limit("newsgroups"),
                    ClaimsError::Malformed("newsgroup"),
                ),
                (
                    |d| d.newsgroups = vec!["a.b".into(), "a.b".into()],
                    ClaimsError::Limit("newsgroups"),
                    ClaimsError::Malformed("duplicate newsgroup"),
                ),
                (
                    |d| d.newsgroups = vec![String::new()],
                    ClaimsError::Limit("newsgroups"),
                    ClaimsError::Limit("newsgroup"),
                ),
                (
                    |d| {
                        d.alt_titles = vec![AltTitle {
                            language: "EN".into(),
                            title: "t".into(),
                        }]
                    },
                    ClaimsError::Limit("alternative titles"),
                    ClaimsError::Malformed("language"),
                ),
                (
                    |d| {
                        d.alt_titles = vec![AltTitle {
                            language: String::new(),
                            title: "t".into(),
                        }]
                    },
                    ClaimsError::Limit("alternative titles"),
                    ClaimsError::Limit("language"),
                ),
                (
                    |d| {
                        d.alt_titles = vec![AltTitle {
                            language: "en".into(),
                            title: String::new(),
                        }]
                    },
                    ClaimsError::Limit("alternative titles"),
                    ClaimsError::Limit("alternative title"),
                ),
                (
                    |d| {
                        let a = AltTitle {
                            language: "en".into(),
                            title: "t".into(),
                        };
                        d.alt_titles = vec![a.clone(), a]
                    },
                    ClaimsError::Limit("alternative titles"),
                    ClaimsError::Malformed("duplicate alternative title"),
                ),
                (
                    |d| d.genres = vec!["Drama".into()],
                    ClaimsError::Limit("genres"),
                    ClaimsError::Malformed("genre"),
                ),
                (
                    |d| d.genres = vec!["drama".into(), "drama".into()],
                    ClaimsError::Limit("genres"),
                    ClaimsError::Malformed("duplicate genre"),
                ),
                (
                    |d| *d = Details::default(),
                    ClaimsError::Limit("empty details"),
                    ClaimsError::Malformed("empty details"),
                ),
            ];
            for (set, enc, dec) in bad {
                let mut d = Details {
                    year: Some(2000),
                    ..Details::default()
                };
                set(&mut d);
                let c = only(d);
                assert_eq!(encode(&c, 3), Err(enc));
                assert_eq!(decode_plain(&unchecked(&c)), Err(dec), "{enc:?}");
            }
            // The same title in two languages is two entries.
            let c = only(Details {
                alt_titles: ["en", "de"]
                    .map(|l| AltTitle {
                        language: l.into(),
                        title: "Same".into(),
                    })
                    .to_vec(),
                ..Details::default()
            });
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c);
        }

        /// An empty-claims body with a details section of these bytes.
        fn section(bytes: &[u8]) -> Vec<u8> {
            let mut b = vec![DETAILS, 0, 0, 0, 0];
            b.extend_from_slice(bytes);
            b
        }

        #[test]
        fn malformed_sections_are_refused() {
            let cases: Vec<(Vec<u8>, ClaimsError)> = vec![
                (
                    vec![0x20, 0, 0, 0],
                    ClaimsError::Malformed("unknown details bits"),
                ),
                (
                    vec![0x80, 0, 0, 0],
                    ClaimsError::Malformed("unknown details bits"),
                ),
                (vec![YEAR, 0, 0, 0, 0, 0], ClaimsError::Malformed("year")),
                (
                    vec![YEAR, 0x27, 0x10, 0, 0, 0],
                    ClaimsError::Malformed("year"),
                ),
                (vec![YEAR, 0x07], ClaimsError::Truncated),
                (
                    vec![MATCH_CONFIDENCE, 101, 0, 0, 0],
                    ClaimsError::Malformed("match confidence"),
                ),
                (
                    vec![MATCH_CONFIDENCE, 0xff, 0, 0, 0],
                    ClaimsError::Malformed("match confidence"),
                ),
                (
                    vec![POSTED_AT, 0x80, 0, 0, 0, 0],
                    ClaimsError::Malformed("varint not minimal"),
                ),
                (
                    vec![CATEGORY, 0, 0, 0, 0],
                    ClaimsError::Limit("source category"),
                ),
                (
                    [&[CATEGORY][..], &str_bytes(&[0xff]), &[0, 0, 0]].concat(),
                    ClaimsError::Malformed("utf-8"),
                ),
                // Counts past their cap or past what's left.
                (vec![0, 9, 0, 0], ClaimsError::Limit("newsgroups")),
                (vec![0, 0, 33, 0], ClaimsError::Limit("alternative titles")),
                (vec![0, 0, 0, 17], ClaimsError::Limit("genres")),
                (vec![0, 3, 1, b'a'], ClaimsError::Truncated),
                (
                    vec![0, 0, 2, 2, b'e', b'n', 1, b't'],
                    ClaimsError::Truncated,
                ),
                (
                    [&[0, 0, 1][..], &str_bytes(b"en"), &str_bytes(&[0xc3]), &[0]].concat(),
                    ClaimsError::Malformed("utf-8"),
                ),
                // A whole valid section with a byte after it.
                (vec![YEAR, 0x07, 0xd0, 0, 0, 0, 0], ClaimsError::Trailing),
                (vec![0, 0, 0, 0], ClaimsError::Malformed("empty details")),
            ];
            for (bytes, want) in cases {
                assert_eq!(decode_plain(&section(&bytes)), Err(want), "{bytes:?}");
            }
            assert!(decode_plain(&section(&[YEAR, 0x07, 0xd0, 0, 0, 0])).is_ok());
        }

        #[test]
        fn a_cut_section_is_refused_at_every_byte() {
            let plain = encode_plain(&v3_sample()).unwrap();
            for n in 0..plain.len() {
                assert!(decode_plain(&plain[..n]).is_err(), "{n}");
                let mut blob = vec![VERSION_DETAILS, RAW];
                blob.extend_from_slice(&plain[..n]);
                assert!(decode(&blob).is_err(), "{n}");
            }
        }

        /// The section counts toward the total at its charged size, so a
        /// row stays within `unverified::MAX_ROW` and the files make room.
        #[test]
        fn the_section_counts_toward_the_total() {
            let d = full_details();
            assert!(d.charged_len() > d.text_len());
            let mut c = Claims {
                ids: full_ids(),
                details: Some(Box::new(d)),
                files: vec![
                    ClaimedFile {
                        path: "f".repeat(MAX_PATH),
                        size: None
                    };
                    MAX_TOTAL / MAX_PATH
                ],
                ..Claims::default()
            };
            assert_eq!(encode(&c, 3), Err(ClaimsError::Limit("total")));
            let bytes = unchecked(&c);
            assert_eq!(decode_plain(&bytes), Err(ClaimsError::Limit("total")));
            // Exactly at the total, both sides accept it.
            let room = MAX_TOTAL - (c.text_len() - c.files.len() * MAX_PATH);
            c.files.truncate(room / MAX_PATH);
            c.files.push(ClaimedFile {
                path: "f".repeat(room % MAX_PATH),
                size: None,
            });
            assert_eq!(c.text_len(), MAX_TOTAL);
            let blob = encode(&c, 3).unwrap();
            assert_eq!(decode(&blob).unwrap(), c);
            assert!(c.decoded_size() <= crate::unverified::MAX_ROW);
        }

        #[test]
        fn the_largest_v3_row_fits_max_row() {
            let mut c = Claims {
                trackers: vec!["t".into(); MAX_TRACKERS],
                ids: full_ids(),
                details: Some(Box::new(most_framing())),
                ..Claims::default()
            };
            let room = MAX_TOTAL - c.text_len();
            c.files = vec![
                ClaimedFile {
                    path: String::new(),
                    size: None
                };
                MAX_FILES
            ];
            c.name = Some("n".repeat(room.min(MAX_NAME)));
            assert!(c.check().is_ok());
            assert!(
                c.decoded_size() <= crate::unverified::MAX_ROW,
                "{} > {}",
                c.decoded_size(),
                crate::unverified::MAX_ROW
            );
        }

        #[test]
        fn debug_shows_no_text() {
            let c = v3_sample();
            let s = format!("{c:?}");
            for t in [
                "某",
                "Some Show",
                "drama",
                "alt.binaries",
                "TV > HD",
                "tt0111161",
            ] {
                assert!(!s.contains(t), "{s}");
            }
            assert!(s.contains("zh-hant-tw"), "languages are shown: {s}");
        }

        /// The section has nowhere to put a person: this lists every
        /// field, so adding one fails to compile here first.
        #[test]
        fn the_section_has_nowhere_to_put_a_person() {
            let Details {
                source_category: _,
                posted_at: _,
                newsgroups: _,
                alt_titles: _,
                canonical_title: _,
                genres: _,
                year: _,
                match_confidence: _,
            } = details();
            let AltTitle {
                language: _,
                title: _,
            } = details().alt_titles[0].clone();
        }

        mod props {
            use super::super::super::*;
            use super::{full_ids, section};
            use proptest::prelude::*;

            fn text(max: usize) -> impl Strategy<Value = String> {
                prop_oneof![
                    4 => proptest::string::string_regex("(.|\\n){1,24}")
                        .unwrap()
                        .prop_filter("within", move |s| s.len() <= max),
                    1 => Just("x".repeat(max)),
                ]
            }

            fn dedup<T: PartialEq>(v: Vec<T>) -> Vec<T> {
                let mut out = Vec::new();
                for x in v {
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
                out
            }

            fn details() -> impl Strategy<Value = Details> {
                (
                    (
                        proptest::option::of(text(MAX_CATEGORY)),
                        proptest::option::of(0..=MAX_POSTED_AT),
                        proptest::collection::vec(
                            "[a-z0-9+_-]{1,10}(\\.[a-zA-Z0-9]{1,10}){0,3}",
                            0..=MAX_NEWSGROUPS,
                        ),
                        proptest::collection::vec(
                            ("[a-z]{2,3}(-[a-z0-9]{1,8}){0,3}", text(MAX_ALT_TITLE))
                                .prop_map(|(language, title)| AltTitle { language, title }),
                            0..6,
                        ),
                    ),
                    (
                        proptest::option::of(text(MAX_CANONICAL_TITLE)),
                        proptest::collection::vec(
                            "[a-z0-9&]{1,10}( [a-z\\p{Han}]{1,6}){0,2}",
                            0..=MAX_GENRES,
                        ),
                        proptest::option::of(1u16..=9999),
                        proptest::option::of(0u8..=MAX_MATCH_CONFIDENCE),
                    ),
                )
                    .prop_map(
                        |(
                            (source_category, posted_at, ng, alt),
                            (canonical_title, genres, year, m),
                        )| Details {
                            source_category,
                            posted_at,
                            newsgroups: dedup(ng),
                            alt_titles: dedup(alt),
                            canonical_title,
                            genres: dedup(genres),
                            year,
                            match_confidence: m,
                        },
                    )
                    .prop_filter("not empty", |d| !d.is_empty())
            }

            proptest! {
                #![proptest_config(ProptestConfig::with_cases(512))]

                /// Version 3: round-trips, stays version 3, and is
                /// canonical.
                #[test]
                fn v3_round_trips(d in details(), ids in 0usize..4, level in 1i32..6) {
                    let c = Claims {
                        name: Some("n".into()),
                        ids: full_ids()[..ids].to_vec(),
                        details: Some(Box::new(d)),
                        ..Claims::default()
                    };
                    prop_assert!(c.check().is_ok(), "{:?}", c);
                    let blob = encode(&c, level).unwrap();
                    prop_assert_eq!(blob[0], VERSION_DETAILS);
                    prop_assert_eq!(&decode(&blob).unwrap(), &c);
                    let plain = encode_plain(&c).unwrap();
                    prop_assert_eq!(encode_plain(&decode_plain(&plain).unwrap()).unwrap(), plain);
                }

                /// Whatever bytes follow a details prefix, decoding never
                /// panics and what it accepts is canonical.
                #[test]
                fn any_section_bytes_are_safe(tail in proptest::collection::vec(any::<u8>(), 0..64)) {
                    let body = section(&tail);
                    if let Ok(c) = decode_plain(&body) {
                        prop_assert!(c.check().is_ok());
                        prop_assert_eq!(encode_plain(&c).unwrap(), body);
                    }
                }
            }
        }
    }

    mod props {
        use super::super::*;
        use proptest::prelude::*;

        /// Up to `max` bytes, often short, sometimes exactly `max`.
        fn text(max: usize) -> impl Strategy<Value = String> {
            prop_oneof![
                4 => proptest::string::string_regex("(.|\\n){1,24}")
                    .unwrap()
                    .prop_filter("within", move |s| s.len() <= max),
                1 => Just("x".repeat(max)),
                1 => Just("é".repeat(max / 2)),
            ]
        }

        fn date() -> impl Strategy<Value = ReleaseDate> {
            (
                1u16..=9999,
                proptest::option::of(1u8..=12),
                proptest::option::of(1u8..=31),
            )
                .prop_filter_map("a real date", |(y, m, d)| {
                    ReleaseDate::new(y, m, d.filter(|_| m.is_some()))
                })
        }

        fn kind() -> impl Strategy<Value = Option<AdultKind>> {
            proptest::option::of(prop_oneof![
                Just(AdultKind::Scene),
                Just(AdultKind::Movie),
                Just(AdultKind::Vr),
            ])
        }

        fn adult() -> impl Strategy<Value = AdultEnrichment> {
            (
                prop_oneof![
                    4 => "[A-Za-z0-9-]{1,40}",
                    1 => Just("i".repeat(MAX_SCENE_ID)),
                ],
                kind(),
                proptest::option::of(text(MAX_SCENE_TITLE)),
                proptest::option::of(text(MAX_STUDIO)),
                proptest::option::of(date()),
                prop_oneof![
                    4 => proptest::collection::vec(text(MAX_PERFORMER), 0..8),
                    1 => Just(vec!["p".repeat(MAX_PERFORMER); MAX_PERFORMERS]),
                ],
                any::<bool>(),
            )
                .prop_map(
                    |(
                        scene_id,
                        kind,
                        title,
                        studio,
                        released,
                        performers,
                        performers_truncated,
                    )| {
                        AdultEnrichment {
                            scene_id,
                            kind,
                            title,
                            studio,
                            released,
                            performers,
                            performers_truncated,
                        }
                    },
                )
        }

        fn claims() -> impl Strategy<Value = Claims> {
            (
                proptest::option::of(text(MAX_NAME)),
                proptest::option::of(any::<u64>()),
                proptest::collection::vec(
                    ("(.){0,20}", proptest::option::of(any::<u64>()))
                        .prop_map(|(path, size)| ClaimedFile { path, size }),
                    0..6,
                ),
                proptest::option::of(text(MAX_PUBLISHER)),
                proptest::option::of(any::<i64>()),
            )
                .prop_map(|(name, size, files, publisher, published_at)| Claims {
                    name,
                    size,
                    files,
                    publisher,
                    provenance: Provenance {
                        published_at,
                        ..Provenance::default()
                    },
                    ..Claims::default()
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// Version 1: round-trips, stays version 1, and is canonical.
            #[test]
            fn v1_round_trips(c in claims(), level in 1i32..6) {
                let blob = encode(&c, level).unwrap();
                prop_assert_eq!(blob[0], VERSION);
                prop_assert_eq!(&decode(&blob).unwrap(), &c);
                let plain = encode_plain(&c).unwrap();
                prop_assert_eq!(encode_plain(&decode_plain(&plain).unwrap()).unwrap(), plain);
            }

            /// Version 2: the same, with any section within its caps.
            #[test]
            fn v2_round_trips(mut c in claims(), a in adult(), level in 1i32..6) {
                c.publisher = None;
                c.adult = Some(a);
                let blob = encode(&c, level).unwrap();
                prop_assert_eq!(blob[0], VERSION_ADULT);
                prop_assert_eq!(&decode(&blob).unwrap(), &c);
                let plain = encode_plain(&c).unwrap();
                prop_assert_eq!(encode_plain(&decode_plain(&plain).unwrap()).unwrap(), plain);
            }

            /// Any field one over its cap is refused by both sides.
            #[test]
            fn one_over_any_cap_is_refused(mut a in adult(), which in 0usize..5) {
                match which {
                    0 => a.scene_id = "i".repeat(MAX_SCENE_ID + 1),
                    1 => a.title = Some("t".repeat(MAX_SCENE_TITLE + 1)),
                    2 => a.studio = Some("s".repeat(MAX_STUDIO + 1)),
                    3 => a.performers = vec!["p".into(); MAX_PERFORMERS + 1],
                    _ => a.performers.push("p".repeat(MAX_PERFORMER + 1)),
                }
                let c = Claims { adult: Some(a), ..Claims::default() };
                prop_assert!(encode(&c, 1).is_err());
                prop_assert!(decode_plain(&super::unchecked_plain(&c)).is_err());
            }

            /// Every date the type holds prints and parses back.
            #[test]
            fn dates_print_and_parse(d in date()) {
                prop_assert_eq!(ReleaseDate::parse(&d.to_string()), Some(d));
            }

            /// Whatever bytes follow a valid prefix, decoding never panics
            /// and what it accepts is canonical.
            #[test]
            fn any_section_bytes_are_safe(tail in proptest::collection::vec(any::<u8>(), 0..64)) {
                let body = super::section(&tail);
                if let Ok(c) = decode_plain(&body) {
                    prop_assert_eq!(encode_plain(&c).unwrap(), body);
                }
            }
        }
    }
}
