//! PreDB, a scene-release enrichment plugin over predb.net's public HTTP
//! API (ENR-predb; architecture §18.2, §18.7; plan
//! `docs/plan/2026-11-05-v07-m7.md` Q4, Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-predb.md`). Built on Wikidata's pattern
//! (ENR-14, `docs/decisions/2026-11-12-ENR-14.md`).
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin answers one [`Query`] of a film
//! or a series, with one request:
//!
//! 1. a search for the query's title [`fold`]ed (lower case, letters,
//!    digits and single spaces) and, when it has one, its year, as one URL
//!    parameter ([`search_request`]); never the torrent's name, hash or
//!    files (owner, v0.7 Q4 and Q5);
//! 2. reads the pre records it returns ([`releases`]), and
//! 3. picks the work they agree on ([`choose`]): the releases whose name
//!    starts with the title and, next, the query's year (or, for a series,
//!    a season). Releases of two different years with no year asked is no
//!    match: it never guesses.
//!
//! **The pre time, section and nuke state** are per release, so they can
//! only be claimed for a torrent by comparing its own name with the
//! returned release names, inside the install ([`pre_for`]). ENR-10's
//! [`Query`] doesn't carry the name and its [`Identification`] has no
//! field for them, so [`Identifier::lookup`] answers with what the query
//! can confirm (the year and the scene's genre) and [`pre_for`] and
//! [`PreClaim`] are ready for the runtime to call locally (the decisions
//! file's follow-up).
//!
//! The request carries a [`USER_AGENT`] naming the project, never the
//! user or the install. Errors read the HTTP status and the parser's
//! report only: the service's text is never kept or put in an error.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::http::parse_retry_after;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::predb::{self, Nuke, Page, PreRelease, SEARCH_LIMITS};
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "predb";
/// The only host the plugin may reach.
pub const HOST: &str = "api.predb.net";
/// The search API's path on it: the root.
pub const API: &str = "/";
/// Records asked for in a search: the API's most. The newest come first,
/// and a title's releases must be among them.
pub const SEARCH_LIMIT: usize = 100;
/// Genres kept from the agreeing releases.
pub const MAX_GENRES: usize = 3;

/// predb.net asks nothing of a client's name; the project names itself
/// anyway: the client, its version and the project's own URL, the same
/// for every install. Nothing about the user, their host or their account.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ") lodestar-sources-plugins"
);

/// What a lookup sends, for its Sources row (UI review 02 §5.5).
pub const SENDS: &str = "The parsed title of a film or a TV series and its year, in \
     lower case with punctuation removed, to api.predb.net. Never the info hash, the \
     torrent's name or its files.";

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 75;
/// The title agrees and the query has no year (or a series' releases name
/// a season, not a year). Lower than the databases': a scene name's title
/// is read from its start, not stated.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 55;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "PreDB",
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

/// predb.net documents 30 requests a minute and blocks an address that
/// overloads it; the owner's cap is at most one a second (v0.7 Q4). The
/// stricter holds: one request every two seconds, with no burst.
/// Back-off after a 429 or 5xx: a minute up to 6 hours.
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

/// The least time between two lookups: one request at 30 a minute.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "PreDB is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "PreDB requests are on hold; backing off";
/// The answer was an error object.
pub const API_ERROR: &str = "PreDB answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "PreDB's answer wasn't understood";
const FAILED: &str = "PreDB's request failed";
const BAD_URL: &str = "PreDB's address didn't build";

/// The PreDB plugin. It holds no credential; its one state is the wait
/// after an API error.
#[derive(Debug, Default)]
pub struct PreDb {
    not_before: Mutex<Option<Instant>>,
}

impl PreDb {
    pub fn new() -> PreDb {
        PreDb::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// The pre records a search for the query finds, for matching inside
    /// the install ([`pre_for`]). Sends the same one request as
    /// [`Identifier::lookup`]; an empty list when the title folds to
    /// nothing or the kind isn't one PreDB names, and nothing is sent.
    pub async fn releases(&self, kit: &Kit, query: &Query) -> Result<Vec<PreRelease>, PluginError> {
        let want = fold(query.title());
        if want.is_empty() || !self.kinds().contains(&query.kind()) {
            return Ok(Vec::new());
        }
        self.not_held()?;
        let res = self.answer(kit.http.send(&search_request(&want, query.year())?).await)?;
        let found = predb::parse_search(&res.body, &SEARCH_LIMITS);
        self.usable(&found.page, &res)?;
        Ok(found.releases)
    }
}

impl Identifier for PreDb {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

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
            let found = self.releases(kit, query).await?;
            let refs: Vec<&PreRelease> = found.iter().collect();
            Ok(choose(query, &refs))
        })
    }
}

