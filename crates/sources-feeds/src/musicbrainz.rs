//! MusicBrainz web service answers (ENR-mb; architecture §18.2, §18.7;
//! `docs/decisions/2026-11-15-ENR-mb.md`): a release-group search
//! (`/ws/2/release-group?query=…&fmt=json`).
//!
//! The same pattern as the Wikidata parser: a pure function from bytes to
//! values, on the bounded [`json`](crate::json) reader, with no I/O and no
//! panics. Every cap is a named field of [`Limits`]. It reads only the
//! shape below, and anything else in a part it reads spoils that result,
//! counted in [`Skipped`]. [`Page::whole`] is the one rule a plugin
//! applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The shape: `{"created": …, "count": n, "offset": 0, "release-groups":
//! [{"id": mbid, "title": text, "first-release-date": "YYYY-MM-DD",
//! "primary-type": "Album", "artist-credit": [{"name": text,
//! "joinphrase": text, "artist": {"name": text, …}}, …], …}, …]}`. An
//! error, on any status, is `{"error": text, …}` ([`Page::error`]; the
//! service's text is never read).
//!
//! **What it keeps:** each release group's id, title, first release year
//! and primary type, and the names it's credited to. Never tags, ratings,
//! disambiguation comments, releases or the artists' ids, aliases or sort
//! names: those are passed over unread. An id is parsed strictly: a
//! lower-case UUID, the form MusicBrainz writes and the store accepts.
//!
//! Keys are compared as written. In a part that's read, a key with an
//! escape spoils it, and so does a repeat of a key it reads. Repeats are
//! found with a bitmask over the known keys, never a list of every key
//! seen, so a body of a million unknown keys is read in linear time.

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
    /// Elements in one array, read or passed over; over it the result is
    /// skipped (and over it in the result list, the parse stops).
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the result is
    /// skipped.
    pub max_text: usize,
    /// Results, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Artist credits read per result; the rest are passed over undecoded
    /// ([`Hit::truncated`]).
    pub max_credits: usize,
}

/// The caps for a search answer. A popular album group lists every one
/// of its releases (hundreds), which is most of the answer; the parser
/// passes them over.
pub const SEARCH_LIMITS: Limits = Limits {
    max_bytes: 4 * 1024 * 1024,
    max_depth: 16,
    max_values: 500_000,
    max_array: 20_000,
    max_text: 4 * 1024,
    max_items: 10,
    max_credits: 16,
};

/// A MusicBrainz id: a lower-case UUID, `8-4-4-4-12` hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mbid(String);

