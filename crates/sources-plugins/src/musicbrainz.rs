//! MusicBrainz, an enrichment plugin for music (ENR-mb; architecture
//! §18.2, §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-mb.md`). It follows Wikidata's pattern
//! (`docs/decisions/2026-11-12-ENR-14.md`, "How to add the next plugin").
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin only answers one [`Query`] of
//! kind Music, with **one request**:
//!
//! 1. the query's title is split into an artist and an album at its first
//!    ` - ` (the release-name parser writes music titles so), or is the
//!    album alone;
//! 2. a release-group search for that album (and artist), each as a
//!    quoted phrase with every Lucene special character escaped
//!    ([`phrase`]), as one encoded URL parameter, for at most
//!    [`SEARCH_LIMIT`] groups;
//! 3. the one group whose title, artist and year agree with the query
//!    ([`choose`]). No group, or several that the primary type can't
//!    tell apart, is no match: it never guesses.
//!
//! Every request carries a [`USER_AGENT`] naming the project and its
//! URL, never the user or the install, as MusicBrainz's rate-limiting
//! rules ask. Errors read the HTTP status and the parser's report only:
//! the service's text is never kept or put in an error.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::claims::ExternalId;
use lodestar_sources::http::parse_retry_after;
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::musicbrainz::{self, Hit, Page, Primary, SEARCH_LIMITS};
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "musicbrainz";
/// The only host the plugin may reach.
pub const HOST: &str = "musicbrainz.org";
/// The release-group search's path on it.
pub const PATH: &str = "/ws/2/release-group";
/// Release groups asked for in a search. The match must be among the top
/// few.
pub const SEARCH_LIMIT: usize = 5;
/// MusicBrainz's User-Agent rule: the application, its version and a
/// way to reach the project, `Name/<version> ( contact-url )`. The
/// project's own URL, the same for every install: nothing about the
/// user, their host or their account.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " ( ",
    env!("CARGO_PKG_REPOSITORY"),
    " )"
);

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
    name: "MusicBrainz",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Low,
    consent: Consent::External,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// MusicBrainz's rate limit: one request a second on average per address
/// (the plan's cap, §2.26), with no burst. Over it, it answers 503. Back
/// off after a 429 or 5xx: a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    ..Policy::STRICT
};

/// The least time between two lookups: one request at one a second.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The service asked for slower requests (429, or its 503 over the rate).
pub const RATE_LIMITED: &str = "MusicBrainz is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "MusicBrainz requests are on hold; backing off";
/// A 200 whose body is an error object.
pub const API_ERROR: &str = "MusicBrainz answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "MusicBrainz's answer wasn't understood";
const FAILED: &str = "MusicBrainz's request failed";
const BAD_URL: &str = "MusicBrainz's address didn't build";

/// The MusicBrainz plugin. It holds no credential; its one state is the
/// wait after an API error.
#[derive(Debug, Default)]
pub struct MusicBrainz {
    not_before: Mutex<Option<Instant>>,
}

impl MusicBrainz {
    pub fn new() -> MusicBrainz {
        MusicBrainz::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
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
/// `None` when nothing in it could name an album.
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

/// MusicBrainz's terms (ENR-terms; `docs/decisions/2026-11-15-ENR-mb.md`).
/// The plugin keeps only core data, which is CC0 ("About / Data License"),
/// so its answers may be passed on. Its "Rate Limiting" page allows
/// background work spread through the day within one request a second,
/// which the runtime's capped walk is. CC0 needs no credit; the line is a
/// courtesy, as for Wikidata. Claims are still refreshed within 180 days.
pub const TERMS: Terms = Terms {
    background_walk: true,
    redistribute: true,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: "MusicBrainz (CC0)",
        link: "https://musicbrainz.org",
        logo: None,
    }),
    // MusicBrainz has no adult marker on a release group to read. The
    // runtime's gate never asks about an Adult item, and its kind is Music
    // only.
    adult_marker_required: false,
};

impl Identifier for MusicBrainz {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn terms(&self) -> &Terms {
        &TERMS
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
            // A title of nothing but punctuation can name no album:
            // nothing is sent.
            let Some(want) = wanted(query.title()) else {
                return Ok(Lookup::NoMatch);
            };
            self.not_held()?;
            let res = self.answer(kit.http.send(&search_request(&want)?).await)?;
            // Up to 4 MiB: parsed on the blocking pool, so a large answer
            // never holds up the runtime's other tasks.
            let (status, headers, body) = (res.status, res.headers, res.body);
            let found = tokio::task::spawn_blocking(move || {
                musicbrainz::parse_search(&body, &SEARCH_LIMITS)
            })
            .await
            .map_err(|_| fail(BAD_ANSWER))?;
            self.usable(
                &found.page,
                &Response {
                    status,
                    headers,
                    body: Vec::new(),
                },
            )?;
            let hits: Vec<&Hit> = found.hits.iter().collect();
            Ok(choose(query, &hits))
        })
    }
}

impl MusicBrainz {
    /// Fails at once, sending nothing, while an API error's wait lasts.
    fn not_held(&self) -> Result<(), PluginError> {
        let until = *self.not_before.lock().unwrap_or_else(|e| e.into_inner());
        match until {
            Some(t) if Instant::now() < t => Err(fail(HELD)),
            _ => Ok(()),
        }
    }

