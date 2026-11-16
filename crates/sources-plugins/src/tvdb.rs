//! TheTVDB, a general enrichment plugin for series (ENR-tvdb;
//! architecture §18.2, §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5,
//! Q12; `docs/decisions/2026-11-15-ENR-tvdb.md`). It copies Wikidata's
//! shape (ENR-14) and adds an Account credential.
//!
//! **Off by default, Account class.** Nothing is sent until you store
//! your own TheTVDB API key (and, for a user-supported key, your
//! subscriber PIN) and enable the source (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored.
//!
//! **What leaves the install:** the API key and PIN, to `/v4/login` only
//! (in the request body, over https), and for each lookup the parsed
//! title alone, as one URL parameter of `/v4/search` with the fixed
//! `type=series`. Never the year (it's checked here, on the answer), an
//! info hash, a file name, a tracker or anything about the install.
//!
//! 1. reads the key and PIN from [`Credentials`] on each lookup, so one
//!    removed on purge stops working at once;
//! 2. logs in for a bearer token when it holds none for that key and PIN,
//!    or holds one older than [`TOKEN_TTL`]. The token lives only in this
//!    struct's memory, in a zeroizing [`Secret`], and goes only in an
//!    `Authorization` header: never a log, an error, `Debug` or the store;
//! 3. searches, and picks the one series whose title and year agree with
//!    the query ([`choose`]). None, or more than one, is no match.
//!
//! **A refusal never loops.** A lookup sends at most one login and one
//! search. The host limiter treats a 401 or 403 as the host refusing us
//! and holds every request to it (until the back-off ceiling when the
//! answer names no wait), so nothing is retried within a lookup. A 401 on
//! the search drops the held token, and the first lookup after the hold
//! logs in again: the token is refreshed once. A token refused just after
//! it was issued, or a refused login, is the key itself refused
//! ([`REFUSED`]). Errors read the HTTP status and the parser's report
//! only: the service's text is never kept or put in an error.

use std::fmt;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{AltTitle, ExternalId};
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::http::{Method, parse_retry_after};
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::tvdb::{self, LOGIN_LIMITS, Page, SEARCH_LIMITS, Series, Type};
use tokio::time::Instant;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "tvdb";
/// The only host the plugin may reach.
pub const HOST: &str = "api4.thetvdb.com";
/// The login path.
pub const LOGIN: &str = "/v4/login";
/// The search path.
pub const SEARCH: &str = "/v4/search";
/// The credential that holds the API key.
pub const API_KEY: &str = "api_key";
/// The credential that holds a subscriber PIN (user-supported keys only).
pub const PIN: &str = "pin";
/// Results asked for. The match must be among the top few.
pub const SEARCH_LIMIT: usize = 10;
/// Bytes in an API key. TheTVDB's v4 keys are UUIDs (36 characters).
pub const MAX_KEY: usize = 128;
/// Bytes in a subscriber PIN.
pub const MAX_PIN: usize = 64;
/// How long a token is used before logging in again. TheTVDB's last a
/// month; renewing well before keeps a lookup from finding it expired.
pub const TOKEN_TTL: Duration = Duration::from_secs(25 * 86_400);

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 90;
/// The title agrees and the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 70;

/// TheTVDB's attribution, which its API terms require wherever its data
/// is shown, with a link to [`ATTRIBUTION_URL`]. The manifest has no
/// field for it yet (see the decisions file).
pub const ATTRIBUTION: &str =
    "Metadata provided by TheTVDB. Please consider adding missing information or subscribing.";
/// Where the attribution links.
pub const ATTRIBUTION_URL: &str = "https://thetvdb.com/subscribe";

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "TheTVDB",
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

/// The settings form (§10.10): none of its own. The key and PIN are
/// credentials the policy declares, stored through the credential store
/// (SRC-4) and never returned.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// TheTVDB publishes no number, only "no excessive calls": the plan's
/// cap of one request a second (§2.26), no burst. A lookup is one search,
/// and a login about once a month. Back-off after a 429 or 5xx: a minute
/// up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    credentials: &[
        CredentialSpec {
            name: API_KEY,
            label: "API key",
            kind: CredentialKind::Secret,
            required: true,
        },
        CredentialSpec {
            name: PIN,
            label: "Subscriber PIN (user-supported keys only)",
            kind: CredentialKind::Secret,
            required: false,
        },
    ],
    ..Policy::STRICT
};

