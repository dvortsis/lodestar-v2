//! AniDB, an anime enrichment plugin (ENR-anidb; architecture §18.2,
//! §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q4, Q5, Q12; owner §2.47;
//! `docs/decisions/2026-11-16-ENR-anidb.md`). Built on Wikidata's pattern
//! (ENR-14).
//!
//! **Off by default, Account class.** AniDB answers only a registered
//! client, so nothing is sent until you store your registered client's
//! name and version and enable the source
//! ([`CLIENT`], [`CLIENT_VERSION`]). The project has no registered client
//! of its own: none is built in.
//!
//! **No title ever leaves the install.** AniDB's HTTP API can't search by
//! title. Its titles dump can, and AniDB publishes it to be matched
//! locally:
//!
//! 1. the dump ([`DUMP_URL`]) is fetched at most once every
//!    [`DUMP_EVERY`] (AniDB bans clients that fetch it more often; the
//!    time of the last attempt is kept in the plugin's state, so a
//!    restart doesn't fetch it again), and kept in memory for
//!    [`REFRESH_AFTER`];
//! 2. the query's title is [`fold`]ed and looked up in it. No title, or
//!    one that names more than one anime, is no match, and nothing is
//!    sent;
//! 3. for the one anime it names, its record is fetched by id
//!    ([`record_request`]): the id, the client's name and version, and
//!    nothing else. At most [`DAILY_RECORDS`] a UTC day, one every
//!    [`MIN_INTERVAL`], each id at most once in [`RECORD_TTL`];
//! 4. the record must agree ([`choose`]): its type with the query's kind,
//!    a title with the query's, and its years with the query's year.
//!
//! Errors read the HTTP status and the parser's report only: the
//! service's text is never kept or put in an error.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lodestar_sources::claims::{AltTitle, ExternalId, language_ok};
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::anidb::{
    self, ANIME_LIMITS, Aid, Anime, AnimeType, ApiError, DUMP_LIMITS, TitleKind,
};
use tokio::time::Instant;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "anidb";
/// The titles dump: every anime's id and titles, gzip XML.
pub const DUMP_URL: &str = "https://anidb.net/api/anime-titles.xml.gz";
/// The HTTP API. AniDB serves it over plain HTTP on port 9001 only.
pub const API_URL: &str = "http://api.anidb.net:9001/httpapi";
/// The credential that holds your registered client's name.
pub const CLIENT: &str = "client";
/// The credential that holds your registered client's version.
pub const CLIENT_VERSION: &str = "clientver";
/// Bytes in a client name.
pub const MAX_CLIENT: usize = 32;

/// Exactly what leaves the install, for the Sources row (plain text).
pub const SENDS: &str = "Once a day at most, a download of AniDB's public titles list. \
Then, for an anime title you open that the list names, one request with that anime's \
AniDB id and your registered client's name and version. No title, hash, file name or \
anything about you is sent. AniDB's API is plain HTTP, so the id and client name are \
not encrypted in transit.";
/// The credit AniDB's data carries.
pub const ATTRIBUTION: &str = "AniDB";
pub const ATTRIBUTION_URL: &str = "https://anidb.net";

/// The least time between two dump downloads, kept across restarts.
pub const DUMP_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How long a downloaded dump is used before it's fetched again.
pub const REFRESH_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
/// Records fetched a UTC day, kept across restarts.
pub const DAILY_RECORDS: u32 = 100;
/// How long a fetched record answers again without a request.
pub const RECORD_TTL: Duration = Duration::from_secs(24 * 3600);
/// Records kept in memory.
pub const RECORD_CACHE: usize = 256;
/// The least time between two requests and two lookups: the plan's cap.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// How sure a match is: title, kind and year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 85;
/// Title and kind agree; the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 65;

/// State keys (never under the runtime's `enr10/`).
pub const STATE_DUMP_AT: &str = "anidb/dump_at";
pub const STATE_DAY: &str = "anidb/day";

/// The User-Agent: the project, never the user or the install.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ") lodestar-sources-plugins"
);

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "AniDB",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[
        Endpoint {
            protocol: "https",
            host: "anidb.net",
        },
        Endpoint {
            protocol: "http",
            host: "api.anidb.net:9001",
        },
    ],
    risk: Risk::Medium,
    consent: Consent::Account,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// The settings form: none of its own. The client's name and version are
