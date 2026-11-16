//! IGDB answers (ENR-igdb; architecture §18.2, §18.9;
//! `docs/decisions/2026-11-16-ENR-igdb.md`): `POST /v4/games` with a
//! search, and Twitch's `POST /oauth2/token` (client credentials) that
//! IGDB's access token comes from.
//!
//! TMDB's shape (ENR-tmdb): pure functions from bytes to values, on the
//! bounded [`json`](crate::json) reader, with no I/O and no panics. Every
//! cap is a named field of [`Limits`]. [`Page::whole`] is the one rule a
//! plugin applies: an answer is used only when it was read to its end with
//! nothing skipped, so a broken answer is never mistaken for a miss.
//!
//! The games answer is an array of `{"id": 1942, "name": text,
//! "first_release_date": unix seconds, "themes": [id, …], "genres":
//! [id, …]}`, with only the fields asked for (and absent ones left out).
//! An error is an object (`{"message": text}`) or an array of objects
//! without an `id` (`[{"title": text, "status": 400}]`): neither is whole.
//! The service's text is never read.
//!
//! Keys are compared as written. In a result, a key with an escape, a
//! repeat of a key it reads, or a read field of the wrong type spoils it.

use std::fmt;

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
    /// spoiled.
    pub max_array: usize,
    /// Raw bytes of a string the parser decodes; over it the result is
    /// spoiled.
    pub max_text: usize,
    /// Results, kept or not ([`Stop::Items`]).
    pub max_items: usize,
    /// Theme ids and genre ids kept per result, each; the rest are passed
    /// over ([`Hit::truncated`]).
    pub max_ids: usize,
}

/// The caps for a games answer: the request's `limit 20`.
pub const LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    max_depth: 8,
    max_values: 50_000,
    max_array: 1_000,
    max_text: 4 * 1024,
    max_items: 20,
    max_ids: 32,
};

/// IGDB's theme id for "Erotic".
pub const THEME_EROTIC: u32 = 42;

/// The result's keys read, in a fixed order (for repeat detection).
const KEYS: &[&str; 5] = &["id", "name", "first_release_date", "themes", "genres"];

/// One game. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// IGDB's id: 1 to `u64::MAX`.
    pub id: u64,
    /// `name`: hidden characters removed, trimmed, at most [`MAX_TITLE`]
    /// bytes, not empty.
    pub title: Option<String>,
    /// The UTC year of `first_release_date`, 1–9999.
    pub year: Option<u16>,
    /// Theme ids, at most [`Limits::max_ids`], plus [`THEME_EROTIC`] when
    /// it's past the cap, so the adult check never depends on the cap.
    /// `None` when the result has no `themes` key (IGDB leaves out empty
    /// lists).
    pub themes: Option<Vec<u32>>,
    /// Genre ids, at most [`Limits::max_ids`].
    pub genres: Vec<u32>,
    /// Ids past the cap were passed over.
    pub truncated: bool,
}

impl Hit {
    /// IGDB's own adult marker: `Some(true)` when the game has the Erotic
    /// theme. IGDB has no "not adult" marker, so otherwise `None`.
    pub fn adult(&self) -> Option<bool> {
        self.themes
            .as_ref()
            .is_some_and(|t| t.contains(&THEME_EROTIC))
            .then_some(true)
    }
}

/// Results skipped, by reason. Any of them makes the answer not
/// [`whole`](Page::whole).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No id, or one that isn't a positive integer.
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
    /// The root was an array.
    pub seen: bool,
    /// Something other than whitespace followed the root value.
    pub trailing: bool,
}

impl Page {
    /// Read to its end, an array, nothing skipped. Only such an answer may
    /// be used, as a match or as a miss.
    pub fn whole(&self) -> bool {
        self.stop.is_none() && !self.trailing && self.seen && self.skipped.total() == 0
    }
}

/// A games answer's results, in the service's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    pub hits: Vec<Hit>,
    pub page: Page,
}

