//! Discogs, an enrichment plugin for music (ENR-discogs; architecture
//! §18.1, §18.2, §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-discogs.md`). Wikidata's shape (ENR-14);
//! the title split and the matching rules are MusicBrainz's (ENR-mb), so
//! the two music plugins agree.
//!
//! **Off by default, Account class.** Discogs' database search needs your
//! own personal access token, stored in the credential store (SRC-4) and
//! read on each lookup, never kept, logged or put in a URL. ENR-10's
//! runtime (`lodestar_pipeline::identify`) decides whether an item may be
//! looked up at all (never a `PRIVACY`, held, hidden or Adult item), paces
//! the lookups, caches the answers and sends them through the writer's
//! judge before anything is stored. This plugin only answers one [`Query`]
//! of kind Music, with **one request**:
//!
//! 1. the query's title is split into an artist and an album at its first
//!    ` - ` (the release-name parser writes music titles so), or is the
//!    album alone ([`wanted`]);
//! 2. `GET /database/search?type=master` with the album and the artist as
//!    their own encoded URL parameters (Discogs' search has plain fields,
//!    no query language), for at most [`SEARCH_LIMIT`] masters, the token
//!    in an `Authorization: Discogs token=…` header;
//! 3. the one master whose album, artist and year agree with the query
//!    ([`choose`]). None, or more than one, is no match: it never guesses.
//!
//! Every request carries a [`USER_AGENT`] naming the project and its URL,
//! never the user or the install, as Discogs asks. Errors read the HTTP
//! status, the rate-limit header and the parser's report only: the
//! service's text is never kept or put in an error.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::ExternalId;
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::discogs::{self, Hit, LIMITS};
use tokio::time::Instant;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "discogs";
/// The credential that holds the personal access token.
pub const TOKEN: &str = "personal_access_token";
/// The only host the plugin may reach.
pub const HOST: &str = "api.discogs.com";
/// The search's path on it.
pub const PATH: &str = "/database/search";
/// Masters asked for in a search. The match must be among the top few.
pub const SEARCH_LIMIT: usize = 5;
/// Bytes in a token. Discogs' personal access tokens are 40 letters and
/// digits.
pub const MAX_TOKEN: usize = 256;
/// Discogs asks for a User-Agent that names the application, its version
/// and a way to reach it. The project's own URL, the same for every
/// install: nothing about the user, their host or their account. The same
/// form as MusicBrainz's.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " ( ",
    env!("CARGO_PKG_REPOSITORY"),
    " )"
);

/// What a lookup sends, for its Sources row (UI review 02 §5.5).
pub const SENDS: &str = "The parsed album title and, when the name has one, its artist, \
     with your Discogs personal access token, to api.discogs.com. Never the info hash, the \
     torrent's name, its year or its files.";
/// Discogs' API terms ask applications to credit the data so, linked to
/// Discogs, wherever it's shown (`docs/decisions/2026-11-15-ENR-discogs.md`).
pub const ATTRIBUTION: &str = "Data provided by Discogs.";
/// Where [`ATTRIBUTION`] links.
pub const ATTRIBUTION_URL: &str = "https://www.discogs.com/";

/// How sure a match is when the artist and the year agree.
pub const CONFIDENCE_ARTIST_YEAR: u8 = 90;
/// The artist agrees and the query has no year.
pub const CONFIDENCE_ARTIST: u8 = 75;
/// The query names no artist; the year agrees.
pub const CONFIDENCE_YEAR: u8 = 70;
/// The query names no artist and has no year.
pub const CONFIDENCE_TITLE: u8 = 60;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Discogs",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Low,
    consent: Consent::Account,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// No settings of its own: the token is the declared credential, never a
/// setting.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// The least time between two requests: the plan's cap for Discogs is at
/// most 25 a minute (§2.26), under Discogs' 60 a minute for an
/// authenticated client, with no burst.
pub const MIN_INTERVAL: Duration = Duration::from_millis(2400);

/// Back-off after a 401, 403, 429 or 5xx: a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::every(MIN_INTERVAL),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    credentials: &[CredentialSpec {
        name: TOKEN,
        label: "Personal access token",
        kind: CredentialKind::Secret,
        required: true,
    }],
    ..Policy::STRICT
};

/// Discogs' rate-limit window: requests are counted over a moving minute.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The token was refused (401 or 403). Names no token.
pub const REFUSED: &str = "Discogs refused the personal access token";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "Discogs is limiting requests; backing off";
/// The host is held after a refusal, a rate limit or a server error, or
/// the plugin after Discogs said no requests remain this minute.
pub const HELD: &str = "Discogs requests are on hold; backing off";
/// No token is stored.
pub const NO_TOKEN: &str = "needs setup: no Discogs personal access token is stored";
const BAD_TOKEN: &str = "the stored Discogs personal access token isn't a valid token";
const UNREADABLE_TOKEN: &str = "the stored Discogs personal access token couldn't be read";
const BAD_ANSWER: &str = "Discogs' answer wasn't understood";
const FAILED: &str = "Discogs' request failed";
const BAD_URL: &str = "Discogs' address didn't build";