/// the credentials the policy declares, stored through the credential
/// store and never returned.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// One request every 2 s per host, no burst (the plan's cap, §2.26);
/// back-off after a 429 or 5xx from 10 minutes up to 6 hours, since AniDB
/// bans clients that keep asking.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::every(MIN_INTERVAL),
        backoff: Backoff {
            base: Duration::from_secs(600),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    credentials: &[
        CredentialSpec {
            name: CLIENT,
            label: "Registered AniDB client name",
            kind: CredentialKind::Username,
            required: true,
        },
        CredentialSpec {
            name: CLIENT_VERSION,
            label: "Registered AniDB client version",
            kind: CredentialKind::Username,
            required: true,
        },
    ],
    ..Policy::STRICT
};

/// AniDB's terms (ENR-terms; checked 2026-11-16, the decisions file):
/// - **no background walk:** its API rules forbid using the API to
///   download AniDB and ban clients that do, and the owner's Q4 allows
///   it for user interest only;
/// - **no redistribution:** AniDB grants no licence to pass its data on,
///   so answers stay in Lodestar's own UI;
/// - **the default retention:** its rules ask clients to cache, and set
///   no shorter limit;
/// - **credited** as AniDB, linking its site, the data's source;
/// - **its `restricted` marker is required:** every record carries it,
///   so an unmarked one is refused like a restricted one.
pub const TERMS: Terms = Terms {
    background_walk: false,
    redistribute: false,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: ATTRIBUTION,
        link: ATTRIBUTION_URL,
        logo: None,
    }),
    adult_marker_required: true,
};

pub const NO_CLIENT: &str = "needs setup: no registered AniDB client is stored";
pub const BAD_CLIENT: &str = "the stored AniDB client name or version isn't valid";
const UNREADABLE_CLIENT: &str = "the stored AniDB client couldn't be read";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "AniDB is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "AniDB requests are on hold; backing off";
/// The answer was `<error>Banned</error>`.
pub const BANNED: &str = "AniDB has banned this client for now; waiting a day";
/// Any other `<error>` answer.
pub const API_ERROR: &str = "AniDB answered with an error; trying later";
/// The dump was fetched under [`DUMP_EVERY`] ago and isn't in memory.
pub const WAITING: &str = "AniDB's titles list was fetched less than a day ago; waiting";
/// Today's [`DAILY_RECORDS`] are used.
pub const DAILY_CAP: &str = "AniDB's daily request cap is reached; waiting for tomorrow";
/// The plugin's state couldn't be read or written: nothing is sent.
pub const NO_STATE: &str = "AniDB's request record couldn't be kept; nothing was sent";
pub const FAILED: &str = "AniDB's request failed";
pub const BAD_ANSWER: &str = "AniDB's answer wasn't understood";
/// The wait after `<error>Banned</error>`.
pub const BANNED_WAIT: Duration = Duration::from_secs(24 * 3600);
/// The wait after any other `<error>`.
pub const API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_URL: &str = "AniDB's address didn't build";

/// Folded title → the anime it names, or [`AMBIGUOUS`].
#[derive(Debug, Default)]
struct Index {
    titles: HashMap<String, u32>,
    built_at: i64,
}

/// A folded title more than one anime has.
const AMBIGUOUS: u32 = 0;

/// The wall clock in Unix seconds, for the dump and day records that
/// outlive a restart.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The AniDB plugin.
pub struct AniDb {
    credentials: Arc<dyn Credentials>,
    clock: Clock,
    index: tokio::sync::Mutex<Option<Arc<Index>>>,
    records: Mutex<HashMap<u32, (Instant, Anime)>>,
    not_before: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for AniDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AniDb").finish_non_exhaustive()
    }
}

fn system_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl AniDb {
    pub fn new(credentials: Arc<dyn Credentials>) -> AniDb {
        AniDb::with_clock(credentials, Arc::new(system_now))
    }

    /// With `clock` for the wall clock (tests).
    pub fn with_clock(credentials: Arc<dyn Credentials>, clock: Clock) -> AniDb {
        AniDb {
            credentials,
            clock,
            index: tokio::sync::Mutex::new(None),
            records: Mutex::new(HashMap::new()),
            not_before: Mutex::new(None),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the dump's
    /// host over https and the API's host and port over http.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs, given the names of the
    /// credentials stored for it: both declared ones, never one alone.
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need_with(&MANIFEST, POLICY.credentials, None, stored)
    }
}

impl Identifier for AniDb {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn terms(&self) -> &Terms {
        &TERMS
    }

    /// AniDB holds anime series and films; the runtime has no anime kind
    /// (AniList's choice too). A record whose type disagrees is no match.
    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Movie, MediaKind::Tv]
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
            let want = fold(query.title());
            if want.is_empty() {
                return Ok(Lookup::NoMatch);
            }
            let (client, version) = self.client().await?;
            self.not_held()?;
            let index = self.titles(kit).await?;
            let aid = match index.titles.get(&want) {
                Some(&aid) if aid != AMBIGUOUS => Aid(aid),
                // Not an anime title, or one several share: nothing sent.
                _ => return Ok(Lookup::NoMatch),
            };
            let anime = self.record(kit, aid, &client, version).await?;
            Ok(choose(query, &anime))
        })
    }
}