    /// A 200, or the error its status means.
    fn answer(&self, res: Result<Response, HttpError>) -> Result<Response, PluginError> {
        let res = match res {
            Ok(r) => r,
            // The limiter holds the host after a 429 or 5xx; the retry
            // waits.
            Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
            Err(_) => return Err(fail(FAILED)),
        };
        match res.status {
            200 => Ok(res),
            // MusicBrainz answers 503 when a client is over its rate.
            429 | 503 => Err(fail(RATE_LIMITED)),
            _ => Err(fail(FAILED)),
        }
    }

    /// Only a whole answer is used, as a match or a miss
    /// ([`Page::whole`]), so a broken one is never cached as a miss. An
    /// error object on a 200, which the host limiter doesn't see, holds
    /// the plugin for its `Retry-After`, or [`API_ERROR_WAIT`], at most
    /// [`MAX_API_ERROR_WAIT`].
    fn usable(&self, page: &Page, res: &Response) -> Result<(), PluginError> {
        if page.error {
            let wait = res
                .header("retry-after")
                .and_then(parse_retry_after)
                .unwrap_or(API_ERROR_WAIT)
                .clamp(MIN_INTERVAL, MAX_API_ERROR_WAIT);
            *self.not_before.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + wait);
            return Err(fail(API_ERROR));
        }
        if !page.whole() {
            return Err(fail(BAD_ANSWER));
        }
        Ok(())
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// `text` as one Lucene phrase: in double quotes, with every character
/// Lucene's query syntax gives a meaning (`\` and `"` among them) escaped
/// by a backslash, so nothing in it can end the phrase, add a field or an
/// operator.
pub fn phrase(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c.is_control() {
            out.push(' ');
            continue;
        }
        if matches!(
            c,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The search's Lucene query: a fixed template, each piece of the title
/// a [`phrase`].
pub fn lucene(want: &Wanted<'_>) -> String {
    let album = format!("releasegroup:{}", phrase(want.album));
    match want.artist {
        Some(a) => format!("{album} AND artist:{}", phrase(a)),
        None => album,
    }
}

/// The search for `want`. The title is the only item data in it, as one
/// encoded parameter; the year and the kind aren't sent.
pub fn search_request(want: &Wanted<'_>) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}{PATH}")).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .append_pair("query", &lucene(want))
        .append_pair("limit", &SEARCH_LIMIT.to_string())
        .append_pair("fmt", "json");
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT))
}

/// The one release group that agrees with the query: its title equal to
/// the album (compared by [`fold`]), its credit or one of its credited
/// names equal to the artist when the title names one, and its first
/// release year equal to the query's when it has one. When several
/// agree, the one album among them; otherwise no match.
pub fn choose(query: &Query, hits: &[&Hit]) -> Lookup {
    if query.kind() != MediaKind::Music {
        return Lookup::NoMatch;
    }
    let Some(want) = wanted(query.title()) else {
        return Lookup::NoMatch;
    };
    let album = fold(want.album);
    let artist = want.artist.map(fold);
    let agree: Vec<&Hit> = hits
        .iter()
        .copied()
        .filter(|h| {
            fold(&h.title) == album
                && artist.as_deref().is_none_or(|a| {
                    h.credit.as_deref().is_some_and(|c| fold(c) == a)
                        || h.artists.iter().any(|n| fold(n) == a)
                })
                && query.year().is_none_or(|y| h.year == Some(y))
        })
        .collect();
    // The same group listed twice is one group.
    let mut one: Vec<&Hit> = Vec::new();
    for h in agree {
        if !one.iter().any(|o| o.id == h.id) {
            one.push(h);
        }
    }
    let pick = match one.as_slice() {
        [h] => Some(*h),
        [] => None,
        _ => {
            let mut albums = one.iter().filter(|h| h.primary == Some(Primary::Album));
            match (albums.next(), albums.next()) {
                (Some(h), None) => Some(*h),
                _ => None,
            }
        }
    };
    match pick {
        Some(h) => Lookup::Match(identification(query, want.artist.is_some(), h)),
        None => Lookup::NoMatch,
    }
}

/// A title for comparing: lower case, letters and digits only, each run
/// of anything else one space.
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

/// The answer for `h`: its release-group id, its title as the canonical
/// title and its first release year. No alternative titles or genres
/// (MusicBrainz's tags are user-written and under a non-commercial
/// licence; see the decisions file).
fn identification(query: &Query, artist: bool, h: &Hit) -> Identification {
    let confidence = match (artist, query.year().is_some()) {
        (true, true) => CONFIDENCE_ARTIST_YEAR,
        (true, false) => CONFIDENCE_ARTIST,
        (false, true) => CONFIDENCE_YEAR,
        (false, false) => CONFIDENCE_TITLE,
    };
    Identification {
        ids: vec![ExternalId {
            scheme: "mbid".into(),
            value: h.id.to_string(),
        }],
        canonical_title: Some(h.title.clone()),
        alt_titles: Vec::new(),
        genres: Vec::new(),
        year: query.year().or(h.year),
        confidence,
        // MusicBrainz doesn't mark adult works; `TERMS` opts out.
        adult: None,
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;

    /// CC0 core data: walked in the background within the rate and passed
    /// on, credited as a courtesy, no marker to read, claims refreshed
    /// within 180 days.
    #[test]
    fn musicbrainz_terms() {
        let t = MusicBrainz::new();
        let t = Identifier::terms(&t);
        assert!(t.background_walk && t.redistribute && !t.adult_marker_required);
        assert_eq!(t.retention, Retention::DEFAULT);
        assert_eq!(
            t.attribution.map(|a| a.link),
            Some("https://musicbrainz.org")
        );
        assert!(t.accepts(None) && !t.accepts(Some(true)));
    }
}
