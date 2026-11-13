//! Wikidata, a general enrichment plugin (ENR-14; architecture §18.1,
//! §18.2, §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-12-ENR-14.md`). It's the pattern the other
//! v0.7 enrichment plugins copy.
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin only answers one [`Query`]:
//!
//! 1. `wbsearchentities`: the query's title, and nothing else from the
//!    item, as a URL parameter (never pasted into a query language), for
//!    at most [`SEARCH_LIMIT`] items;
//! 2. `wbgetentities` for the results whose label, alias or matched text
//!    is the title (a search also finds items by words in a longer
//!    title), their ids each checked by
//!    [`Qid::parse`]: their labels and aliases in [`LANGUAGES`] and their
//!    statements;
//! 3. picks the one item whose kind, title and year agree with the query
//!    ([`choose`]). No item, or more than one, is no match: it never
//!    guesses.
//!
//! An API error (such as `maxlag`) holds the plugin for the answer's
//! `Retry-After` before it sends again.
//!
//! Both requests use Wikimedia's Action API with a fixed set of
//! parameters, `maxlag` (the service's back-pressure for background
//! work) and a [`USER_AGENT`] naming the project, never the user or the
//! install. Errors read the HTTP status and the parser's report only: the
//! service's text is never kept or put in an error.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::claims::{AltTitle, ExternalId};
use lodestar_sources::http::parse_retry_after;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::wikidata::{self, ENTITY_LIMITS, Entity, Page, Qid, SEARCH_LIMITS};
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "wikidata";
/// The only host the plugin may reach.
pub const HOST: &str = "www.wikidata.org";
/// The Action API's path on it.
pub const API: &str = "/w/api.php";
/// Items asked for in a search. The match must be among the top few.
pub const SEARCH_LIMIT: usize = 5;
/// The languages labels and aliases are asked in; the search itself is in
/// English. `mul` is Wikidata's label for every language.
pub const LANGUAGES: &str = "en|mul|de|es|fr|it|ja|ko|pt|ru|zh";
/// Seconds of replication lag past which the service refuses the request
/// (`maxlag`): Wikimedia asks every non-interactive client to send it.
pub const MAXLAG: &str = "5";
/// Wikimedia's User-Agent policy: the client, its version and a way to
/// reach the project. The project's own URL, the same for every install:
/// nothing about the user, their host or their account.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ") lodestar-sources-plugins"
);

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 90;
/// The title agrees and the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 70;

/// What a film is an instance of (P31). An item with a TMDB movie id is a
/// film too.
pub const FILMS: [u64; 6] = [
    11_424,  // film
    24_869,  // feature film
    202_866, // animated film
    506_240, // television film
    226_730, // silent film
    93_204,  // short film
];
/// What a series is an instance of. An item with a TMDB TV or TVDB id is
/// a series too.
pub const SERIES: [u64; 5] = [
    5_398_426,  // television series
    581_714,    // animated television series
    1_259_759,  // miniseries
    526_877,    // web series
    63_952_888, // anime television series
];

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Wikidata",
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

/// Wikimedia's robot policy for clients that don't sign in: one request at
/// a time, well under five a second. The plan's cap is one a second
/// (§2.26), with no burst; a lookup is two requests, so they're at least
/// [`MIN_INTERVAL`] apart. Back-off after a 429 or 5xx: a minute up to
/// 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: std::time::Duration::from_secs(60),
            max: std::time::Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    ..Policy::STRICT
};

/// The least time between two lookups: two requests at one a second.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "Wikidata is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "Wikidata requests are on hold; backing off";
/// The answer was an API error, such as `maxlag` when the service is busy.
pub const API_ERROR: &str = "Wikidata answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "Wikidata's answer wasn't understood";
const FAILED: &str = "Wikidata's request failed";
const BAD_URL: &str = "Wikidata's address didn't build";

/// The Wikidata plugin. It holds no credential; its one state is the
/// wait after an API error.
#[derive(Debug, Default)]
pub struct Wikidata {
    not_before: Mutex<Option<Instant>>,
}

impl Wikidata {
    pub fn new() -> Wikidata {
        Wikidata::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl Identifier for Wikidata {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Movie, MediaKind::Tv]
    }

    fn min_interval(&self) -> std::time::Duration {
        MIN_INTERVAL
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            // A title of nothing but punctuation can name no item: nothing
            // is sent.
            let want = fold(query.title());
            if want.is_empty() {
                return Ok(Lookup::NoMatch);
            }
            self.not_held()?;
            let res = self.answer(kit.http.send(&search_request(query.title())?).await)?;
            let found = wikidata::parse_search(&res.body, &SEARCH_LIMITS);
            self.usable(&found.page, &res)?;
            // Only results that matched on the title itself are fetched,
            // once each: a search also finds items by words in a longer
            // title, and every entity fetched is up to megabytes.
            let mut ids: Vec<Qid> = Vec::new();
            for hit in found.hits {
                let named = hit.texts.iter().any(|t| fold(t) == want);
                if named && !ids.contains(&hit.id) {
                    ids.push(hit.id);
                }
            }
            if ids.is_empty() {
                return Ok(Lookup::NoMatch);
            }
            let res = self.answer(kit.http.send(&entities_request(&ids)?).await)?;
            // Up to 8 MiB: parsed on the blocking pool, so a large answer
            // never holds up the runtime's other tasks.
            let body = res.body;
            let got = tokio::task::spawn_blocking(move || {
                wikidata::parse_entities(&body, &ENTITY_LIMITS)
            })
            .await
            .map_err(|_| fail(BAD_ANSWER))?;
            self.usable(
                &got.page,
                &Response {
                    status: res.status,
                    headers: res.headers,
                    body: Vec::new(),
                },
            )?;
            // Only the items asked about count, in the search's order, and
            // each must be there: one left out (missing, or keyed by a
            // redirect's target) is an error, never cached as a miss.
            let mut entities = Vec::with_capacity(ids.len());
            for id in &ids {
                let e = got.entities.iter().find(|e| e.id == *id);
                entities.push(e.ok_or_else(|| fail(BAD_ANSWER))?);
            }
            Ok(choose(query, &entities))
        })
    }
}

