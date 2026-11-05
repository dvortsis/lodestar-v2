//! StashDB, an adult-enrichment plugin (ENR-5; architecture §20.9; plan
//! `docs/plan/2026-11-04-v06-rulepacks.md` Q2, Q4, Q5, Q15;
//! `docs/decisions/2026-11-05-ENR-5.md`). The twin of `tpdb` (ENR-4).
//!
//! **On HOLD: not registered anywhere, and not enable-able.** The Q2 terms
//! and robots check for the service's API is owed by the owner, and the
//! host wiring is a later slice (see the decisions file).
//!
//! The pipeline's enrichment runtime (`AdultEnrichSource`) decides whether
//! an item may be looked up at all, and paces the lookups. This plugin only
//! answers one [`Query`]:
//!
//! 1. builds one `POST https://stashdb.org/graphql` from the display name
//!    and ENR-3's fixed `SEARCH_SCENE` document. The name travels as a
//!    GraphQL variable, never spliced into the document. A [`Query`] has no
//!    field for an info hash, a file name, a tracker or anything about the
//!    install; the plugin never asks for a performer id or a personal
//!    field, and never uses the fingerprint or by-id queries;
//! 2. sends it with the API key in an `ApiKey` header (never in the URL or
//!    the body), through the kit's client, the only way it may send
//!    anything;
//! 3. reads the answer with ENR-3's bounded parser and maps the top match
//!    to ENR-0's section, deciding match, miss or error only on what the
//!    parser reports it saw.
//!
//! **Errors read the HTTP status only.** A 401 or 403 is a bad key, a 429
//! backs the host off, and the service's message text is never kept, read
//! or put in an error.

use std::fmt;
use std::sync::Arc;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{
    AdultEnrichment, AdultKind, MAX_PERFORMER, MAX_PERFORMERS, MAX_SCENE_ID, MAX_SCENE_TITLE,
    MAX_STUDIO, ReleaseDate,
};
use lodestar_sources::consent::Need;
use lodestar_sources::enrich::{AdultEnricher, Lookup, Query, QueryForm};
use lodestar_sources::http::Method;
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Risk,
};
use lodestar_sources_feeds::stashdb::{self, LIMITS, Limits, Scene, Seen, Stop};
use serde::Serialize;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "stashdb";
/// The credential that holds the API key.
pub const API_KEY: &str = "api_key";
/// StashDB's host, the only one the plugin may reach.
pub const HOST: &str = "stashdb.org";
/// The GraphQL endpoint.
pub const ENDPOINT: &str = "https://stashdb.org/graphql";
/// The header StashDB reads the key from.
pub const KEY_HEADER: &str = "ApiKey";
/// Results asked for. One: only the top result is ever used.
pub const SEARCH_LIMIT: u8 = 1;
/// Bytes in an API key. StashDB's are JWTs of a few hundred characters.
pub const MAX_KEY: usize = 2048;

/// The caps for one answer: ENR-3's, with one item read and the performer
/// limit the section holds. Only the service's top-ranked result is used:
/// if it can't be read, the answer is an error, never the second-best
/// guess, which could pin another scene's performers on the torrent (Q4).
pub const ANSWER_LIMITS: Limits = Limits {
    max_items: 1,
    max_performers: MAX_PERFORMERS,
    ..LIMITS
};

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "StashDB",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Medium,
    consent: Consent::Account,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
};

/// The settings form (§10.10). Only the API key, write-only: the host
/// stores it as the `api_key` credential (SRC-4) and never returns it.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"api_key":{"type":"string","writeOnly":true,"title":"API key","description":"Your StashDB API key"}},"required":["api_key"],"additionalProperties":false}"#;

/// Q15: at most one request a second to the service, no burst. The runtime
/// holds the same rate (`adult_enrich::capped`) and counts the daily cap
/// (5,000 background lookups) and the caches itself, so there's no second
/// limiter here. Back-off after a 429 or 5xx: a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: std::time::Duration::from_secs(60),
            max: std::time::Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    ..Policy::STRICT
};

/// The key was refused (401 or 403). Names no key.
pub const REFUSED: &str = "StashDB refused the API key";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "StashDB is limiting requests; backing off";
/// The host is held after a refusal or a rate limit; the limiter doesn't say which.
pub const HELD: &str = "StashDB requests are on hold after a refusal or a rate limit; backing off";
const NO_KEY: &str = "needs setup: no StashDB API key is stored";
const BAD_KEY: &str = "the stored StashDB API key isn't a valid key";
const UNREADABLE_KEY: &str = "the stored StashDB API key couldn't be read";
const BAD_ANSWER: &str = "StashDB's answer wasn't understood";
const FAILED: &str = "StashDB's request failed";
const WRONG_FORM: &str = "StashDB is asked by name only";

/// The StashDB plugin.
pub struct StashDb {
    credentials: Arc<dyn Credentials>,
}

impl fmt::Debug for StashDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StashDb").finish_non_exhaustive()
    }
}

impl StashDb {
    pub fn new(credentials: Arc<dyn Credentials>) -> StashDb {
        StashDb { credentials }
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs, given the names of the
    /// credentials stored for it: the API key (Account class).
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need(&MANIFEST, None, stored)
    }
}

