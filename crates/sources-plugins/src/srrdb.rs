//! srrDB, an enrichment plugin for films (ENR-srrdb; architecture §18.2,
//! §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q4, Q5;
//! `docs/decisions/2026-11-15-ENR-srrdb.md`). It follows Wikidata's
//! pattern (`docs/decisions/2026-11-12-ENR-14.md`, "How to add the next
//! plugin").
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin answers one [`Query`] of kind
//! Movie with **one request**:
//!
//! 1. the query's title folded to its words (letters and digits only) and
//!    its year, each one path segment of a srrDB search ([`search_request`]);
//! 2. each returned scene release name read locally into a title and a
//!    year ([`scene_title`]);
//! 3. the one IMDb id that every agreeing release carries ([`choose`]).
//!    Two ids, two years or none is no match: it never guesses.
//!
//! **Only the parsed title and year leave the install** (owner Q5). Never
//! the torrent's name, hash, files, sizes or CRCs: matching those against
//! srrDB's would need the runtime to hand the plugin the raw name, which
//! Q5 keeps from it (the decisions file's follow-up). srrDB's crawl delay
//! is 10 s ([`MIN_INTERVAL`]), stricter than the owner's one a second.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::claims::ExternalId;
use lodestar_sources::http::parse_retry_after;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::srrdb::{self, Hit, ImdbId, Page, SEARCH_LIMITS};
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "srrdb";
/// The only host the plugin may reach.
pub const HOST: &str = "api.srrdb.com";
/// The search's path on it; each word follows as one path segment.
pub const PATH: &str = "/v1/search";
/// Words of the title sent at most; the rest are left out (a search with
/// fewer words finds more, never fewer, releases).
pub const MAX_WORDS: usize = 12;
/// The project, its version and its URL, the same for every install:
/// nothing about the user, their host or their account.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " ( ",
    env!("CARGO_PKG_REPOSITORY"),
    " )"
);
/// What leaves the install, for the Sources row.
pub const SENDS: &str = "The film's parsed title, as separate words, and its year. Never the \
    torrent's name, hash, files, sizes or checksums.";

/// How sure a match is when the query has a year and it agrees.
pub const CONFIDENCE_YEAR: u8 = 80;
/// The query has no year; every agreeing release has the same one.
pub const CONFIDENCE_TITLE: u8 = 65;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "srrDB",
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

/// The least time between two requests: srrDB's API host asks every
/// client for a 10 s crawl delay (its `robots.txt`), stricter than the
/// owner's cap of one a second (Q4).
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);

/// One request per [`MIN_INTERVAL`], no burst. Back off after a 429 or
/// 5xx: a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::every(MIN_INTERVAL),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    ..Policy::STRICT
};

/// The service asked for slower requests (429 or 503).
pub const RATE_LIMITED: &str = "srrDB is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "srrDB requests are on hold; backing off";
/// A 200 whose body is an error object.
pub const API_ERROR: &str = "srrDB answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "srrDB's answer wasn't understood";
const FAILED: &str = "srrDB's request failed";
const BAD_URL: &str = "srrDB's address didn't build";

/// The srrDB plugin. It holds no credential; its one state is the wait
/// after an API error.
#[derive(Debug, Default)]
pub struct Srrdb {
    not_before: Mutex<Option<Instant>>,
}

impl Srrdb {
    pub fn new() -> Srrdb {
        Srrdb::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl Identifier for Srrdb {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Movie]
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
            if query.kind() != MediaKind::Movie {
                return Ok(Lookup::NoMatch);
            }
            // A title of nothing but punctuation names no film: nothing
            // is sent.
            let Some(req) = search_request(query)? else {
                return Ok(Lookup::NoMatch);
            };
            self.not_held()?;
            let res = self.answer(kit.http.send(&req).await)?;
            let (status, headers, body) = (res.status, res.headers, res.body);
            let found =
                tokio::task::spawn_blocking(move || srrdb::parse_search(&body, &SEARCH_LIMITS))
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
            Ok(choose(query, &found.hits))
        })
    }
}

impl Srrdb {
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
            Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
            Err(_) => return Err(fail(FAILED)),
        };
        match res.status {
            200 => Ok(res),
            429 | 503 => Err(fail(RATE_LIMITED)),
            _ => Err(fail(FAILED)),
        }
    }

    /// Only a whole answer is used, as a match or a miss
    /// ([`Page::whole`]). An error object on a 200, which the host
    /// limiter doesn't see, holds the plugin for its `Retry-After`, or
    /// [`API_ERROR_WAIT`], at most [`MAX_API_ERROR_WAIT`].
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

