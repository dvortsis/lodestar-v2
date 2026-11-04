//! StashDB enrichment answers (architecture §20.9; plan
//! `docs/plan/2026-11-04-v06-rulepacks.md`, ENR-3): the JSON a stash-box
//! GraphQL server returns for the two fixed query documents below.
//!
//! The safety rules are [`archive_org`](crate::archive_org)'s. It's a pure
//! function from bytes to scenes with no I/O. It reads one scene at a
//! time, stops at a structural cap with a [`Stop`] reason and keeps the
//! scenes it finished, and skips (and counts) a scene whose own id, date
//! or field types are bad, or that has no id. A half-read scene is never
//! used: a scene is handed out only once its object has closed and every
//! field in it checked out. It never panics.
//!
//! Extra evidence degrades instead of spoiling the scene (runner, ENR-3
//! review): a long title is cut, a studio with a bad or long name is
//! dropped, and a bad performer entry, or one past
//! [`Limits::max_performers`], is left out and sets
//! [`Scene::performers_truncated`].
//!
//! It reads with the bounded [`json`](crate::json) reader, so every cap
//! holds while reading. A structural fault (bad JSON, input cut short, a
//! depth or value cap) ends the read: the reader's cursor is then
//! mid-token, so every read propagates its `Err` and nothing reads on.
//!
//! **What it keeps, and what it never asks for.** The queries select only
//! what the enrichment claim can store (plan §4.3, ENR-0): the scene's id,
//! title, release date and studio, and each performer's name. They
//! never select a performer's id (no per-performer pages or cross-item
//! lists, plan Q4), birth date, measurements, birthplace,
//! aliases, images or any other personal field (plan Q5), and the parser
//! has nowhere to put one if a server sends it anyway: unknown keys are
//! read past, never copied. Performers come back beside the match
//! ([`Scene::performers`]), not inside it, so a caller that mustn't keep
//! them (plan Q4) drops them with [`Scene::matched`] alone.
//!
//! The shape is GraphQL's: `{"data": {"searchScene": [scene, …]}}` or
//! `{"data": {"findScene": scene | null}}`, with an optional `errors`
//! array, which is counted and never read.

use crate::common::{self, Bad, clean_title, days_in_month, prepare, small, unsafe_char};
pub use crate::common::{MAX_TITLE, Stop};
use crate::json::{self, R, Reader, TextError, Walk};

/// Searches scenes by text: the display name, or the parsed site, date
/// and title (ENR-1 decides which). Variables: `term` (required) and
/// `limit`. Answers `{"data": {"searchScene": [scene, …]}}`.
pub const SEARCH_SCENE: &str = "query SearchScene($term: String!, $limit: Int) { \
searchScene(term: $term, limit: $limit) { \
id title release_date studio { id name } performers { performer { name } } } }";

/// Looks one scene up by its StashDB id. Variable: `id`. Answers
/// `{"data": {"findScene": scene}}`, or `null` for no such scene.
///
/// The parser doesn't know which query an answer is for, so it reads
/// either field. A caller of this one checks that the scene's id is the
/// one it asked for.
pub const FIND_SCENE: &str = "query FindScene($id: ID!) { \
findScene(id: $id) { \
id title release_date studio { id name } performers { performer { name } } } }";

/// Bytes in a StashDB id: a UUID, `8-4-4-4-12` hex digits.
pub const ID_LEN: usize = 36;
/// Bytes in a kept studio or performer name.
pub const MAX_NAME: usize = 256;
/// The page a kept scene id links to ([`Match::link`]).
pub const SCENES: &str = "https://stashdb.org/scenes/";

/// Caps on one parse. The defaults are [`LIMITS`]; why each value:
/// `docs/decisions/2026-11-04-ENR-3.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Scenes, kept or skipped ([`Stop::Items`]). A `null` `findScene` is
    /// no scene.
    pub max_items: usize,
    /// Performers kept in one scene; later ones are left out
    /// ([`Scene::performers_truncated`]).
    pub max_performers: usize,
    /// Elements in one array the parser reads past inside a scene; over
    /// it the scene is skipped, or the studio or performer entry it's in
    /// dropped.
    pub max_array: usize,
    /// Raw bytes of a string the parser reads; over it the scene is
    /// skipped, or the studio or performer entry it's in dropped.
    pub max_text: usize,
}