/// The least time between two lookups: one search at one a second.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The key was refused (401 or 403). Names no key.
pub const REFUSED: &str = "TheTVDB refused the API key";
/// The held token was refused (401): the next lookup logs in again.
pub const TOKEN_REFUSED: &str = "TheTVDB refused the session; logging in again on the next lookup";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "TheTVDB is limiting requests; backing off";
/// The host is held after a refusal, a rate limit, a server error or an
/// API error.
pub const HELD: &str = "TheTVDB requests are on hold; backing off";
/// The answer was an API error on a 200.
pub const API_ERROR: &str = "TheTVDB answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
/// No API key is stored.
pub const NO_KEY: &str = "needs setup: no TheTVDB API key is stored";
/// The stored key or PIN has characters a key can't have.
pub const BAD_KEY: &str = "the stored TheTVDB API key or PIN isn't valid";
/// The credential store couldn't be read.
pub const UNREADABLE_KEY: &str = "the stored TheTVDB API key couldn't be read";
/// The answer wasn't understood.
pub const BAD_ANSWER: &str = "TheTVDB's answer wasn't understood";
/// The request failed.
pub const FAILED: &str = "TheTVDB's request failed";
const BAD_URL: &str = "TheTVDB's address didn't build";

/// A token, the fingerprint of the key and PIN it was issued for, and
/// when.
struct Token {
    secret: Secret,
    of: u64,
    at: Instant,
}

/// What the plugin remembers between lookups, in memory only.
#[derive(Default)]
struct State {
    token: Option<Token>,
    /// The wait after an API error.
    not_before: Option<Instant>,
}

/// The TheTVDB plugin.
pub struct Tvdb {
    credentials: Arc<dyn Credentials>,
    state: Mutex<State>,
    /// Keys the fingerprints, at random per process, so a fingerprint
    /// says nothing about the key outside this struct.
    hasher: RandomState,
}

/// Names nothing: no key, PIN or token.
impl fmt::Debug for Tvdb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tvdb").finish_non_exhaustive()
    }
}

impl Tvdb {
    pub fn new(credentials: Arc<dyn Credentials>) -> Tvdb {
        Tvdb {
            credentials,
            state: Mutex::default(),
            hasher: RandomState::new(),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs, given the names of the
    /// credentials stored for it: the API key (Account class).
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need_with(&MANIFEST, POLICY.credentials, None, stored)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fingerprint(&self, key: &Secret, pin: Option<&Secret>) -> u64 {
        let mut h = self.hasher.build_hasher();
        h.write(key.expose().as_bytes());
        h.write_u8(0xff);
        if let Some(p) = pin {
            h.write(p.expose().as_bytes());
        }
        h.finish()
    }
}

/// TheTVDB's terms (ENR-terms; `docs/decisions/2026-11-15-ENR-tvdb.md`).
/// Its API is licensed per product, solely for that product's use, and
/// its terms forbid "excessive calls" and distributing "the API or the
/// Data", so: no whole-index walk, only the items you open, and its
/// answers stay in Lodestar's own UI. They ask for no shorter keeping than
/// the default. The API page requires the attribution with a link to
/// TheTVDB wherever its data is shown.
pub const TERMS: Terms = Terms {
    background_walk: false,
    redistribute: false,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: ATTRIBUTION,
        link: ATTRIBUTION_URL,
        logo: None,
    }),
    // TheTVDB's search results carry no adult marker (its v4 API has no
    // such field and no filter for one). The runtime's gate never asks
    // about an Adult item, and the plugin's kind is TV only.
    adult_marker_required: false,
};

