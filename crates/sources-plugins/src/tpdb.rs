//! ThePornDB, an adult-enrichment plugin (ENR-4; architecture §20.9; plan
//! `docs/plan/2026-11-04-v06-rulepacks.md` Q2, Q4, Q5, Q15;
//! `docs/decisions/2026-11-05-ENR-4.md`).
//!
//! **On HOLD: not registered anywhere, and not enable-able.** The Q2 terms
//! and robots check for the service's API is owed by the owner, and the
//! host wiring is a later slice (see the decisions file).
//!
//! The pipeline's enrichment runtime (`AdultEnrichSource`) decides whether
//! an item may be looked up at all, and paces the lookups. This plugin only
//! answers one [`Query`]:
//!
//! 1. builds `GET https://api.theporndb.net/scenes?parse=<name>&per_page=5`
//!    from the display name, and from nothing else: a [`Query`] has no
//!    field for an info hash, a file name, a tracker or anything about the
//!    install. It never asks for performer ids or personal fields, and
//!    never uses the service's by-hash lookup;
//! 2. sends it with the API key in an `Authorization` header (never in the
//!    URL), through the kit's client, the only way it may send anything;
//! 3. reads the answer with ENR-2b's bounded parser and maps the first
//!    match to ENR-0's section, deciding match, miss or error only on what
//!    the parser reports it saw.
//!
//! **Errors read the HTTP status only.** A 401 or 403 is a bad key, a 429
//! backs the host off, and the service's message text is never kept, read
//! or put in an error.

use std::fmt;
use std::sync::Arc;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{
    AdultEnrichment, AdultKind, MAX_PERFORMER, MAX_PERFORMERS, MAX_SCENE_TITLE, MAX_STUDIO,
    ReleaseDate,
};
use lodestar_sources::consent::Need;
use lodestar_sources::enrich::{AdultEnricher, Lookup, Query, QueryForm};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Risk,
};
use lodestar_sources_feeds::tpdb::{self, Found, LIMITS, Limits, Seen, Stop};

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "tpdb";
/// The credential that holds the API key.
pub const API_KEY: &str = "api_key";
/// ThePornDB's API host, the only one the plugin may reach.
pub const HOST: &str = "api.theporndb.net";
/// Results asked for. The first is used, so a few are enough to let a bad
/// first item be skipped without paging.
pub const PER_PAGE: u8 = 5;
/// Bytes in an API key. ThePornDB's are about 40 characters.
pub const MAX_KEY: usize = 256;

/// The caps for one answer: ENR-2b's, with one item read and the performer
/// limit the section holds. Only the service's top-ranked result is used:
/// if it can't be read, the answer is an error, never the second-best
/// guess, which could pin another scene's performers on the torrent (Q4).
pub const ANSWER_LIMITS: Limits = Limits {
    max_items: 1,
    max_performers: MAX_PERFORMERS,
    ..LIMITS
};

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "ThePornDB",
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
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"api_key":{"type":"string","writeOnly":true,"title":"API key","description":"Your ThePornDB API key"}},"required":["api_key"],"additionalProperties":false}"#;

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
pub const REFUSED: &str = "ThePornDB refused the API key";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "ThePornDB is limiting requests; backing off";
/// The host is held after a refusal or a rate limit; the limiter doesn't say which.
pub const HELD: &str =
    "ThePornDB requests are on hold after a refusal or a rate limit; backing off";
const NO_KEY: &str = "needs setup: no ThePornDB API key is stored";
const BAD_KEY: &str = "the stored ThePornDB API key isn't a valid key";
const UNREADABLE_KEY: &str = "the stored ThePornDB API key couldn't be read";
const BAD_ANSWER: &str = "ThePornDB's answer wasn't understood";
const FAILED: &str = "ThePornDB's request failed";
const WRONG_FORM: &str = "ThePornDB is asked by name only";

/// The ThePornDB plugin.
pub struct Tpdb {
    credentials: Arc<dyn Credentials>,
}