/// The words sent for a title: [`fold`]ed, so each is letters and digits
/// only and none can be one of srrDB's search operators (`r:`, `imdb:`,
/// `-word`, `/`), at most [`MAX_WORDS`].
pub fn words(title: &str) -> Vec<String> {
    fold(title)
        .split(' ')
        .filter(|w| !w.is_empty())
        .take(MAX_WORDS)
        .map(str::to_owned)
        .collect()
}

/// The search for `query`: `https://api.srrdb.com/v1/search/<word>/…/<year>`,
/// each word and the year one encoded path segment. `None` when the
/// title has no word: nothing is sent.
pub fn search_request(query: &Query) -> Result<Option<Request>, PluginError> {
    let words = words(query.title());
    if words.is_empty() {
        return Ok(None);
    }
    let mut url = url::Url::parse(&format!("https://{HOST}{PATH}")).map_err(|_| fail(BAD_URL))?;
    {
        let mut path = url.path_segments_mut().map_err(|_| fail(BAD_URL))?;
        path.extend(words.iter().map(String::as_str));
        if let Some(y) = query.year() {
            path.push(&y.to_string());
        }
    }
    Ok(Some(
        Request::get(url.as_str())
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT),
    ))
}

/// A scene film release's title and year, read from its name the way
/// scene names are written: `Title.Words.YEAR.tags-GROUP` (dots,
/// underscores or spaces between words). The year is the last
/// four-digit token from 1880 to 2100 after the first word; the title is
/// every word before it, [`fold`]ed. `None` when there's no such year, no
/// title, or a season or episode token (`S01`, `S01E02`) follows: that's
/// a TV release.
pub fn scene_title(release: &str) -> Option<(String, u16)> {
    // The group: what follows the last `-`, when it has no separator.
    let base = match release.rsplit_once('-') {
        Some((b, g)) if !g.is_empty() && !g.contains(['.', '_', ' ']) => b,
        _ => release,
    };
    let tokens: Vec<&str> = base
        .split(['.', '_', ' '])
        .filter(|t| !t.is_empty())
        .collect();
    let (at, year) = tokens
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(i, t)| year_token(t).map(|y| (i, y)))
        .next_back()?;
    if tokens.get(at..)?.iter().any(|t| episode_token(t)) {
        return None;
    }
    let title = fold(&tokens.get(..at)?.join(" "));
    (!title.is_empty()).then_some((title, year))
}

fn year_token(t: &str) -> Option<u16> {
    let y: u16 = (t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit()))
        .then(|| t.parse().ok())
        .flatten()?;
    (1880..=2100).contains(&y).then_some(y)
}

/// `S01`, `S01E02`, `S2024E101`: a season, with or without an episode.
fn episode_token(t: &str) -> bool {
    let b = t.as_bytes();
    let Some((&s, rest)) = b.split_first() else {
        return false;
    };
    if !s.eq_ignore_ascii_case(&b'S') {
        return false;
    }
    let season = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let tail = rest.get(season..).unwrap_or_default();
    (1..=4).contains(&season)
        && (tail.is_empty()
            || tail.split_first().is_some_and(|(&e, d)| {
                e.eq_ignore_ascii_case(&b'E')
                    && !d.is_empty()
                    && d.len() <= 4
                    && d.iter().all(u8::is_ascii_digit)
            }))
}

/// The one IMDb id the releases that agree with the query carry. A
/// release agrees when [`scene_title`] reads its name to the query's
/// [`fold`]ed title, and to its year when the query has one. A match
/// needs every agreeing release that has an id to have the same one, and
/// every agreeing release the same year; otherwise no match.
pub fn choose(query: &Query, hits: &[Hit]) -> Lookup {
    if query.kind() != MediaKind::Movie {
        return Lookup::NoMatch;
    }
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let (mut id, mut year): (Option<&ImdbId>, Option<u16>) = (None, None);
    for h in hits {
        let Some((title, y)) = scene_title(&h.release) else {
            continue;
        };
        if title != want || query.year().is_some_and(|q| q != y) {
            continue;
        }
        if year.replace(y).is_some_and(|seen| seen != y) {
            return Lookup::NoMatch;
        }
        if let Some(i) = &h.imdb
            && id.replace(i).is_some_and(|seen| seen != i)
        {
            return Lookup::NoMatch;
        }
    }
    let Some(id) = id else {
        return Lookup::NoMatch;
    };
    Lookup::Match(Identification {
        ids: vec![ExternalId {
            scheme: "imdb".into(),
            value: id.to_string(),
        }],
        canonical_title: None,
        alt_titles: Vec::new(),
        genres: Vec::new(),
        year: query.year().or(year),
        confidence: if query.year().is_some() {
            CONFIDENCE_YEAR
        } else {
            CONFIDENCE_TITLE
        },
    })
}

/// A title for comparing and sending: lower case, letters and digits
/// only, each run of anything else one space.
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