impl AniDb {
    /// The stored client's name and version, read on each lookup.
    async fn client(&self) -> Result<(String, u16), PluginError> {
        let read = |name| async move {
            self.credentials
                .get(ID, name)
                .await
                .map_err(|_| fail(UNREADABLE_CLIENT))?
                .ok_or_else(|| fail(NO_CLIENT))
        };
        // Both are read before either is judged, so the plugin always
        // reads exactly the credentials it declares.
        let (client, version) = (read(CLIENT).await, read(CLIENT_VERSION).await);
        let (client, version) = (client?, version?);
        let (c, v) = (client.expose(), version.expose());
        if !valid_client(c) {
            return Err(fail(BAD_CLIENT));
        }
        let v = version_of(v).ok_or_else(|| fail(BAD_CLIENT))?;
        Ok((c.to_owned(), v))
    }

    /// Fails at once, sending nothing, while an API error's wait lasts.
    fn not_held(&self) -> Result<(), PluginError> {
        match *lock(&self.not_before) {
            Some(t) if Instant::now() < t => Err(fail(HELD)),
            _ => Ok(()),
        }
    }

    fn hold(&self, wait: Duration) {
        *lock(&self.not_before) = Some(Instant::now() + wait);
    }

    /// The titles index: in memory while under [`REFRESH_AFTER`] old;
    /// otherwise the dump, fetched only when the last attempt (kept in
    /// state) is over [`DUMP_EVERY`] ago. The attempt is recorded before
    /// the request is sent, so a failed or interrupted one counts too.
    async fn titles(&self, kit: &Kit) -> Result<Arc<Index>, PluginError> {
        let mut slot = self.index.lock().await;
        let now = (self.clock)();
        let fresh = |i: &Index| now.saturating_sub(i.built_at) < secs(REFRESH_AFTER);
        if let Some(i) = slot.as_ref().filter(|i| fresh(i)) {
            return Ok(i.clone());
        }
        let last = kit
            .state
            .get(STATE_DUMP_AT)
            .await
            .map_err(|_| fail(NO_STATE))?
            .and_then(|v| std::str::from_utf8(&v).ok()?.parse::<i64>().ok());
        if let Some(at) = last
            && now.saturating_sub(at) < secs(DUMP_EVERY)
        {
            // A record from the future (the clock was set back) restarts
            // the wait from now, so it can't hold the plugin forever.
            if at > now {
                kit.state
                    .set(STATE_DUMP_AT, now.to_string().into_bytes())
                    .await
                    .map_err(|_| fail(NO_STATE))?;
            }
            return slot.clone().ok_or_else(|| fail(WAITING));
        }
        kit.state
            .set(STATE_DUMP_AT, now.to_string().into_bytes())
            .await
            .map_err(|_| fail(NO_STATE))?;
        let req = Request::get(DUMP_URL).header("User-Agent", USER_AGENT);
        let res = self.answer(kit.http.send(&req).await)?;
        let body = res.body;
        let built = tokio::task::spawn_blocking(move || build_index(&body, now))
            .await
            .map_err(|_| fail(BAD_ANSWER))?
            .ok_or_else(|| fail(BAD_ANSWER))?;
        let built = Arc::new(built);
        *slot = Some(built.clone());
        Ok(built)
    }

    /// `aid`'s record: from memory within [`RECORD_TTL`], else one request
    /// within the day's cap.
    async fn record(
        &self,
        kit: &Kit,
        aid: Aid,
        client: &str,
        version: u16,
    ) -> Result<Anime, PluginError> {
        {
            let mut cache = lock(&self.records);
            cache.retain(|_, (at, _)| at.elapsed() < RECORD_TTL);
            if let Some((_, a)) = cache.get(&aid.0) {
                return Ok(a.clone());
            }
        }
        self.count_today(kit).await?;
        let res = self.answer(kit.http.send(&record_request(aid, client, version)?).await)?;
        let body = res.body;
        let (anime, page) =
            tokio::task::spawn_blocking(move || anidb::parse_anime(&body, &ANIME_LIMITS))
                .await
                .map_err(|_| fail(BAD_ANSWER))?;
        match page.error {
            Some(ApiError::Banned) => {
                self.hold(BANNED_WAIT);
                return Err(fail(BANNED));
            }
            Some(ApiError::Other) => {
                self.hold(API_ERROR_WAIT);
                return Err(fail(API_ERROR));
            }
            None => {}
        }
        let anime = anime
            .filter(|a| page.whole() && a.aid == aid)
            .ok_or_else(|| fail(BAD_ANSWER))?;
        let mut cache = lock(&self.records);
        if cache.len() >= RECORD_CACHE {
            cache.clear();
        }
        cache.insert(aid.0, (Instant::now(), anime.clone()));
        Ok(anime)
    }