impl fmt::Debug for Tpdb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tpdb").finish_non_exhaustive()
    }
}

impl Tpdb {
    pub fn new(credentials: Arc<dyn Credentials>) -> Tpdb {
        Tpdb { credentials }
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

impl AdultEnricher for Tpdb {
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

/// An API key goes into a header, so only the characters a bearer token
/// has: ASCII letters, digits and `-`, `_`, `.`, `|`.
fn valid_key(k: &str) -> bool {
    (1..=MAX_KEY).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'|'))
}

/// The search request for `name`. The name is the only item data in it.
pub fn search_request(name: &str, key: &Secret) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}/scenes"))
        .map_err(|_| fail("ThePornDB's address didn't build"))?;
    url.query_pairs_mut()
        .append_pair("parse", name)
        .append_pair("per_page", &PER_PAGE.to_string());
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", key.expose())))
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

/// Maps a 200's body: the first kept match, no match for an answer that
/// understood to nothing, and an error for one that wasn't understood, so a
/// broken answer is never cached as a miss.
///
/// Every decision is made on what the parser saw ([`tpdb::Page`]), never on
/// a second read of the body. The answer must be one whole document: the
/// parser read it to its end (stopping, if at all, only at the item cap
/// after the top result), with no trailing bytes and one `data` key.
pub fn parse_answer(body: &[u8]) -> Result<Lookup, PluginError> {
    let mut found = None;
    let page = tpdb::parse_each(body, &ANSWER_LIMITS, |f| {
        found.get_or_insert(f);
    });
    let whole = matches!(page.stop, None | Some(Stop::Items)) && !page.trailing;
    if !whole || page.data_repeated || page.skipped.total() > 0 {
        return Err(fail(BAD_ANSWER));
    }
    match (found, page.data) {
        // `max_items` is 1, so a kept match is the top result.
        (Some(f), Seen::List | Seen::Object) => Ok(to_lookup(f)),
        // A miss is `data` as an empty list or `null`, and nothing else: an
        // error object on a 200, or a `data` of another type, is not a miss.
        (None, Seen::List | Seen::Null) if page.stop.is_none() => Ok(Lookup::NoMatch),
        _ => Err(fail(BAD_ANSWER)),
    }
}

/// ENR-0's section from one match. A scene id the section refuses is no
/// match: a cut id would point at another scene. Performers are names
/// only (Q4); the parser keeps no performer id and no personal field (Q5).
pub fn to_lookup(f: Found) -> Lookup {
    let Found {
        matched,
        performers,
    } = f;
    if !scene_id_ok(&matched.id) {
        return Lookup::NoMatch;
    }
    let mut section = AdultEnrichment::new(matched.id);
    section.kind = matched.kind.map(|k| match k {
        // The service has no VR type; JAV is a scene from its Japanese
        // catalogue.
        tpdb::Kind::Scene | tpdb::Kind::Jav => AdultKind::Scene,
        tpdb::Kind::Movie => AdultKind::Movie,
    });
    section.title = matched.title.filter(|t| t.len() <= MAX_SCENE_TITLE);
    section.studio = matched
        .studio
        .filter(|s| !s.is_empty() && s.len() <= MAX_STUDIO);
    section.released = matched.released.as_deref().and_then(ReleaseDate::parse);
    let before = performers.list.len();
    section.performers = performers
        .list
        .into_iter()
        .map(|p| p.name)
        .filter(|n| !n.is_empty() && n.len() <= MAX_PERFORMER)
        .take(MAX_PERFORMERS)
        .collect();
    section.performers_truncated = performers.truncated || section.performers.len() < before;
    Lookup::Match(section)
}

/// ENR-0's rule for a scene id: 1 to `MAX_SCENE_ID` ASCII letters, digits
/// and `-` (the store checks it again).
fn scene_id_ok(s: &str) -> bool {
    (1..=lodestar_sources::claims::MAX_SCENE_ID).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