impl AdultEnricher for StashDb {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn query_form(&self) -> QueryForm {
        QueryForm::Name
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            let Query::Name(name) = query else {
                return Err(fail(WRONG_FORM));
            };
            // Read on each lookup, so a key removed on purge stops working
            // at once and no copy outlives the call.
            let key = self
                .credentials
                .get(ID, API_KEY)
                .await
                .map_err(|_| fail(UNREADABLE_KEY))?
                .ok_or_else(|| fail(NO_KEY))?;
            if !valid_key(key.expose()) {
                return Err(fail(BAD_KEY));
            }
            let request = search_request(name, &key)?;
            let res = kit.http.send(&request).await;
            answer(res)
        })
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// An API key goes into a header, so only the characters a JWT or token
/// has: ASCII letters, digits and `-`, `_`, `.`, `|`.
fn valid_key(k: &str) -> bool {
    (1..=MAX_KEY).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'|'))
}

/// The GraphQL request body: ENR-3's fixed document and the variables.
#[derive(Serialize)]
struct Body<'a> {
    query: &'static str,
    variables: Variables<'a>,
}

#[derive(Serialize)]
struct Variables<'a> {
    term: &'a str,
    limit: u8,
}

/// The search request for `name`. The name is the only item data in it,
/// and it's a variable, never part of the document.
pub fn search_request(name: &str, key: &Secret) -> Result<Request, PluginError> {
    let body = serde_json::to_vec(&Body {
        query: stashdb::SEARCH_SCENE,
        variables: Variables {
            term: name,
            limit: SEARCH_LIMIT,
        },
    })
    .map_err(|_| fail("StashDB's request didn't build"))?;
    Ok(Request {
        method: Method::Post,
        url: ENDPOINT.to_owned(),
        headers: vec![
            ("Accept".to_owned(), "application/json".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            (KEY_HEADER.to_owned(), key.expose().to_owned()),
        ],
        body,
    })
}

/// The lookup's outcome from the HTTP answer, by status first.
fn answer(res: Result<lodestar_sources::Response, HttpError>) -> Result<Lookup, PluginError> {
    let res = match res {
        Ok(r) => r,
        // The limiter holds the host after a 429 or 5xx; the retry waits.
        Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
        Err(_) => return Err(fail(FAILED)),
    };
    match res.status {
        200 => parse_answer(&res.body),
        401 | 403 => Err(fail(REFUSED)),
        429 => Err(fail(RATE_LIMITED)),
        _ => Err(fail(FAILED)),
    }
}

/// Maps a 200's body: the top match, no match for an answer that
/// understood to nothing, and an error for one that wasn't understood, so a
/// broken answer is never cached as a miss.
///
/// Every decision is made on what ENR-3's parser saw ([`stashdb::Page`]),
/// never on a second read of the body. The answer must be one whole
/// document of exactly this query's shape: read to its end (stopping, if at
/// all, only at the item cap after the top scene), no trailing bytes, no
/// repeated key in the root or in `data`, a `data.searchScene` list, no
/// `findScene` key (another query's answer, which the parser also reads)
/// and no GraphQL `errors` anywhere in the root.
pub fn parse_answer(body: &[u8]) -> Result<Lookup, PluginError> {
    let mut found = None;
    let page = stashdb::parse_each(body, &ANSWER_LIMITS, |s| {
        found.get_or_insert(s);
    });
    let whole = matches!(page.stop, None | Some(Stop::Items)) && !page.trailing;
    // GraphQL errors, even beside a readable scene, mean part of the answer
    // may be missing (a failed performers resolver, say): not understood.
    let shape = page.search == Seen::List && !page.find && !page.repeated && page.errors == 0;
    if !whole || !shape || page.skipped.total() > 0 {
        return Err(fail(BAD_ANSWER));
    }
    match found {
        // `max_items` is 1 and there's no `findScene`, so a kept scene is
        // `searchScene`'s first entry: the service's top result.
        Some(s) => to_lookup(s),
        None if page.stop.is_none() => Ok(Lookup::NoMatch),
        None => Err(fail(BAD_ANSWER)),
    }
}

/// ENR-0's section from one scene. A scene id the section refuses is an
/// error: a cut id would point at another scene. Performers are names
/// only (Q4); the parser keeps no performer id and no personal field (Q5).
pub fn to_lookup(s: Scene) -> Result<Lookup, PluginError> {
    let Scene {
        matched,
        performers,
        performers_truncated,
    } = s;
    // Can't happen (the parser keeps UUIDs), but if it did, a non-empty
    // answer must not be cached as a miss.
    if !scene_id_ok(&matched.id) {
        return Err(fail(BAD_ANSWER));
    }
    let mut section = AdultEnrichment::new(matched.id);
    // Stash-box has scenes only (VR is a tag, which isn't read).
    section.kind = Some(AdultKind::Scene);
    section.title = matched.title.filter(|t| t.len() <= MAX_SCENE_TITLE);
    section.studio = matched
        .studio
        .map(|s| s.name)
        .filter(|s| !s.is_empty() && s.len() <= MAX_STUDIO);
    section.released = matched
        .released
        .and_then(|d| ReleaseDate::new(d.year, d.month, d.day));
    let before = performers.len();
    section.performers = performers
        .into_iter()
        .map(|p| p.name)
        .filter(|n| !n.is_empty() && n.len() <= MAX_PERFORMER)
        .take(MAX_PERFORMERS)
        .collect();
    section.performers_truncated = performers_truncated || section.performers.len() < before;
    Ok(Lookup::Match(section))
}

/// ENR-0's rule for a scene id: 1 to `MAX_SCENE_ID` ASCII letters, digits
/// and `-` (the store checks it again).
fn scene_id_ok(s: &str) -> bool {
    (1..=MAX_SCENE_ID).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