    /// Counts one record request against today's [`DAILY_RECORDS`] (UTC
    /// days), or refuses it. Kept in state, so a restart doesn't reset it.
    async fn count_today(&self, kit: &Kit) -> Result<(), PluginError> {
        let today = (self.clock)().div_euclid(86_400);
        let old = kit.state.get(STATE_DAY).await.map_err(|_| fail(NO_STATE))?;
        let used = old
            .as_deref()
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.split_once(' '))
            .and_then(|(d, n)| Some((d.parse::<i64>().ok()?, n.parse::<u32>().ok()?)))
            .map_or(0, |(day, n)| if day == today { n } else { 0 });
        if used >= DAILY_RECORDS {
            return Err(fail(DAILY_CAP));
        }
        let new = format!("{today} {}", used + 1).into_bytes();
        let wrote = kit
            .state
            .compare_and_set(STATE_DAY, old.as_deref(), Some(new))
            .await
            .map_err(|_| fail(NO_STATE))?;
        if wrote { Ok(()) } else { Err(fail(NO_STATE)) }
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
            429 => Err(fail(RATE_LIMITED)),
            _ => Err(fail(FAILED)),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// A client name as AniDB registers them: 1–[`MAX_CLIENT`] lower-case
/// ASCII letters and digits. It goes into the URL, so nothing else.
pub fn valid_client(c: &str) -> bool {
    (1..=MAX_CLIENT).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A client version: 1–5 digits, 1–65535, no leading zero.
pub fn version_of(v: &str) -> Option<u16> {
    let ok =
        (1..=5).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit()) && !v.starts_with('0');
    ok.then(|| v.parse().ok().filter(|&n| n > 0)).flatten()
}

/// The record request for `aid`: a fixed template with the id and the
/// client, nothing from the item.
pub fn record_request(aid: Aid, client: &str, version: u16) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(API_URL).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .append_pair("request", "anime")
        .append_pair("client", client)
        .append_pair("clientver", &version.to_string())
        .append_pair("protover", "1")
        .append_pair("aid", &aid.to_string());
    Ok(Request::get(url.as_str())
        .header("Accept-Encoding", "gzip")
        .header("User-Agent", USER_AGENT))
}

/// The index of `body`'s main, official and synonym titles, or `None`
/// unless it's [`whole`](anidb::Page::whole). Short titles (`CotS`) and
/// unknown types are left out: they name too many things.
fn build_index(body: &[u8], now: i64) -> Option<Index> {
    let mut titles: HashMap<String, u32> = HashMap::new();
    let page = anidb::parse_titles_each(body, &DUMP_LIMITS, |a| {
        for t in a.titles.iter().filter(|t| matchable(t.kind)) {
            let key = fold(&t.text);
            if key.is_empty() {
                continue;
            }
            titles
                .entry(key)
                .and_modify(|v| {
                    if *v != a.aid.0 {
                        *v = AMBIGUOUS;
                    }
                })
                .or_insert(a.aid.0);
        }
    });
    page.whole().then_some(Index {
        titles,
        built_at: now,
    })
}

fn matchable(k: TitleKind) -> bool {
    matches!(
        k,
        TitleKind::Main | TitleKind::Official | TitleKind::Synonym
    )
}

/// A title for comparing: lower case, letters and digits only, each run of
/// anything else one space.
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

/// Whether the record's type is the query's kind.
fn kind_agrees(kind: MediaKind, t: Option<AnimeType>) -> bool {
    match kind {
        MediaKind::Movie => t == Some(AnimeType::Movie),
        MediaKind::Tv => matches!(
            t,
            Some(
                AnimeType::TvSeries
                    | AnimeType::TvSpecial
                    | AnimeType::Ova
                    | AnimeType::Web
                    | AnimeType::Other
            )
        ),
        _ => false,
    }
}