/// The caps for a real answer.
pub const LIMITS: Limits = Limits {
    max_bytes: 4 * 1024 * 1024,
    max_depth: 16,
    max_values: 200_000,
    max_items: 100,
    max_performers: 64,
    max_array: 1_000,
    // Room for a [`MAX_TITLE`] title written with `\u` escapes.
    max_text: 8 * 1024,
};

/// What a match is. Stash-box has scenes only: no movies, and VR is a
/// tag, not a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scene,
}

/// A release date as StashDB states it: a year, a month or a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// 1–9999.
    pub year: u16,
    /// 1–12.
    pub month: Option<u8>,
    /// A day of `month`; only with a month.
    pub day: Option<u8>,
}

/// The studio a scene is credited to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Studio {
    /// A UUID, lower case.
    pub id: String,
    /// Not empty, at most [`MAX_NAME`] bytes, hidden characters removed.
    pub name: String,
}

/// One scene StashDB matched, without its performers. Every field is a
/// claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub kind: Kind,
    /// A UUID, lower case.
    pub id: String,
    /// Not empty, at most [`MAX_TITLE`] bytes, hidden characters removed.
    pub title: Option<String>,
    pub studio: Option<Studio>,
    pub released: Option<Date>,
}

impl Match {
    /// [`SCENES`] and the id: the external reference.
    pub fn link(&self) -> String {
        format!("{SCENES}{}", self.id)
    }
}

/// A performer credited in a scene: a name, nothing else. No id, so
/// nothing links one person across scenes (plan Q4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Performer {
    /// Not empty, at most [`MAX_NAME`] bytes, hidden characters removed.
    pub name: String,
}

/// A kept scene: the match, and its performers apart from it, so a caller
/// that mustn't keep people (plan Q4) takes [`Self::matched`] only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub matched: Match,
    /// In StashDB's order, at most [`Limits::max_performers`].
    pub performers: Vec<Performer>,
    /// Some performers were left out: past [`Limits::max_performers`], or
    /// an entry that wasn't a performer with a usable name.
    pub performers_truncated: bool,
}

/// Scenes skipped, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No `id`, or a `null` one.
    pub no_id: u32,
    /// A string over [`Limits::max_text`] raw in the scene's own fields,
    /// or an array read past that's over [`Limits::max_array`].
    pub over_cap: u32,
    /// A kept field of the wrong type, an id that isn't a UUID, a date
    /// that isn't one, a bad escape, a repeated key, or a scene that isn't
    /// an object.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.no_id
            .saturating_add(self.over_cap)
            .saturating_add(self.malformed)
    }
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub kept: u32,
    pub skipped: Skipped,
    pub stop: Option<Stop>,
    /// Entries in the answer's GraphQL `errors` (1 if it isn't a list).
    /// Their messages are the server's text and aren't kept.
    pub errors: u32,
}

/// Every kept scene and the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub scenes: Vec<Scene>,
    pub page: Page,
}

/// Parses `body` with [`LIMITS`].
pub fn parse(body: &[u8]) -> Found {
    let mut scenes = Vec::new();
    let page = parse_each(body, &LIMITS, |s| scenes.push(s));
    Found { scenes, page }
}

/// Parses `body`, handing each kept scene to `emit` as soon as it ends.
pub fn parse_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Scene)) -> Page {
    let mut summary = common::Summary::default();
    let bytes = common::Limits {
        max_bytes: limits.max_bytes,
        ..common::LIMITS
    };
    let text = prepare(body, &bytes, &mut summary);
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
        page: Page {
            stop: summary.stop,
            ..Page::default()
        },
    };
    if let Err(stop) = p.document(&mut emit) {
        p.page.stop.get_or_insert(stop);
    }
    p.page
}

/// The scene being read.
#[derive(Debug, Default)]
struct Draft {
    bad: Option<Bad>,
    /// Keys read so far, one bit each ([`F_ID`] …).
    seen: u8,
    id: Option<String>,
    title: Option<String>,
    studio: Option<Studio>,
    released: Option<Date>,
    performers: Vec<Performer>,
    truncated: bool,
}

const F_ID: u8 = 1;
const F_TITLE: u8 = 2;
const F_DATE: u8 = 4;
const F_STUDIO: u8 = 8;
const F_PERFORMERS: u8 = 16;
const F_NAME: u8 = 32;
const F_PERFORMER: u8 = 64;

impl Draft {
    fn spoil(&mut self, bad: Bad) {
        self.bad.get_or_insert(bad);
    }

