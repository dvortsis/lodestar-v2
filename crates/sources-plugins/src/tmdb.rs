//! TMDB (The Movie Database), a general enrichment plugin for films and
//! series (ENR-tmdb; architecture §18.1, §18.2, §18.7; plan
//! `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-tmdb.md`). Wikidata's shape (ENR-14).
//!
//! **Off by default, Account class.** It needs your own TMDB API read
//! access token, stored in the credential store (SRC-4) and read on each
//! lookup, never kept, logged or put in a URL. ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin only answers one [`Query`], with
//! one request:
//!
//! 1. `GET /3/search/movie` or `/3/search/tv` (the query's kind) with the
//!    title and, when the query has one, the year, each one encoded URL
//!    parameter, `include_adult=false`, and the token in an
//!    `Authorization: Bearer` header;
//! 2. picks the one result whose title and year agree with the query
//!    ([`choose`]). None, or more than one, is no match: it never guesses.
//!
//! Errors read the HTTP status and the parser's report only: the service's
//! text is never kept or put in an error.

use std::fmt;
use std::sync::Arc;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{AltTitle, ExternalId};
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::tmdb::{self, Form, Hit, LIMITS};

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "tmdb";
/// The credential that holds the API read access token.
pub const TOKEN: &str = "read_access_token";
/// The only host the plugin may reach.
pub const HOST: &str = "api.themoviedb.org";
/// Bytes in a token. TMDB's read access tokens are JWTs of a few hundred.
pub const MAX_TOKEN: usize = 2048;

/// What a lookup sends, for its Sources row (UI review 02 §5.5).
pub const SENDS: &str = "The parsed title, its year and whether it's a film or a TV series, \
     with your TMDB read access token, to api.themoviedb.org. Never the info hash, the \
     torrent's name or its files.";
/// TMDB's terms of use require this notice wherever TMDB data is shown,
/// with TMDB's logo (`docs/decisions/2026-11-15-ENR-tmdb.md`).
pub const ATTRIBUTION: &str = "Lodestar uses TMDB and the TMDB APIs but is not endorsed, certified, or otherwise approved by TMDB.";
/// Where the attribution links.
pub const ATTRIBUTION_URL: &str = "https://www.themoviedb.org";

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 90;
/// The title agrees and the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 70;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "TMDB",
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

/// The plan's cap for TMDB is at most 10 requests a second (§2.26), well
/// under TMDB's documented ~40 a second, with no burst. Back-off after a
/// 401, 403, 429 or 5xx: a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(10),
        backoff: Backoff {
            base: std::time::Duration::from_secs(60),
            max: std::time::Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    credentials: &[CredentialSpec {
        name: TOKEN,
        label: "API read access token",
        kind: CredentialKind::Secret,
        required: true,
    }],
    ..Policy::STRICT
};

/// The least time between two lookups: one request at ten a second.
pub const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The token was refused (401 or 403). Names no token.
pub const REFUSED: &str = "TMDB refused the read access token";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "TMDB is limiting requests; backing off";
/// The host is held after a refusal, a rate limit or a server error.
pub const HELD: &str = "TMDB requests are on hold; backing off";
/// No token is stored.
pub const NO_TOKEN: &str = "needs setup: no TMDB read access token is stored";
const BAD_TOKEN: &str = "the stored TMDB read access token isn't a valid token";
const UNREADABLE_TOKEN: &str = "the stored TMDB read access token couldn't be read";
const BAD_ANSWER: &str = "TMDB's answer wasn't understood";
const FAILED: &str = "TMDB's request failed";
const BAD_URL: &str = "TMDB's address didn't build";

/// The TMDB plugin. It holds no token: it reads it on each lookup.
pub struct Tmdb {
    credentials: Arc<dyn Credentials>,
}

impl fmt::Debug for Tmdb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tmdb").finish_non_exhaustive()
    }
}