impl Identifier for Tvdb {
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
        &[MediaKind::Tv]
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
            // A title of nothing but punctuation can name no series, and
            // only series are asked about: nothing is sent.
            if query.kind() != MediaKind::Tv || fold(query.title()).is_empty() {
                return Ok(Lookup::NoMatch);
            }
            self.not_held()?;
            // Both read, the PIN even when absent, so what's read is what
            // the policy declares.
            let key = self.credentials.get(ID, API_KEY).await;
            let pin = self.credentials.get(ID, PIN).await;
            if !matches!(key, Ok(Some(_))) {
                // The key was removed (purge) or can't be read: the token
                // issued for it goes too.
                self.state().token = None;
            }
            let key = key
                .map_err(|_| fail(UNREADABLE_KEY))?
                .ok_or_else(|| fail(NO_KEY))?;
            let pin = pin
                .map_err(|_| fail(UNREADABLE_KEY))?
                .filter(|p| !p.is_empty());
            if !valid_key(key.expose()) || pin.as_ref().is_some_and(|p| !valid_pin(p.expose())) {
                return Err(fail(BAD_KEY));
            }
            let of = self.fingerprint(&key, pin.as_ref());
            let request = search_request(query.title())?;
            let (token, fresh) = match self.cached(of) {
                Some(t) => (t, false),
                None => (self.login(kit, &key, pin.as_ref(), of).await?, true),
            };
            let res = answer(kit.http.send(&authorized(&request, &token)).await)?;
            drop(token);
            match res.status {
                200 => {}
                // The token was refused: dropped, so the next lookup (once
                // the host limiter's hold ends) logs in again. One just
                // issued being refused is the key refused.
                401 => {
                    self.state().token = None;
                    return Err(fail(if fresh { REFUSED } else { TOKEN_REFUSED }));
                }
                s => return Err(status_error(s)),
            }
            let found = tvdb::parse_search(&res.body, &SEARCH_LIMITS);
            self.usable(&found.page, &res)?;
            Ok(choose(query, &found.results))
        })
    }
}

impl Tvdb {
    /// Fails at once, sending nothing, while an API error's wait lasts.
    fn not_held(&self) -> Result<(), PluginError> {
        match self.state().not_before {
            Some(t) if Instant::now() < t => Err(fail(HELD)),
            _ => Ok(()),
        }
    }

    /// A copy of the token issued for `of`, unless it's older than
    /// [`TOKEN_TTL`].
    fn cached(&self, of: u64) -> Option<Secret> {
        let st = self.state();
        st.token
            .as_ref()
            .filter(|t| t.of == of && t.at.elapsed() < TOKEN_TTL)
            .map(|t| Secret::new(t.secret.expose()))
    }

    /// Exchanges the key (and PIN) for a token, and keeps it in memory.
    async fn login(
        &self,
        kit: &Kit,
        key: &Secret,
        pin: Option<&Secret>,
        of: u64,
    ) -> Result<Secret, PluginError> {
        self.state().token = None;
        let res = answer(kit.http.send(&login_request(key, pin)?).await)?;
        if res.status != 200 {
            return Err(status_error(res.status));
        }
        let got = tvdb::parse_login(&res.body, &LOGIN_LIMITS);
        self.usable(&got.page, &res)?;
        let token = got.token.ok_or_else(|| fail(BAD_ANSWER))?;
        let secret = Secret::new(token.into_inner());
        self.state().token = Some(Token {
            secret: Secret::new(secret.expose()),
            of,
            at: Instant::now(),
        });
        Ok(secret)
    }