    /// The first time `key` is read in `seen`; a repeat spoils the scene,
    /// since which value a server meant is unclear.
    fn once(&mut self, seen: &mut u8, key: u8) -> bool {
        let first = *seen & key == 0;
        *seen |= key;
        if !first {
            self.spoil(Bad::Malformed);
        }
        first
    }

    /// The finished scene, or the counter it's skipped under.
    fn finish(self) -> Result<Scene, fn(&mut Skipped)> {
        match self.bad {
            Some(Bad::OverCap) => return Err(|s| s.over_cap = s.over_cap.saturating_add(1)),
            Some(Bad::Malformed) => return Err(|s| s.malformed = s.malformed.saturating_add(1)),
            None => {}
        }
        let Some(id) = self.id else {
            return Err(|s| s.no_id = s.no_id.saturating_add(1));
        };
        Ok(Scene {
            matched: Match {
                kind: Kind::Scene,
                id,
                title: self.title,
                studio: self.studio,
                released: self.released,
            },
            performers: self.performers,
            performers_truncated: self.truncated,
        })
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

impl Parser<'_, '_> {
    // --- Values the parser keeps -------------------------------------

    /// Skips the next value inside a scene; an over-long array spoils it.
    fn skip_in(&mut self, d: &mut Draft) -> R<()> {
        if self.j.skip()? {
            d.spoil(Bad::OverCap);
        }
        Ok(())
    }

    /// Skips a value of the wrong type for a kept field: the scene is
    /// malformed.
    fn wrong(&mut self, d: &mut Draft) -> R<()> {
        d.spoil(Bad::Malformed);
        self.skip_in(d)
    }

    /// A string, or `null` (`None`). Anything else spoils the scene, as
    /// does a string over [`Limits::max_text`] or with a bad escape.
    fn text(&mut self, d: &mut Draft) -> R<Option<String>> {
        match self.j.peek() {
            Some(b'"') => match self.j.text()? {
                Ok(s) => Ok(Some(s)),
                Err(e) => {
                    d.spoil(match e {
                        TextError::OverCap => Bad::OverCap,
                        TextError::Malformed => Bad::Malformed,
                    });
                    Ok(None)
                }
            },
            Some(b'n') => self.j.skip().map(|_| None),
            _ => self.wrong(d).map(|()| None),
        }
    }

    /// An id: a UUID string, or `null`. Any other string spoils the scene.
    fn id(&mut self, d: &mut Draft) -> R<Option<String>> {
        let s = self.text(d)?;
        let id = s.as_deref().and_then(uuid);
        if s.is_some() && id.is_none() {
            d.spoil(Bad::Malformed);
        }
        Ok(id)
    }

    /// A required name: a string with something left once trimmed and
    /// cleaned, at most [`MAX_NAME`] bytes. Anything else spoils `d`, which
    /// for a studio or performer is that entry's own draft.
    fn name(&mut self, d: &mut Draft) -> R<Option<String>> {
        let Some(s) = self.text(d)? else {
            return Ok(None);
        };
        let n = clean(&s);
        if n.len() > MAX_NAME {
            d.spoil(Bad::OverCap);
            return Ok(None);
        }
        if n.is_empty() {
            d.spoil(Bad::Malformed);
            return Ok(None);
        }
        Ok(Some(n))
    }

    // --- The shape ---------------------------------------------------

    fn document(&mut self, emit: &mut dyn FnMut(Scene)) -> R<()> {
        if self.j.peek() == Some(b'{') {
            let (mut data, mut errors) = (false, false);
            self.object(|p, k| match k {
                "data" if !data => {
                    data = true;
                    p.data(emit)
                }
                "errors" if !errors => {
                    errors = true;
                    p.errors()
                }
                _ => p.j.skip().map(drop),
            })?;
        } else {
            self.j.skip()?;
        }
        self.j.end()
    }

    /// GraphQL's `errors`: counted, never read.
    fn errors(&mut self) -> R<()> {
        match self.j.peek() {
            Some(b'[') => self.array(|p, _| {
                p.page.errors = p.page.errors.saturating_add(1);
                p.j.skip().map(drop)
            }),
            Some(b'n') => self.j.skip().map(drop),
            _ => {
                self.page.errors = self.page.errors.saturating_add(1);
                self.j.skip().map(drop)
            }
        }
    }

    /// `data`: the one answer to the one query. `null` when the query
    /// failed (the `errors` say so).
    fn data(&mut self, emit: &mut dyn FnMut(Scene)) -> R<()> {
        if self.j.peek() != Some(b'{') {
            return self.j.skip().map(drop);
        }
        let (mut search, mut find) = (false, false);
        self.object(|p, k| match k {
            "searchScene" if !search && p.j.peek() == Some(b'[') => {
                search = true;
                p.array(|p, _| p.scene(emit))
            }
            "findScene" if !find && p.j.peek() != Some(b'n') => {
                find = true;
                p.scene(emit)
            }
            _ => p.j.skip().map(drop),
        })
    }

    fn scene(&mut self, emit: &mut dyn FnMut(Scene)) -> R<()> {
        self.items += 1;
        if self.items > self.l.max_items {
            return Err(Stop::Items);
        }
        let mut d = Draft::default();
        if self.j.peek() == Some(b'{') {
            self.object(|p, k| p.field(&mut d, k))?;
        } else {
            self.wrong(&mut d)?;
        }
        match d.finish() {
            Ok(scene) => {
                self.page.kept = self.page.kept.saturating_add(1);
                emit(scene);
            }
            Err(skip) => skip(&mut self.page.skipped),
        }
        Ok(())
    }

    /// A field of a scene. Each may appear once.
    fn field(&mut self, d: &mut Draft, key: &str) -> R<()> {
        let mut seen = d.seen;
        let bit = match key {
            "id" => F_ID,
            "title" => F_TITLE,
            "release_date" => F_DATE,
            "studio" => F_STUDIO,
            "performers" => F_PERFORMERS,
            _ => return self.skip_in(d),
        };
        let first = d.once(&mut seen, bit);
        d.seen = seen;
        if !first {
            return self.skip_in(d);
        }
        match bit {
            F_ID => d.id = self.id(d)?,
            F_TITLE => {
                if let Some(t) = self.text(d)? {
                    // Cut to [`MAX_TITLE`] as the other feeds do.
                    let t = clean_title(t.trim()).trim().to_owned();
                    d.title = Some(t).filter(|t| !t.is_empty());
                }
            }
            F_DATE => {
                if let Some(s) = self.text(d)? {
                    d.released = date(&s);
                    if d.released.is_none() {
                        d.spoil(Bad::Malformed);
                    }
                }
            }
            F_STUDIO => d.studio = self.studio(d)?,
            _ => self.performers(d)?,
        }
        Ok(())
    }

    /// `studio`: `null`, or an object with an id and a name. Any other
    /// type spoils the scene; an object without a valid id and name is
    /// dropped and the scene kept.
    fn studio(&mut self, d: &mut Draft) -> R<Option<Studio>> {
        match self.j.peek() {
            Some(b'n') => self.j.skip().map(|_| None),
            Some(b'{') => {
                let mut e = Draft::default();
                let (mut id, mut name, mut seen) = (None, None, 0u8);
                self.object(|p, k| {
                    match k {
                        "id" if e.once(&mut seen, F_ID) => id = p.id(&mut e)?,
                        "name" if e.once(&mut seen, F_NAME) => name = p.name(&mut e)?,
                        _ => p.skip_in(&mut e)?,
                    }
                    Ok(())
                })?;
                let ok = e.bad.is_none();
                Ok(id
                    .zip(name)
                    .filter(|_| ok)
                    .map(|(id, name)| Studio { id, name }))
            }
            _ => self.wrong(d).map(|()| None),
        }
    }

    /// `performers`: `null`, or a list of `{"performer": {"name": …}}`.
    /// Any other type spoils the scene. An entry past
    /// [`Limits::max_performers`], or without one usable name, is left
    /// out and sets `truncated`; nothing is stored past the cap.
    fn performers(&mut self, d: &mut Draft) -> R<()> {
        match self.j.peek() {
            Some(b'n') => return self.j.skip().map(drop),
            Some(b'[') => {}
            _ => return self.wrong(d),
        }
        let max = self.l.max_performers;
        self.array(|p, n| {
            if n >= max {
                d.truncated = true;
                return p.j.skip().map(drop);
            }
            match p.performer()? {
                Some(f) => d.performers.push(f),
                None => d.truncated = true,
            }
            Ok(())
        })
    }

    /// One `performers` entry, or `None` if it isn't usable. Its faults
    /// are its own: they never spoil the scene.
    fn performer(&mut self) -> R<Option<Performer>> {
        let mut e = Draft::default();
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            return Ok(None);
        }
        let (mut name, mut seen) = (None, 0u8);
        self.object(|p, k| {
            if k != "performer" || !e.once(&mut seen, F_PERFORMER) {
                return p.skip_in(&mut e);
            }
            if p.j.peek() != Some(b'{') {
                return p.wrong(&mut e);
            }
            let mut inner = 0u8;
            p.object(|p, k| {
                match k {
                    "name" if e.once(&mut inner, F_NAME) => name = p.name(&mut e)?,
                    _ => p.skip_in(&mut e)?,
                }
                Ok(())
            })
        })?;
        Ok(name
            .filter(|_| e.bad.is_none())
            .map(|name| Performer { name }))
    }
}

/// `s` with hidden characters removed, then trimmed.
fn clean(s: &str) -> String {
    let c: String = s.chars().filter(|&c| !unsafe_char(c)).collect();
    c.trim().to_owned()
}

/// A UUID, `8-4-4-4-12` hex digits, in lower case.
fn uuid(s: &str) -> Option<String> {
    let ok = s.len() == ID_LEN
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    ok.then(|| s.to_ascii_lowercase())
}

/// StashDB's fuzzy dates: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
fn date(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if !s.is_ascii() || !matches!(s.len(), 4 | 7 | 10) || (s.len() > 4 && b[4] != b'-') {
        return None;
    }
    if s.len() == 10 && b[7] != b'-' {
        return None;
    }
    let year = small(s.get(..4)?, 4).filter(|y| *y >= 1)?;
    let month = match s.get(5..7) {
        Some(m) => Some(small(m, 2).filter(|m| (1..=12).contains(m))?),
        None => None,
    };
    let day = match (month, s.get(8..10)) {
        (Some(m), Some(d)) => {
            Some(small(d, 2).filter(|d| (1..=days_in_month(year, m)).contains(d))?)
        }
        _ => None,
    };
    Some(Date {
        year: u16::try_from(year).ok()?,
        month: month.and_then(|m| u8::try_from(m).ok()),
        day: day.and_then(|d| u8::try_from(d).ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        let id = "0A1B2C3D-0000-4000-8000-00000000000F";
        assert_eq!(uuid(id).as_deref(), Some(&*id.to_ascii_lowercase()));
        for bad in [
            "",
            "0a1b2c3d00000400080000000000000f",
            "0a1b2c3d-0000-4000-8000-00000000000",
            "0a1b2c3d-0000-4000-8000-00000000000g",
            "0a1b2c3d_0000-4000-8000-00000000000f",
            " 0a1b2c3d-0000-4000-8000-0000000000f",
        ] {
            assert_eq!(uuid(bad), None, "{bad:?}");
        }
        let d = |y, m, d| {
            Some(Date {
                year: y,
                month: m,
                day: d,
            })
        };
        assert_eq!(date("2024"), d(2024, None, None));
        assert_eq!(date("2024-02"), d(2024, Some(2), None));
        assert_eq!(date("2024-02-29"), d(2024, Some(2), Some(29)));
        for bad in [
            "",
            "0000",
            "2023-02-29",
            "2024-13",
            "2024-00",
            "2024-01-00",
            "2024-1-01",
            "24-01-01",
            "2024/01/01",
            "2024-01-01T00:00:00Z",
            "+024",
            "2024-01-",
            "2024-01-1x",
        ] {
            assert_eq!(date(bad), None, "{bad:?}");
        }
        assert_eq!(clean(" a\u{200b}b \u{202e}"), "ab");
    }

    /// The query documents select only what the claim stores: no
    /// personal field can be requested (plan Q5), and nothing past the
    /// fields this parser reads.
    #[test]
    fn queries_select_only_kept_fields() {
        const ALLOWED: [&str; 17] = [
            "query",
            "SearchScene",
            "FindScene",
            "term",
            "limit",
            "String",
            "Int",
            "ID",
            "searchScene",
            "findScene",
            "id",
            "title",
            "release_date",
            "studio",
            "name",
            "performers",
            "performer",
        ];
        for q in [SEARCH_SCENE, FIND_SCENE] {
            for word in q
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
            {
                assert!(ALLOWED.contains(&word), "{word} in {q}");
            }
            let open = q.matches('{').count();
            assert_eq!(open, q.matches('}').count(), "{q}");
            assert!(q.contains(
                "{ id title release_date studio { id name } performers { performer { name } } }"
            ));
            // No performer ids (plan Q4): `id` only at the scene and studio.
            let people = &q[q.find("performers").unwrap()..];
            assert!(!people.contains("id"), "{q}");
        }
    }
}