fn reader<'a>(body: &'a [u8], limits: &Limits, stop: &mut Option<Stop>) -> Reader<'a> {
    let mut summary = crate::common::Summary::default();
    let bytes = crate::common::Limits {
        max_bytes: limits.max_bytes,
        ..crate::common::LIMITS
    };
    let text = prepare(body, &bytes, &mut summary);
    *stop = summary.stop;
    Reader::new(
        text,
        crate::json::Limits {
            max_depth: limits.max_depth,
            max_values: limits.max_values,
            max_array: limits.max_array,
            max_text: limits.max_text,
        },
    )
}

/// Parses a `/v4/games` answer.
pub fn parse_games(body: &[u8], limits: &Limits) -> Search {
    let mut stop = None;
    let j = reader(body, limits, &mut stop);
    let mut p = Parser {
        j,
        l: limits,
        items: 0,
        hits: Vec::new(),
        page: Page {
            stop,
            ..Page::default()
        },
    };
    if let Err(stop) = p.root() {
        p.page.stop.get_or_insert(stop);
    }
    p.page.kept = u32::try_from(p.hits.len()).unwrap_or(u32::MAX);
    Search {
        hits: p.hits,
        page: p.page,
    }
}

/// Bytes in an access token kept. Twitch's are 30.
pub const MAX_TOKEN: usize = 512;
/// Twitch's app access tokens last about 60 days; a longer life is cut
/// to this.
pub const MAX_EXPIRES_IN: u64 = 90 * 86_400;

/// Caps for a token answer: a small object.
pub const TOKEN_LIMITS: Limits = Limits {
    max_bytes: 8 * 1024,
    max_depth: 4,
    max_values: 64,
    max_array: 16,
    max_text: 1024,
    max_items: 0,
    max_ids: 0,
};

/// A Twitch app access token answer. Its `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    /// 1–[`MAX_TOKEN`] bytes of ASCII letters, digits, `-`, `_` and `.`:
    /// safe in a header.
    pub access_token: String,
    /// Seconds it lasts: 1–[`MAX_EXPIRES_IN`].
    pub expires_in: u64,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("access_token", &"[redacted]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// A token is header-safe: ASCII letters, digits and `-`, `_`, `.`.
pub fn token_ok(t: &str) -> bool {
    (1..=MAX_TOKEN).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Parses Twitch's `{"access_token": text, "expires_in": n, "token_type":
/// "bearer"}`. `None` for anything else: not whole, a repeated or escaped
/// key, a token that isn't [`token_ok`], no or a zero `expires_in`, or a
/// `token_type` other than `bearer`. Longer lives are cut to
/// [`MAX_EXPIRES_IN`].
pub fn parse_token(body: &[u8], limits: &Limits) -> Option<Token> {
    let mut stop = None;
    let mut j = reader(body, limits, &mut stop);
    if stop.is_some() || j.peek() != Some(b'{') {
        return None;
    }
    let mut t = TokenParse {
        j,
        token: None,
        expires: None,
        bad: false,
        seen: 0,
    };
    t.root().ok()?;
    if t.bad {
        return None;
    }
    let access_token = t.token.filter(|s| token_ok(s))?;
    let expires_in = t.expires.filter(|&n| n > 0)?.min(MAX_EXPIRES_IN);
    Some(Token {
        access_token,
        expires_in,
    })
}

struct TokenParse<'a> {
    j: Reader<'a>,
    token: Option<String>,
    expires: Option<u64>,
    bad: bool,
    seen: u8,
}

impl<'a> Walk<'a> for TokenParse<'a> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

impl TokenParse<'_> {
    fn root(&mut self) -> R<()> {
        self.object(|p, k| {
            const KNOWN: &[&str] = &["access_token", "expires_in", "token_type"];
            if k.contains('\\') {
                p.bad = true;
                return p.j.skip().map(drop);
            }
            let Some(i) = KNOWN.iter().position(|w| *w == k) else {
                return p.j.skip().map(drop);
            };
            let bit = 1u8 << i;
            if p.seen & bit != 0 {
                p.bad = true;
                return p.j.skip().map(drop);
            }
            p.seen |= bit;
            match i {
                0 | 2 if p.j.peek() == Some(b'"') => match p.j.text()? {
                    Ok(s) if i == 0 => p.token = Some(s),
                    Ok(s) if s.eq_ignore_ascii_case("bearer") => {}
                    _ => p.bad = true,
                },
                1 if matches!(p.j.peek(), Some(b'0'..=b'9')) => {
                    let raw = p.j.num()?;
                    p.expires = raw
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| raw.parse::<u64>().ok())
                        .flatten();
                    if p.expires.is_none() {
                        p.bad = true;
                    }
                }
                _ => {
                    p.bad = true;
                    p.j.skip()?;
                }
            }
            Ok(())
        })?;
        self.j.end()
    }
}