/// The Discogs plugin. It holds no token, reading it on each lookup; its
/// one state is the wait after Discogs said none of the minute's requests
/// remain.
pub struct Discogs {
    credentials: Arc<dyn Credentials>,
    not_before: Mutex<Option<Instant>>,
}

impl fmt::Debug for Discogs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Discogs").finish_non_exhaustive()
    }
}

impl Discogs {
    pub fn new(credentials: Arc<dyn Credentials>) -> Discogs {
        Discogs {
            credentials,
            not_before: Mutex::new(None),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs, given the names of the
    /// credentials stored for it: the token (Account class).
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need_with(&MANIFEST, POLICY.credentials, None, stored)
    }
}

/// What a music title names: the album and, when the title is
/// `Artist - Album`, the artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted<'a> {
    pub artist: Option<&'a str>,
    pub album: &'a str,
}

/// Splits a title at its first ` - ` into artist and album, when both
/// sides have a letter or digit; otherwise the whole title is the album.
/// `None` when nothing in it could name an album. MusicBrainz's rule.
pub fn wanted(title: &str) -> Option<Wanted<'_>> {
    if let Some((artist, album)) = title.split_once(" - ")
        && !fold(artist).is_empty()
        && !fold(album).is_empty()
    {
        return Some(Wanted {
            artist: Some(artist.trim()),
            album: album.trim(),
        });
    }
    (!fold(title).is_empty()).then(|| Wanted {
        artist: None,
        album: title.trim(),
    })
}

impl Identifier for Discogs {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Music]
    }

    fn min_interval(&self) -> Duration {
        MIN_INTERVAL
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            if query.kind() != MediaKind::Music {
                return Ok(Lookup::NoMatch);
            }
            // A title of nothing but punctuation can name no album:
            // nothing is sent, and no token read.
            let Some(want) = wanted(query.title()) else {
                return Ok(Lookup::NoMatch);
            };
            self.not_held()?;
            // Read on each lookup, so a token removed on purge stops
            // working at once and no copy outlives the call.
            let token = self
                .credentials
                .get(ID, TOKEN)
                .await
                .map_err(|_| fail(UNREADABLE_TOKEN))?
                .ok_or_else(|| fail(NO_TOKEN))?;
            if !valid_token(token.expose()) {
                return Err(fail(BAD_TOKEN));
            }
            let request = search_request(&want, &token)?;
            drop(token);
            let res = answer(kit.http.send(&request).await)?;
            self.note_remaining(&res);
            let found = discogs::parse_search(&res.body, &LIMITS);
            if !found.page.whole() {
                return Err(fail(BAD_ANSWER));
            }
            Ok(choose(query, &found.hits))
        })
    }
}

impl Discogs {
    /// Fails at once, sending nothing, while the minute's wait lasts.
    fn not_held(&self) -> Result<(), PluginError> {
        let until = *self.not_before.lock().unwrap_or_else(|e| e.into_inner());
        match until {
            Some(t) if Instant::now() < t => Err(fail(HELD)),
            _ => Ok(()),
        }
    }

    /// Discogs counts each client's requests over a moving minute and
    /// says how many remain (`X-Discogs-Ratelimit-Remaining`). At none
    /// left, the plugin waits out the [`WINDOW`] before it sends again,
    /// rather than earn a 429.
    fn note_remaining(&self, res: &Response) {
        let none_left = res
            .header("x-discogs-ratelimit-remaining")
            .is_some_and(|v| v.trim() == "0");
        if none_left {
            *self.not_before.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + WINDOW);
        }
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// A token goes into a header, so only letters and digits, which is all
/// a Discogs token has.
fn valid_token(t: &str) -> bool {
    (1..=MAX_TOKEN).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The search for `want`: the album and, when the title names one, the
/// artist are the only item data in it, each one encoded parameter. The
/// year and the kind aren't sent.
pub fn search_request(want: &Wanted<'_>, token: &Secret) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}{PATH}")).map_err(|_| fail(BAD_URL))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("type", "master")
            .append_pair("release_title", want.album);
        if let Some(a) = want.artist {
            q.append_pair("artist", a);
        }
        q.append_pair("per_page", &SEARCH_LIMIT.to_string())
            .append_pair("page", "1");
    }
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("Authorization", format!("Discogs token={}", token.expose())))
}