impl Tmdb {
    pub fn new(credentials: Arc<dyn Credentials>) -> Tmdb {
        Tmdb { credentials }
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

/// TMDB's terms (ENR-terms; `docs/decisions/2026-11-15-ENR-tmdb.md`), its
/// API Terms of Use of 2023-10-20. The licence is non-sublicensable and
/// bars excessive bandwidth and building data sets, so: no whole-index
/// walk, only the items you open, and its answers stay in Lodestar's own
/// UI. It bars caching anything for longer than six months, which the
/// default 180 days meets. Its notice is required wherever its data is
/// shown, in its own words.
pub const TERMS: Terms = Terms {
    background_walk: false,
    redistribute: false,
    // TMDB bars caching its data longer than six months: 180 days, set here.
    retention: Retention::days(180),
    attribution: Some(Attribution {
        text: ATTRIBUTION,
        link: ATTRIBUTION_URL,
        // TMDB's logo is required too; no bundled asset exists yet (see
        // the decisions file, "Before it can be switched on").
        logo: None,
    }),
    // TMDB marks every result `adult` true or false: an answer without the
    // marker is refused.
    adult_marker_required: true,
};

impl Identifier for Tmdb {
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
            let Some(form) = form(query.kind()) else {
                return Ok(Lookup::NoMatch);
            };
            // A title of nothing but punctuation can name no work: nothing
            // is sent, and no token read.
            if fold(query.title()).is_empty() {
                return Ok(Lookup::NoMatch);
            }
            // Read on each lookup, so a token removed on purge stops
            // working at once and none is held between lookups. The
            // request's `Authorization` header is a plain, unzeroed copy
            // that lives until the request is dropped.
            let token = self
                .credentials
                .get(ID, TOKEN)
                .await
                .map_err(|_| fail(UNREADABLE_TOKEN))?
                .ok_or_else(|| fail(NO_TOKEN))?;
            if !valid_token(token.expose()) {
                return Err(fail(BAD_TOKEN));
            }
            let request = search_request(query, &token)?;
            drop(token);
            let res = answer(kit.http.send(&request).await)?;
            let found = tmdb::parse_search(&res.body, form, &LIMITS);
            if !found.page.whole() {
                return Err(fail(BAD_ANSWER));
            }
            Ok(choose(query, &found.hits))
        })
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

fn form(kind: MediaKind) -> Option<Form> {
    match kind {
        MediaKind::Movie => Some(Form::Movie),
        MediaKind::Tv => Some(Form::Tv),
        _ => None,
    }
}

/// A token goes into a header, so only the characters a JWT or bearer
/// token has: ASCII letters, digits and `-`, `_`, `.`.
fn valid_token(t: &str) -> bool {
    (1..=MAX_TOKEN).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The search for `query`: its kind picks the path, and its title and
/// year are the only item data in it, each one encoded parameter. Adult
/// results are never asked for.
pub fn search_request(query: &Query, token: &Secret) -> Result<Request, PluginError> {
    let (path, year_param) = match query.kind() {
        MediaKind::Movie => ("movie", "primary_release_year"),
        MediaKind::Tv => ("tv", "first_air_date_year"),
        _ => return Err(fail(BAD_URL)),
    };
    let mut url =
        url::Url::parse(&format!("https://{HOST}/3/search/{path}")).map_err(|_| fail(BAD_URL))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("query", query.title())
            .append_pair("include_adult", "false")
            .append_pair("language", "en-US")
            .append_pair("page", "1");
        if let Some(y) = query.year() {
            q.append_pair(year_param, &y.to_string());
        }
    }
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", token.expose())))
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

/// The one result that agrees with the query: not marked adult, its title
/// or original title equal to the query's (compared by [`fold`]), and,
/// when the query has a year, that year. None, or more than one, is no
/// match.
pub fn choose(query: &Query, hits: &[Hit]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let mut agree = hits.iter().filter(|h| {
        h.adult != Some(true)
            && [&h.title, &h.original_title]
                .into_iter()
                .flatten()
                .any(|t| fold(t) == want)
            && query.year().is_none_or(|y| h.year == Some(y))
    });
    match (agree.next(), agree.next()) {
        (Some(h), None) => Lookup::Match(identification(query, h)),
        _ => Lookup::NoMatch,
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

/// The answer for `h`: `tmdb:<id>`, its English title as the canonical
/// one, the original title (with its language) as an alternative when it
/// differs, its genres by name, and the year.
fn identification(query: &Query, h: &Hit) -> Identification {
    let canonical = h.title.clone().or_else(|| h.original_title.clone());
    let alt_titles = match (&h.original_title, &h.original_language) {
        (Some(t), Some(l)) if canonical.as_ref() != Some(t) => vec![AltTitle {
            language: l.clone(),
            title: t.clone(),
        }],
        _ => Vec::new(),
    };
    let mut genres: Vec<String> = Vec::new();
    for g in h.genre_ids.iter().filter_map(|&g| genre(query.kind(), g)) {
        if !genres.iter().any(|x| x == g) {
            genres.push(g.to_owned());
        }
    }
    Identification {
        ids: vec![ExternalId {
            scheme: "tmdb".into(),
            value: h.id.to_string(),
        }],
        canonical_title: canonical,
        alt_titles,
        genres,
        year: query.year().or(h.year),
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
        // TMDB's own marker, passed on: the runtime refuses an answer
        // without one (`TERMS`).
        adult: h.adult,
    }
}

/// TMDB's fixed genre ids (`/3/genre/movie/list`, `/3/genre/tv/list`), by
/// name. An id not listed is dropped, so no text from the answer is used.
pub fn genre(kind: MediaKind, id: u32) -> Option<&'static str> {
    let shared = match id {
        16 => Some("Animation"),
        35 => Some("Comedy"),
        80 => Some("Crime"),
        99 => Some("Documentary"),
        18 => Some("Drama"),
        10751 => Some("Family"),
        9648 => Some("Mystery"),
        37 => Some("Western"),
        _ => None,
    };
    shared.or(match (kind, id) {
        (MediaKind::Movie, 28) => Some("Action"),
        (MediaKind::Movie, 12) => Some("Adventure"),
        (MediaKind::Movie, 14) => Some("Fantasy"),
        (MediaKind::Movie, 36) => Some("History"),
        (MediaKind::Movie, 27) => Some("Horror"),
        (MediaKind::Movie, 10402) => Some("Music"),
        (MediaKind::Movie, 10749) => Some("Romance"),
        (MediaKind::Movie, 878) => Some("Science Fiction"),
        (MediaKind::Movie, 10770) => Some("TV Movie"),
        (MediaKind::Movie, 53) => Some("Thriller"),
        (MediaKind::Movie, 10752) => Some("War"),
        (MediaKind::Tv, 10759) => Some("Action & Adventure"),
        (MediaKind::Tv, 10762) => Some("Kids"),
        (MediaKind::Tv, 10763) => Some("News"),
        (MediaKind::Tv, 10764) => Some("Reality"),
        (MediaKind::Tv, 10765) => Some("Sci-Fi & Fantasy"),
        (MediaKind::Tv, 10766) => Some("Soap"),
        (MediaKind::Tv, 10767) => Some("Talk"),
        (MediaKind::Tv, 10768) => Some("War & Politics"),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_header_safe() {
        assert!(valid_token("abc.DEF-123_x"));
        for bad in ["", "a b", "a\r\nX: y", "a:b", "é"] {
            assert!(!valid_token(bad), "{bad:?}");
        }
        assert!(!valid_token(&"a".repeat(MAX_TOKEN + 1)));
    }

    #[test]
    fn genres_are_by_kind() {
        assert_eq!(genre(MediaKind::Movie, 878), Some("Science Fiction"));
        assert_eq!(genre(MediaKind::Tv, 878), None);
        assert_eq!(genre(MediaKind::Tv, 10765), Some("Sci-Fi & Fantasy"));
        assert_eq!(genre(MediaKind::Movie, 18), Some("Drama"));
        assert_eq!(genre(MediaKind::Movie, 1), None);
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;

    /// Opened items only, kept in Lodestar's own UI, its required notice,
    /// claims dropped before six months, and its marker required.
    #[test]
    fn tmdb_terms() {
        let t = Tmdb::new(Arc::new(crate::StaticCredentials::default()));
        let t = Identifier::terms(&t);
        assert!(!t.background_walk && !t.redistribute && t.adult_marker_required);
        assert_eq!(
            t.retention.max_age,
            std::time::Duration::from_secs(180 * 86_400)
        );
        // Six months is at least 181 days.
        assert!(t.retention.drop_after() < 181 * 86_400);
        let a = t.attribution.unwrap();
        assert_eq!((a.text, a.link), (ATTRIBUTION, ATTRIBUTION_URL));
        assert!(
            a.text
                .ends_with("but is not endorsed, certified, or otherwise approved by TMDB.")
        );
        assert!(t.accepts(Some(false)) && !t.accepts(None) && !t.accepts(Some(true)));
    }
}