impl PreDb {
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

/// The search for `title`, which the caller has already [`fold`]ed, and
/// `year`: the only item data in it, as the one `q` parameter, so no
/// character of it can reach another parameter. Search type, page size
/// and order are fixed.
pub fn search_request(title: &str, year: Option<u16>) -> Result<Request, PluginError> {
    let q = match year {
        Some(y) => format!("{title} {y}"),
        None => title.to_owned(),
    };
    let mut url = url::Url::parse(&format!("https://{HOST}{API}")).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .append_pair("type", "search")
        .append_pair("q", &q)
        .append_pair("limit", &SEARCH_LIMIT.to_string());
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT))
}

/// A title for comparing: lower case, letters and digits only, each run
/// of anything else one space. A release name folds the same way, so its
/// dots, underscores and dashes are spaces.
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

/// What a release name says after the title, when it starts with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// A year, 1900–2099.
    Year(u16),
    /// A season (`s01`) or an episode (`s01e02`).
    Season,
}

/// Whether `name` (a release name) starts with the folded title `want`
/// and, right after it, a year or a season. `None` otherwise, including
/// a longer title that only starts the same way.
pub fn after_title(name: &str, want: &str) -> Option<After> {
    let folded = fold(name);
    let rest = folded.strip_prefix(want)?.strip_prefix(' ')?;
    let next = rest.split(' ').next()?;
    if next.len() == 4 && next.bytes().all(|b| b.is_ascii_digit()) {
        let y: u16 = next.parse().ok()?;
        return (1900..=2099).contains(&y).then_some(After::Year(y));
    }
    let season = next.strip_prefix('s')?;
    let num = |t: &str| (1..=4).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit());
    let ok = match season.split_once('e') {
        Some((s, e)) => num(s) && num(e),
        None => num(season),
    };
    ok.then_some(After::Season)
}

/// The work the releases agree on. A release agrees when its name starts
/// with the query's title and then its year, or, for a series, a season
/// (a series' episodes rarely carry a year). With no year asked, the
/// agreeing releases' years must all be one, or it's no match. No
/// agreeing release is no match.
///
/// The answer: the query's (or the releases') year, and up to
/// [`MAX_GENRES`] of the genres the agreeing releases name. No id: PreDB
/// has none for a work.
pub fn choose(query: &Query, releases: &[&PreRelease]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let tv = query.kind() == MediaKind::Tv;
    let mut years: Vec<u16> = Vec::new();
    let mut genres: Vec<String> = Vec::new();
    let mut any = false;
    for r in releases {
        let ok = match after_title(&r.name, &want) {
            Some(After::Year(y)) => {
                if query.year().is_some_and(|q| q != y) {
                    false
                } else {
                    if !years.contains(&y) {
                        years.push(y);
                    }
                    true
                }
            }
            Some(After::Season) => tv,
            None => false,
        };
        if !ok {
            continue;
        }
        any = true;
        if let Some(g) = &r.genre
            && genres.len() < MAX_GENRES
            && !genres.iter().any(|h| h.eq_ignore_ascii_case(g))
        {
            genres.push(g.clone());
        }
    }
    if !any || (query.year().is_none() && years.len() > 1) {
        return Lookup::NoMatch;
    }
    let year = query.year().or(years.first().copied());
    Lookup::Match(Identification {
        ids: Vec::new(),
        canonical_title: None,
        alt_titles: Vec::new(),
        genres,
        year,
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
    })
}

/// The pre claim for one torrent: what PreDB (and IRC-3's live feed)
/// states about the release it is. Kept simple so both write the same:
/// the release name, its pre time in Unix seconds UTC, its section and
/// its nuke state with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreClaim {
    pub release: String,
    pub pretime: i64,
    pub section: Option<String>,
    pub nuke: Nuke,
    pub reason: Option<String>,
}

/// The pre record for a torrent named `name`, matched inside the install:
/// the one release whose name equals `name` ignoring ASCII case (a
/// torrent of a scene release is named as the release). `None` when none
/// does, when its pre time isn't known, or when two records of that name
/// disagree. `name` is never sent anywhere.
pub fn pre_for(name: &str, releases: &[PreRelease]) -> Option<PreClaim> {
    let name = name.trim();
    predb::release_name(name)?;
    let mut hits = releases
        .iter()
        .filter(|r| r.name.eq_ignore_ascii_case(name));
    let first = hits.next()?;
    if hits.any(|r| r != first) {
        return None;
    }
    Some(PreClaim {
        release: first.name.clone(),
        pretime: first.pretime?,
        section: first.section.clone(),
        nuke: first.nuke,
        reason: first.reason.clone(),
    })
}