/// A 200, or the error its status means.
fn answer(res: Result<Response, HttpError>) -> Result<Response, PluginError> {
    let res = match res {
        Ok(r) => r,
        // The limiter holds the host after a 401, 403, 429 or 5xx; the
        // retry waits.
        Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
        Err(_) => return Err(fail(FAILED)),
    };
    match res.status {
        200 => Ok(res),
        401 | 403 => Err(fail(REFUSED)),
        429 => Err(fail(RATE_LIMITED)),
        _ => Err(fail(FAILED)),
    }
}

/// An artist as Discogs prints it, without its marks: a trailing `*`
/// (a name variation) and a trailing ` (n)` (Discogs' number telling two
/// artists of one name apart).
pub fn artist_name(s: &str) -> &str {
    let s = s.trim().trim_end_matches('*').trim_end();
    let s = s
        .strip_suffix(')')
        .and_then(|r| r.rsplit_once(" ("))
        .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(s, |(name, _)| name);
    s.trim_end().trim_end_matches('*').trim_end()
}

/// The album `h` names, when it agrees with `album` and `artist` (both
/// folded): Discogs prints a master's title as `Artist - Album`, so each
/// ` - ` in it is tried as the split, the artist side read by
/// [`artist_name`]. A title with no ` - ` is the album alone, and agrees
/// only when the query names no artist.
fn agrees<'h>(h: &'h Hit, album: &str, artist: Option<&str>) -> Option<&'h str> {
    let t = h.title.as_str();
    let mut at = 0;
    while let Some(i) = t[at..].find(" - ") {
        let (left, right) = (&t[..at + i], &t[at + i + 3..]);
        if fold(right) == album && artist.is_none_or(|a| fold(artist_name(left)) == a) {
            return Some(right.trim());
        }
        at += i + 3;
    }
    (artist.is_none() && !t.contains(" - ") && fold(t) == album).then_some(t)
}

/// The one master that agrees with the query: its album equal to the
/// query's (compared by [`fold`]), its artist equal when the title names
/// one, and its year equal when the query has one. None, or more than one
/// (the same id listed twice counts once), is no match.
pub fn choose(query: &Query, hits: &[Hit]) -> Lookup {
    if query.kind() != MediaKind::Music {
        return Lookup::NoMatch;
    }
    let Some(want) = wanted(query.title()) else {
        return Lookup::NoMatch;
    };
    let album = fold(want.album);
    let artist = want.artist.map(fold);
    let mut one: Vec<(&Hit, &str)> = Vec::new();
    for h in hits {
        if query.year().is_some_and(|y| h.year != Some(y)) {
            continue;
        }
        if let Some(name) = agrees(h, &album, artist.as_deref())
            && !one.iter().any(|(o, _)| o.id == h.id)
        {
            one.push((h, name));
        }
    }
    match one.as_slice() {
        [(h, name)] => Lookup::Match(identification(query, want.artist.is_some(), h, name)),
        _ => Lookup::NoMatch,
    }
}

/// A title for comparing: lower case, letters and digits only, each run
/// of anything else one space. MusicBrainz's rule.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

/// The answer for `h`: `discogs:m<id>` (a master), its album as the
/// canonical title and its year. No alternative titles, no genres and no
/// artist (see the decisions file).
fn identification(query: &Query, artist: bool, h: &Hit, album: &str) -> Identification {
    let confidence = match (artist, query.year().is_some()) {
        (true, true) => CONFIDENCE_ARTIST_YEAR,
        (true, false) => CONFIDENCE_ARTIST,
        (false, true) => CONFIDENCE_YEAR,
        (false, false) => CONFIDENCE_TITLE,
    };
    Identification {
        ids: vec![ExternalId {
            scheme: "discogs".into(),
            value: format!("m{}", h.id),
        }],
        canonical_title: Some(album.to_owned()),
        alt_titles: Vec::new(),
        genres: Vec::new(),
        year: query.year().or(h.year),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_header_safe() {
        assert!(valid_token("abcDEF0123456789"));
        for bad in ["", "a b", "a\r\nX: y", "a:b", "a.b", "é", "a=b"] {
            assert!(!valid_token(bad), "{bad:?}");
        }
        assert!(!valid_token(&"a".repeat(MAX_TOKEN + 1)));
    }

    #[test]
    fn artist_marks_are_dropped() {
        assert_eq!(artist_name("Nirvana (2)"), "Nirvana");
        assert_eq!(artist_name("Prince*"), "Prince");
        assert_eq!(artist_name(" Name (12)* "), "Name");
        assert_eq!(artist_name("Band (Live)"), "Band (Live)");
        assert_eq!(artist_name("()"), "()");
        assert_eq!(artist_name(" (3)"), "(3)");
    }
}