impl Wikidata {
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
            429 => Err(fail(RATE_LIMITED)),
            _ => Err(fail(FAILED)),
        }
    }

    /// Only a whole answer is used, as a match or a miss
    /// ([`Page::whole`]), so a broken one is never cached as a miss. An
    /// API error (such as `maxlag`, which comes as a 200 the host limiter
    /// doesn't see) holds the plugin for its `Retry-After`, or
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

/// The fixed parameters every request carries.
fn api_url(pairs: &[(&str, &str)]) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}{API}")).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("format", "json")
        .append_pair("maxlag", MAXLAG);
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT))
}

/// The search for `title`. The title is the only item data in it, as one
/// encoded parameter.
pub fn search_request(title: &str) -> Result<Request, PluginError> {
    api_url(&[
        ("action", "wbsearchentities"),
        ("search", title),
        ("language", "en"),
        ("uselang", "en"),
        ("type", "item"),
        ("limit", &SEARCH_LIMIT.to_string()),
    ])
}

/// The labels, aliases and statements of `ids`, which [`Qid`] already
/// checked (their text is `Q` and digits only).
pub fn entities_request(ids: &[Qid]) -> Result<Request, PluginError> {
    let ids = ids
        .iter()
        .take(SEARCH_LIMIT)
        .map(Qid::to_string)
        .collect::<Vec<_>>()
        .join("|");
    api_url(&[
        ("action", "wbgetentities"),
        ("ids", &ids),
        ("props", "labels|aliases|claims"),
        ("languages", LANGUAGES),
    ])
}

/// The one item that agrees with the query: its kind (an instance of a
/// film or series, or holding that kind's TMDB or TVDB id), a label or
/// alias equal to the title (compared by [`fold`]), and, when the query
/// has a year, that year among its publication or start dates. None, or
/// more than one, is no match.
pub fn choose(query: &Query, entities: &[&Entity]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let mut agree = entities.iter().filter(|e| {
        kind_ok(query.kind(), e)
            && e.labels
                .iter()
                .chain(&e.aliases)
                .any(|t| fold(&t.value) == want)
            && query.year().is_none_or(|y| e.years.contains(&y))
    });
    match (agree.next(), agree.next()) {
        (Some(e), None) => Lookup::Match(identification(query, e)),
        _ => Lookup::NoMatch,
    }
}

fn kind_ok(kind: MediaKind, e: &Entity) -> bool {
    let of = |list: &[u64]| e.instance_of.iter().any(|q| list.contains(&q.0));
    match kind {
        MediaKind::Movie => of(&FILMS) || !e.tmdb_movie.is_empty(),
        MediaKind::Tv => of(&SERIES) || !e.tmdb_tv.is_empty() || !e.tvdb.is_empty(),
        _ => false,
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

/// The answer for `e`: its Wikidata id, and each cross-id of the query's
/// kind only when the item holds exactly one value of it (two disagreeing
/// values name no one work). The English label (else the `mul` one) is
/// the canonical title; the other labels and aliases are alternatives.
fn identification(query: &Query, e: &Entity) -> Identification {
    let mut ids = vec![id("wikidata", e.id.to_string())];
    let mut one = |scheme: &str, values: &[String]| {
        if let [v] = dedup(values).as_slice() {
            ids.push(id(scheme, (*v).clone()));
        }
    };
    one("imdb", &e.imdb);
    match query.kind() {
        MediaKind::Movie => one("tmdb", &e.tmdb_movie),
        MediaKind::Tv => {
            one("tmdb", &e.tmdb_tv);
            one("tvdb", &e.tvdb);
        }
        _ => {}
    }
    let canonical = ["en", "mul"]
        .iter()
        .find_map(|l| e.labels.iter().find(|t| t.language == *l))
        .map(|t| t.value.clone());
    let alt_titles = e
        .labels
        .iter()
        .chain(&e.aliases)
        .filter(|t| canonical.as_deref() != Some(t.value.as_str()))
        .map(|t| AltTitle {
            language: t.language.clone(),
            title: t.value.clone(),
        })
        .collect();
    let year = query.year().or_else(|| e.years.iter().min().copied());
    Identification {
        ids,
        canonical_title: canonical,
        alt_titles,
        genres: Vec::new(),
        year,
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
    }
}

fn dedup(v: &[String]) -> Vec<&String> {
    let mut out: Vec<&String> = Vec::new();
    for s in v {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn id(scheme: &str, value: String) -> ExternalId {
    ExternalId {
        scheme: scheme.into(),
        value,
    }
}