impl Mbid {
    /// Lower-case hex digits and `-` in the UUID's places; nothing else
    /// (no upper case, braces, `urn:` or spaces). The nil UUID isn't one.
    pub fn parse(s: &str) -> Option<Mbid> {
        let b = s.as_bytes();
        let ok = b.len() == 36
            && b.iter().enumerate().all(|(i, &c)| match i {
                8 | 13 | 18 | 23 => c == b'-',
                _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
            })
            && b.iter().any(|&c| c != b'0' && c != b'-');
        ok.then(|| Mbid(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Mbid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A release group's primary type. A type MusicBrainz adds later is read
/// as [`Primary::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primary {
    Album,
    Single,
    Ep,
    Broadcast,
    Other,
}

impl Primary {
    fn of(s: &str) -> Primary {
        match s {
            "Album" => Primary::Album,
            "Single" => Primary::Single,
            "EP" => Primary::Ep,
            "Broadcast" => Primary::Broadcast,
            _ => Primary::Other,
        }
    }
}

/// One release group. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: Mbid,
    /// Hidden characters removed, trimmed, at most [`MAX_TITLE`] bytes,
    /// not empty.
    pub title: String,
    /// The year of `first-release-date`, 1–9999, when it has one.
    pub year: Option<u16>,
    pub primary: Option<Primary>,
    /// The whole credit as printed (each credited name and its join
    /// phrase), cleaned like the title; `None` when there's no credit or
    /// it was cut at [`Limits::max_credits`].
    pub credit: Option<String>,
    /// Each credit's `name` and its artist's `name`, cleaned like the
    /// title, at most twice [`Limits::max_credits`].
    pub artists: Vec<String>,
    /// Credits past [`Limits::max_credits`] were passed over.
    pub truncated: bool,
}

/// Results skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't a lower-case UUID.
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
    /// The root has an `error` key.
    pub error: bool,
    /// The root's `release-groups` array was read.
    pub seen: bool,
    /// A root key read was written twice or with an escape.
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

/// A search answer's release groups, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub hits: Vec<Hit>,
    pub page: Page,
}

const ROOT: &str = "release-groups";

/// Parses a release-group search answer.
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
    let mut hits = Vec::new();
    let mut read_all = |p: &mut Parser<'_, '_>| -> R<()> {
        if p.j.peek() != Some(b'{') {
            p.j.skip()?;
        } else {
            let mut keys = Keys::default();
            p.object(|p, k| {
                // Unknown root keys are passed over; their repeats don't
                // matter.
                if keys.repeat(k, &[ROOT, "error"]) {
                    p.page.bad_root = true;
                    return p.j.skip().map(drop);
                }
                match k {
                    ROOT if p.j.peek() == Some(b'[') => {
                        p.page.seen = true;
                        let shape = p.arr(|p, _| {
                            hits.extend(p.result()?);
                            Ok(())
                        })?;
                        if shape == Shape::OverLong {
                            p.page.stop.get_or_insert(Stop::Items);
                        }
                        Ok(())
                    }
                    "error" => {
                        p.page.error = true;
                        p.j.skip().map(drop)
                    }
                    _ => p.j.skip().map(drop),
                }
            })?;
        }
        let end = p.j.end();
        p.page.trailing = end.is_err();
        end
    };
    if let Err(stop) = read_all(&mut p) {
        p.page.stop.get_or_insert(stop);
    }
    Search { hits, page: p.page }
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

/// The result being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
    }

    fn check(&mut self, shaped: bool) {
        if !shaped {
            self.set(Bad::Malformed);
        }
    }

    fn shape(&mut self, shape: Shape) {
        match shape {
            Shape::Fine => {}
            Shape::OverLong => self.set(Bad::OverCap),
            Shape::Wrong => self.set(Bad::Malformed),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Fine,
    /// An array with more than [`Limits::max_array`] elements.
    OverLong,
    /// Not the container asked for; read past.
    Wrong,
}

impl<'a> Parser<'a, '_> {
    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    fn key(s: &mut Spoil, keys: &mut Keys, k: &str, known: &[&str]) {
        if keys.repeat(k, known) {
            s.set(Bad::Malformed);
        }
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

    /// A string or `null` (MusicBrainz writes `null` for an unset field).
    fn opt_string(&mut self, s: &mut Spoil) -> R<Option<String>> {
        if self.j.peek() == Some(b'n') {
            return self.skip_in(s).map(|()| None);
        }
        self.string(s)
    }

    fn obj(&mut self, f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<Shape> {
        if self.j.peek() != Some(b'{') {
            return self.j.skip().map(|_| Shape::Wrong);
        }
        self.object(f).map(|()| Shape::Fine)
    }

    /// An array that must be next, handing `f` its first
    /// [`Limits::max_array`] elements and passing over the rest.
    fn arr(&mut self, mut f: impl FnMut(&mut Self, usize) -> R<()>) -> R<Shape> {
        if self.j.peek() != Some(b'[') {
            return self.j.skip().map(|_| Shape::Wrong);
        }
        let (max, mut over) = (self.l.max_array, false);
        self.array(|p, n| {
            if n >= max {
                over = true;
                return p.j.skip().map(drop);
            }
            f(p, n)
        })?;
        Ok(if over { Shape::OverLong } else { Shape::Fine })
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

    /// One release group.
    fn result(&mut self) -> R<Option<Hit>> {
        self.items += 1;
        if self.items > self.l.max_items {
            self.page.stop.get_or_insert(Stop::Items);
            return self.j.skip().map(|_| None);
        }
        const KNOWN: &[&str] = &[
            "id",
            "title",
            "first-release-date",
            "primary-type",
            "artist-credit",
        ];
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let (mut id, mut name, mut date, mut primary) = (None, None, None, None);
        let (mut credit, mut artists, mut truncated) = (String::new(), Vec::new(), false);
        let shape = self.obj(|p, k| {
            Self::key(&mut s, &mut keys, k, KNOWN);
            match k {
                "id" => id = p.string(&mut s)?,
                "title" => name = p.string(&mut s)?,
                "first-release-date" => date = p.opt_string(&mut s)?,
                "primary-type" => primary = p.opt_string(&mut s)?,
                "artist-credit" => {
                    let shape = p.arr(|p, n| {
                        if n >= p.l.max_credits {
                            truncated = true;
                            return p.skip_in(&mut s);
                        }
                        p.credit(&mut s, &mut credit, &mut artists)
                    })?;
                    s.shape(shape);
                }
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        s.shape(shape);
        let year = match date.as_deref().map(year_of) {
            None => None,
            Some(Some(y)) => y,
            Some(None) => {
                s.check(false);
                None
            }
        };
        let name = name.as_deref().and_then(title);
        s.check(name.is_some());
        if !self.tally(&s) {
            return Ok(None);
        }
        let (Some(id), Some(name)) = (id.as_deref().and_then(Mbid::parse), name) else {
            self.page.skipped.bad_id = self.page.skipped.bad_id.saturating_add(1);
            return Ok(None);
        };
        self.page.kept = self.page.kept.saturating_add(1);
        Ok(Some(Hit {
            id,
            title: name,
            year,
            primary: primary.as_deref().map(Primary::of),
            credit: if truncated { None } else { title(&credit) },
            artists,
            truncated,
        }))
    }

    /// One artist credit: `{"name": text, "joinphrase": text, "artist":
    /// {"name": text, …}}`. Its name and join phrase go on the printed
    /// credit; its name and the artist's name are each kept.
    fn credit(&mut self, s: &mut Spoil, credit: &mut String, artists: &mut Vec<String>) -> R<()> {
        let (mut keys, mut name, mut join, mut artist) = (Keys::default(), None, None, None);
        let shape = self.obj(|p, k| {
            Self::key(s, &mut keys, k, &["name", "joinphrase", "artist"]);
            match k {
                "name" => name = p.string(s)?,
                "joinphrase" => join = p.opt_string(s)?,
                "artist" => {
                    let mut own = Keys::default();
                    let shape = p.obj(|p, k| {
                        Self::key(s, &mut own, k, &["name"]);
                        match k {
                            "name" => artist = p.string(s)?,
                            _ => p.skip_in(s)?,
                        }
                        Ok(())
                    })?;
                    s.shape(shape);
                }
                _ => p.skip_in(s)?,
            }
            Ok(())
        })?;
        s.shape(shape);
        let Some(name) = name else {
            s.check(false);
            return Ok(());
        };
        // The printed credit is bounded by the caps: `max_credits` names
        // and join phrases of at most `max_text` bytes each.
        credit.push_str(&name);
        credit.push_str(join.as_deref().unwrap_or(""));
        for n in [Some(name), artist].into_iter().flatten() {
            if let Some(t) = title(&n)
                && !artists.contains(&t)
            {
                artists.push(t);
            }
        }
        Ok(())
    }
}

/// The year of a MusicBrainz date, `YYYY`, `YYYY-MM` or `YYYY-MM-DD`:
/// `Some(None)` for an empty date or year 0; `None` for any other form.
fn year_of(d: &str) -> Option<Option<u16>> {
    if d.is_empty() {
        return Some(None);
    }
    let b = d.as_bytes();
    let form = match b.len() {
        4 => &b"0000"[..],
        7 => b"0000-00",
        10 => b"0000-00-00",
        _ => return None,
    };
    let shaped = b.iter().zip(form).all(|(&c, &f)| {
        if f == b'0' {
            c.is_ascii_digit()
        } else {
            c == f
        }
    });
    if !shaped {
        return None;
    }
    let y: u16 = d.get(..4)?.parse().ok()?;
    Some((y > 0).then_some(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_parsers() {
        let id = "b1392450-e666-3926-a536-22c65f834433";
        assert_eq!(Mbid::parse(id).map(|m| m.to_string()).as_deref(), Some(id));
        for bad in [
            "",
            "B1392450-e666-3926-a536-22c65f834433",
            "b1392450e6663926a53622c65f834433",
            "{b1392450-e666-3926-a536-22c65f834433}",
            "b1392450-e666-3926-a536-22c65f83443",
            "b1392450-e666-3926-a536-22c65f8344333",
            "b1392450-e666-3926-a536_22c65f834433",
            "g1392450-e666-3926-a536-22c65f834433",
            " b1392450-e666-3926-a536-22c65f834433",
            concat!("00000000-0000-0000-0000-", "000000", "000000"),
        ] {
            assert_eq!(Mbid::parse(bad), None, "{bad}");
        }
        assert_eq!(year_of("1997-05-21"), Some(Some(1997)));
        assert_eq!(year_of("1997-05"), Some(Some(1997)));
        assert_eq!(year_of("1997"), Some(Some(1997)));
        assert_eq!(year_of(""), Some(None));
        assert_eq!(year_of("0000"), Some(None));
        for bad in [
            "97",
            "1997-5",
            "1997/05/21",
            "+1997",
            "1997-05-21T00",
            "１９９７",
            "-997",
        ] {
            assert_eq!(year_of(bad), None, "{bad}");
        }
        assert_eq!(Primary::of("EP"), Primary::Ep);
        assert_eq!(Primary::of("Audio drama"), Primary::Other);
    }
}