/// The keys of one object a parser reads, as a bitmask over their place
/// in a short list.
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

/// The result being read: its first fault.
#[derive(Debug, Default)]
struct Spoil(Option<Bad>);

impl Spoil {
    fn set(&mut self, bad: Bad) {
        self.0.get_or_insert(bad);
    }
}

struct Parser<'a, 'l> {
    j: Reader<'a>,
    l: &'l Limits,
    items: usize,
    hits: Vec<Hit>,
    page: Page,
}

impl<'a> Walk<'a> for Parser<'a, '_> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

impl<'a> Parser<'a, '_> {
    fn root(&mut self) -> R<()> {
        if self.j.peek() == Some(b'[') {
            self.page.seen = true;
            let max = self.l.max_array;
            self.array(|p, n| {
                if n >= max {
                    p.page.stop.get_or_insert(Stop::Items);
                    return p.j.skip().map(drop);
                }
                p.result()
            })?;
        } else {
            self.j.skip()?;
        }
        let end = self.j.end();
        self.page.trailing = end.is_err();
        end
    }

    fn skip_in(&mut self, s: &mut Spoil) -> R<()> {
        if self.j.skip()? {
            s.set(Bad::OverCap);
        }
        Ok(())
    }

    /// A non-negative integer that fits a `u64`: digits only.
    fn integer(&mut self, s: &mut Spoil) -> R<Option<u64>> {
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let raw = self.j.num()?;
        let n = raw
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| raw.parse::<u64>().ok())
            .flatten();
        if n.is_none() {
            s.set(Bad::Malformed);
        }
        Ok(n)
    }

    /// Unix seconds, signed (IGDB dates games before 1970 too): an
    /// optional `-` and digits that fit an `i64`.
    fn seconds(&mut self, s: &mut Spoil) -> R<Option<i64>> {
        if !matches!(self.j.peek(), Some(b'-' | b'0'..=b'9')) {
            s.set(Bad::Malformed);
            return self.skip_in(s).map(|()| None);
        }
        let raw = self.j.num()?;
        let digits = raw.strip_prefix('-').unwrap_or(raw);
        let n = (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| raw.parse::<i64>().ok())
            .flatten();
        if n.is_none() {
            s.set(Bad::Malformed);
        }
        Ok(n)
    }

    /// One result: the fields [`KEYS`] names; everything else is passed
    /// over undecoded.
    fn result(&mut self) -> R<()> {
        self.items += 1;
        if self.items > self.l.max_items {
            self.page.stop.get_or_insert(Stop::Items);
            return self.j.skip().map(drop);
        }
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            self.page.skipped.malformed = self.page.skipped.malformed.saturating_add(1);
            return Ok(());
        }
        let (mut s, mut keys) = (Spoil::default(), Keys::default());
        let mut id: Option<u64> = None;
        let mut hit = Hit {
            id: 0,
            title: None,
            year: None,
            themes: None,
            genres: Vec::new(),
            truncated: false,
        };
        self.object(|p, k| {
            if keys.repeat(k, KEYS) {
                s.set(Bad::Malformed);
                return p.skip_in(&mut s);
            }
            match KEYS.iter().position(|w| *w == k) {
                Some(0) => id = p.integer(&mut s)?,
                Some(1) => {
                    if p.j.peek() != Some(b'"') {
                        s.set(Bad::Malformed);
                        return p.skip_in(&mut s);
                    }
                    match p.j.text()? {
                        Ok(t) => hit.title = title(&t),
                        Err(TextError::OverCap) => s.set(Bad::OverCap),
                        Err(TextError::Malformed) => s.set(Bad::Malformed),
                    }
                }
                Some(2) => {
                    if let Some(secs) = p.seconds(&mut s)? {
                        match year_of(secs) {
                            Some(y) => hit.year = Some(y),
                            None => s.set(Bad::Malformed),
                        }
                    }
                }
                Some(3) => {
                    let mut themes = Vec::new();
                    p.ids(&mut s, &mut themes, &mut hit.truncated, Some(THEME_EROTIC))?;
                    hit.themes = Some(themes);
                }
                Some(4) => {
                    let mut genres = Vec::new();
                    p.ids(&mut s, &mut genres, &mut hit.truncated, None)?;
                    hit.genres = genres;
                }
                _ => p.skip_in(&mut s)?,
            }
            Ok(())
        })?;
        let k = &mut self.page.skipped;
        match (s.0, id) {
            (Some(Bad::OverCap), _) => k.over_cap = k.over_cap.saturating_add(1),
            (Some(Bad::Malformed), _) => k.malformed = k.malformed.saturating_add(1),
            (None, None | Some(0)) => k.bad_id = k.bad_id.saturating_add(1),
            (None, Some(n)) => {
                hit.id = n;
                self.hits.push(hit);
            }
        }
        Ok(())
    }

    /// An array of integers that fit a `u32`, the first
    /// [`Limits::max_ids`] kept, and `always` kept wherever it is (once).
    fn ids(
        &mut self,
        s: &mut Spoil,
        out: &mut Vec<u32>,
        truncated: &mut bool,
        always: Option<u32>,
    ) -> R<()> {
        if self.j.peek() != Some(b'[') {
            s.set(Bad::Malformed);
            return self.skip_in(s);
        }
        let (max_array, max_ids) = (self.l.max_array, self.l.max_ids);
        self.array(|p, n| {
            if n >= max_array {
                s.set(Bad::OverCap);
                return p.j.skip().map(drop);
            }
            let g = p.integer(s)?;
            match g.map(u32::try_from) {
                Some(Ok(g)) if out.len() < max_ids => out.push(g),
                Some(Ok(g)) => {
                    *truncated = true;
                    if always == Some(g) && !out.contains(&g) {
                        out.push(g);
                    }
                }
                Some(Err(_)) => s.set(Bad::Malformed),
                None => {}
            }
            Ok(())
        })
    }
}