    /// Only a whole answer is used, as a match or a miss
    /// ([`Page::whole`]), so a broken one is never cached as a miss. An
    /// API error on a 200 (which the host limiter doesn't see) holds the
    /// plugin for its `Retry-After`, or [`API_ERROR_WAIT`], at most
    /// [`MAX_API_ERROR_WAIT`].
    fn usable(&self, page: &Page, res: &Response) -> Result<(), PluginError> {
        if page.failed {
            let wait = res
                .header("retry-after")
                .and_then(parse_retry_after)
                .unwrap_or(API_ERROR_WAIT)
                .clamp(MIN_INTERVAL, MAX_API_ERROR_WAIT);
            self.state().not_before = Some(Instant::now() + wait);
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

/// The response, or the transport's error.
fn answer(res: Result<Response, HttpError>) -> Result<Response, PluginError> {
    match res {
        Ok(r) => Ok(r),
        // The limiter holds the host after a 429 or 5xx; the retry waits.
        Err(HttpError::Blocked { .. }) => Err(fail(HELD)),
        Err(_) => Err(fail(FAILED)),
    }
}

fn status_error(status: u16) -> PluginError {
    match status {
        401 | 403 => fail(REFUSED),
        429 => fail(RATE_LIMITED),
        _ => fail(FAILED),
    }
}

/// An API key goes into a JSON body, so only what a key has: ASCII
/// letters, digits and `-`.
fn valid_key(k: &str) -> bool {
    (1..=MAX_KEY).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A subscriber PIN: ASCII letters and digits.
fn valid_pin(p: &str) -> bool {
    (1..=MAX_PIN).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The login request. The key and PIN, already checked to be letters,
/// digits and `-`, are the body's only values; no item data.
pub fn login_request(key: &Secret, pin: Option<&Secret>) -> Result<Request, PluginError> {
    if !valid_key(key.expose()) || pin.is_some_and(|p| !valid_pin(p.expose())) {
        return Err(fail(BAD_KEY));
    }
    let body = match pin {
        Some(p) => format!(r#"{{"apikey":"{}","pin":"{}"}}"#, key.expose(), p.expose()),
        None => format!(r#"{{"apikey":"{}"}}"#, key.expose()),
    };
    Ok(Request {
        method: Method::Post,
        url: format!("https://{HOST}{LOGIN}"),
        headers: vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/json".into()),
        ],
        body: body.into_bytes(),
    })
}

/// The search for `title`, without its token. The title is the only item
/// data in it, as one encoded parameter.
pub fn search_request(title: &str) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}{SEARCH}")).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .append_pair("query", title)
        .append_pair("type", "series")
        .append_pair("limit", &SEARCH_LIMIT.to_string());
    Ok(Request::get(url.as_str()).header("Accept", "application/json"))
}

/// `request` with the bearer token in its `Authorization` header.
fn authorized(request: &Request, token: &Secret) -> Request {
    request
        .clone()
        .header("Authorization", format!("Bearer {}", token.expose()))
}

/// The one series that agrees with the query: a series, a title (name,
/// alias or translation) equal to the query's (compared by [`fold`]),
/// and, when the query has a year, that year among its years. None, or
/// more than one, is no match.
pub fn choose(query: &Query, results: &[Series]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() || query.kind() != MediaKind::Tv {
        return Lookup::NoMatch;
    }
    let mut agree = results.iter().filter(|s| {
        s.kind == Type::Series
            && s.titles().any(|t| fold(t) == want)
            && query.year().is_none_or(|y| s.years.contains(&y))
    });
    let one = agree.next();
    // The same series listed twice is still one.
    let other = agree.find(|s| one.is_some_and(|o| o.tvdb_id != s.tvdb_id));
    match (one, other) {
        (Some(s), None) => Lookup::Match(identification(query, s)),
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

/// ISO 639-2 codes TheTVDB writes, as the BCP 47 tag stored (the two
/// letters where there are two). Any other code is kept as written: a
/// three-letter code is a valid BCP 47 tag.
const LANGUAGES: &[(&str, &str)] = &[
    ("ara", "ar"),
    ("ces", "cs"),
    ("dan", "da"),
    ("deu", "de"),
    ("ell", "el"),
    ("eng", "en"),
    ("fin", "fi"),
    ("fra", "fr"),
    ("heb", "he"),
    ("hin", "hi"),
    ("hun", "hu"),
    ("ita", "it"),
    ("jpn", "ja"),
    ("kor", "ko"),
    ("nld", "nl"),
    ("nor", "no"),
    ("pol", "pl"),
    ("por", "pt"),
    ("ron", "ro"),
    ("rus", "ru"),
    ("spa", "es"),
    ("swe", "sv"),
    ("tha", "th"),
    ("tur", "tr"),
    ("ukr", "uk"),
    ("zho", "zh"),
];

/// The stored tag for a language TheTVDB names.
pub fn language(code: &str) -> String {
    LANGUAGES
        .iter()
        .find(|(three, _)| *three == code)
        .map_or_else(|| code.to_owned(), |(_, two)| (*two).to_owned())
}

/// The answer for `s`: its TVDB id, and its IMDb and TMDB ids only when
/// it holds exactly one of each (two disagreeing values name no one
/// work). The English translation (else the name) is the canonical
/// title; the other titles are alternatives, the name in its primary
/// language and an alias in `und` (not known).
fn identification(query: &Query, s: &Series) -> Identification {
    let mut ids = vec![id("tvdb", s.tvdb_id.clone())];
    let mut one = |scheme: &str, values: &[String]| {
        if let [v] = dedup(values).as_slice() {
            ids.push(id(scheme, (*v).clone()));
        }
    };
    one("imdb", &s.imdb);
    one("tmdb", &s.tmdb);
    let canonical = s
        .translations
        .iter()
        .find(|t| t.language == "eng")
        .map(|t| t.value.clone())
        .or_else(|| s.name.clone());
    let name_language = s
        .primary_language
        .as_deref()
        .map_or_else(|| "und".to_owned(), language);
    let mut alt_titles: Vec<AltTitle> = Vec::new();
    let mut push = |language: String, title: &str| {
        if canonical.as_deref() != Some(title)
            && !alt_titles
                .iter()
                .any(|a| a.title == title && a.language == language)
        {
            alt_titles.push(AltTitle {
                language,
                title: title.to_owned(),
            });
        }
    };
    if let Some(n) = &s.name {
        push(name_language, n);
    }
    for t in &s.translations {
        push(language(&t.language), &t.value);
    }
    for a in &s.aliases {
        push("und".to_owned(), a);
    }
    let year = query.year().or_else(|| s.years.iter().min().copied());
    Identification {
        ids,
        canonical_title: canonical,
        alt_titles,
        genres: s.genres.clone(),
        year,
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
        // TheTVDB doesn't mark adult series; `TERMS` opts out.
        adult: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_pins_are_checked() {
        assert!(valid_key("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"));
        for bad in ["", "a\"b", "a b", "a,\"x\":\"y", &"a".repeat(MAX_KEY + 1)] {
            assert!(!valid_key(bad), "{bad}");
        }
        assert!(valid_pin("ABCD1234") && !valid_pin("AB-CD") && !valid_pin(""));
    }

    #[test]
    fn the_login_body_is_fixed_json() {
        let key = Secret::new("fake-key-0000");
        let r = login_request(&key, None).unwrap();
        assert_eq!(r.body, br#"{"apikey":"fake-key-0000"}"#);
        let pin = Secret::new("FAKEPIN1");
        let r = login_request(&key, Some(&pin)).unwrap();
        assert_eq!(r.body, br#"{"apikey":"fake-key-0000","pin":"FAKEPIN1"}"#);
        assert!(login_request(&Secret::new("x\",\"pin\":\"1"), None).is_err());
        // Debug shows the body's length, never the key.
        assert!(!format!("{r:?}").contains("fake-key"));
    }

    #[test]
    fn languages_map_to_stored_tags() {
        assert_eq!(language("eng"), "en");
        assert_eq!(language("jpn"), "ja");
        assert_eq!(language("yue"), "yue");
        for (three, two) in LANGUAGES {
            assert!(lodestar_sources::claims::language_ok(two), "{three}");
        }
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;

    /// Licensed for the product only: opened items only, kept in
    /// Lodestar's own UI, with the attribution its API page requires, no
    /// marker to read.
    #[test]
    fn tvdb_terms() {
        let t = Tvdb::new(Arc::new(crate::StaticCredentials::default()));
        let t = Identifier::terms(&t);
        assert!(!t.background_walk && !t.redistribute && !t.adult_marker_required);
        assert_eq!(t.retention, Retention::DEFAULT);
        let a = t.attribution.unwrap();
        assert_eq!((a.text, a.link), (ATTRIBUTION, ATTRIBUTION_URL));
        assert!(a.text.starts_with("Metadata provided by TheTVDB"));
        assert!(t.accepts(None) && !t.accepts(Some(true)));
    }
}