/// The match, if the record agrees with the query: its type with the
/// kind, one of its main, official or synonym titles with the title, and,
/// when the query has a year, its start year (or, for a series, a year
/// from its start to its end).
pub fn choose(query: &Query, a: &Anime) -> Lookup {
    let want = fold(query.title());
    let titled = !want.is_empty()
        && a.titles
            .iter()
            .any(|t| matchable(t.kind) && fold(&t.text) == want);
    let year_ok = match (query.year(), a.start_year) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(y), Some(start)) if query.kind() == MediaKind::Tv => {
            (start..=a.end_year.unwrap_or(start).max(start)).contains(&y)
        }
        (Some(y), Some(start)) => y == start,
    };
    if !(titled && year_ok && kind_agrees(query.kind(), a.kind)) {
        return Lookup::NoMatch;
    }
    Lookup::Match(identification(query, a))
}

/// AniDB's language for a title as a stored BCP 47 tag: its own
/// romanisation tags (`x-jat`, `x-zht`, `x-kot`) as the language in Latin
/// script, others lower-cased when well formed, else `und`.
pub fn language(lang: &str) -> String {
    let l = lang.to_ascii_lowercase();
    let mapped = match l.as_str() {
        "x-jat" => "ja-latn",
        "x-zht" => "zh-latn",
        "x-kot" => "ko-latn",
        "x-tht" => "th-latn",
        other => other,
    };
    if language_ok(mapped) {
        mapped.to_owned()
    } else {
        "und".to_owned()
    }
}

fn identification(query: &Query, a: &Anime) -> Identification {
    let canonical = a
        .titles
        .iter()
        .find(|t| t.kind == TitleKind::Main)
        .map(|t| t.text.clone());
    let mut alt_titles: Vec<AltTitle> = Vec::new();
    for t in a
        .titles
        .iter()
        .filter(|t| matches!(t.kind, TitleKind::Official | TitleKind::Synonym))
    {
        let language = language(&t.lang);
        if canonical.as_ref() != Some(&t.text)
            && !alt_titles
                .iter()
                .any(|x| x.title == t.text && x.language == language)
        {
            alt_titles.push(AltTitle {
                language,
                title: t.text.clone(),
            });
        }
    }
    Identification {
        ids: vec![ExternalId {
            scheme: "anidb".into(),
            value: a.aid.to_string(),
        }],
        canonical_title: canonical,
        alt_titles,
        genres: Vec::new(),
        year: query.year().or(a.start_year),
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
        // AniDB's 18+ flag: marked is refused, unmarked too (TERMS).
        adult: a.restricted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_and_versions_are_strict() {
        assert!(valid_client("lodestartest") && valid_client("a1"));
        for bad in ["", "Upper", "a b", "a&b=c", "a-b", &"a".repeat(33)] {
            assert!(!valid_client(bad), "{bad}");
        }
        assert_eq!(version_of("1"), Some(1));
        assert_eq!(version_of("65535"), Some(65535));
        for bad in ["", "0", "01", "65536", "1.0", "-1", "123456"] {
            assert_eq!(version_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn languages_map_to_stored_tags() {
        assert_eq!(language("x-jat"), "ja-latn");
        assert_eq!(language("en"), "en");
        assert_eq!(language("zh-Hans"), "zh-hans");
        assert_eq!(language("x-other"), "und");
        assert_eq!(language(""), "und");
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;

    /// Opened items only (its API rules forbid using the API to download
    /// AniDB), kept in Lodestar's own UI (no licence lets its data be
    /// passed on), the default retention, credited with a link, and its
    /// `restricted` marker required: an unmarked or restricted anime is
    /// refused.
    #[test]
    fn anidb_terms() {
        let p = AniDb::new(Arc::new(crate::StaticCredentials::default()));
        let t = Identifier::terms(&p);
        assert_eq!(t, &TERMS);
        assert!(!t.background_walk && !t.redistribute && t.adult_marker_required);
        assert_eq!(t.retention, Retention::DEFAULT);
        assert_eq!(
            t.attribution.map(|a| (a.text, a.link)),
            Some(("AniDB", "https://anidb.net"))
        );
        assert!(!t.accepts(None) && t.accepts(Some(false)) && !t.accepts(Some(true)));
    }

    /// The egress lets exactly the two addresses through: the dump over
    /// https, the API over http on its port.
    #[test]
    fn only_the_two_endpoints_are_permitted() {
        let e = AniDb::new(Arc::new(crate::StaticCredentials::default())).endpoints();
        let req = record_request(Aid(1), "lodestartest", 1).unwrap();
        assert!(e.permits(DUMP_URL) && e.permits(&req.url));
        for other in [
            "http://anidb.net/api/anime-titles.xml.gz",
            "https://api.anidb.net:9001/httpapi",
            "http://api.anidb.net/httpapi",
            "https://example.org/",
        ] {
            assert!(!e.permits(other), "{other}");
        }
    }
}