/// The UTC year of `secs` past 1970-01-01, when it's 1–9999.
pub fn year_of(secs: i64) -> Option<u16> {
    // Days to a civil date (H. Hinnant's algorithm), year only.
    let days = secs.div_euclid(86_400);
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month_from_march = mp < 10;
    let y = yoe.checked_add(era.checked_mul(400)?)? + i64::from(!month_from_march);
    u16::try_from(y).ok().filter(|y| (1..=9999).contains(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn years_are_utc() {
        assert_eq!(year_of(0), Some(1970));
        assert_eq!(year_of(-1), Some(1969));
        // 2015-05-19.
        assert_eq!(year_of(1_431_993_600), Some(2015));
        // 2000-12-31T23:59:59Z and the second after.
        assert_eq!(year_of(978_307_199), Some(2000));
        assert_eq!(year_of(978_307_200), Some(2001));
        // 1962-01-01.
        assert_eq!(year_of(-252_460_800), Some(1962));
        assert_eq!(year_of(i64::MAX), None);
        assert_eq!(year_of(i64::MIN), None);
        assert_eq!(year_of(-62_135_596_801), None, "year 0");
    }

    #[test]
    fn tokens_are_header_safe() {
        assert!(token_ok("abc.DEF-123_x"));
        for bad in ["", "a b", "a\r\nX: y", "a:b", "é"] {
            assert!(!token_ok(bad), "{bad:?}");
        }
        assert!(!token_ok(&"a".repeat(MAX_TOKEN + 1)));
    }

    #[test]
    fn token_debug_is_redacted() {
        let t = parse_token(
            br#"{"access_token":"faketoken0000","expires_in":5000,"token_type":"bearer"}"#,
            &TOKEN_LIMITS,
        )
        .unwrap();
        assert_eq!(t.expires_in, 5000);
        assert!(!format!("{t:?}").contains("faketoken0000"));
    }
}
