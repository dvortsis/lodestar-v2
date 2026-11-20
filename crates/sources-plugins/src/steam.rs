//! Steam, a games enrichment plugin (ENR-steam; architecture §18.1,
//! §18.2, §18.7; `docs/decisions/2026-11-16-ENR-steam.md`).
//!
//! **No title ever leaves the install** (owner). The plugin fetches
//! Steam's whole app list, page by page (`IStoreService/GetAppList/v1`,
//! games only), folds each page into a local index of names and drops the
//! page, then answers every [`Query`] from that index. A request carries
//! the paging cursor and fixed parameters only, never anything from an
//! item. The list is fetched again at most once per [`REFRESH`]; a failed
//! fetch keeps the old index and waits [`RETRY`] before trying again (a
//! cancelled one waits [`CANCELLED`]). A finished list that is empty, or
//! much smaller than the one in use, is refused: the old index stays, a
//! plain warning is kept as the plugin's status, and the next try waits
//! [`RETRY`] (see [`suspicious`]).
//!
//! **Account class, off by default.** The list needs a Steam Web API key,
//! read from the per-source store on each refresh and sent in the
//! `x-webapi-key` header, never in the URL, so it can't reach a URL log.
//! Errors read the HTTP status and the parser's report only.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::ExternalId;
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Risk,
};
use lodestar_sources_feeds::steam::{self, LIMITS};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "steam";
/// The only host the plugin may reach.
pub const HOST: &str = "api.steampowered.com";
/// The app list's path on it.
pub const API: &str = "/IStoreService/GetAppList/v1/";
/// The credential's name in the store.
pub const API_KEY: &str = "api_key";
/// Apps asked for per page.
pub const PAGE: u32 = 10_000;
/// Pages fetched in one refresh at most: 600,000 apps.
pub const MAX_PAGES: usize = 60;
/// Apps kept in the index at most: about 1.5 times today's games, and
/// about what [`MAX_LIST_BYTES`] holds at ~100 bytes of JSON an app.
pub const MAX_APPS: usize = 300_000;
/// Body bytes read in one refresh at most. Each name is kept twice
/// (folded, which can be up to 1.5 times longer, and as written) plus
/// about 100 B of map overhead per app, so one index is at most about
/// 110 MiB; during a refresh the old index and the partial one coexist,
/// so the peak is about 250 MiB, inside the §3 RSS budget.
pub const MAX_LIST_BYTES: usize = 32 << 20;
/// How long a fetched list is used before it's fetched again.
pub const REFRESH: Duration = Duration::from_secs(24 * 3600);
/// The wait after a failed fetch before the next try.
pub const RETRY: Duration = Duration::from_secs(3600);
/// The wait after a refresh cancelled mid-request (its lookup timed out):
/// short, since nothing failed and the partial list resumes at its cursor.
pub const CANCELLED: Duration = Duration::from_secs(60);
/// Below this many apps in the list in use, a new list is refused only
/// when it's empty: a small list can halve for ordinary reasons.
pub const SHRINK_FLOOR: usize = 1_000;
/// The key in the plugin's state store that holds its warning, if any.
/// Removed once a list is accepted.
pub const WARNING_KEY: &str = "warning";
/// How sure a match is: one game has the title. The list has no years.
pub const CONFIDENCE: u8 = 70;
/// What leaves the install: no title, no item data.
pub const SENDS: &str = "The Steam Web API key and a page cursor (a Steam app id), to fetch Steam's whole list of games; titles are matched on this computer.";
/// The credit line Valve's terms ask for.
pub const ATTRIBUTION: &str = "Data from Steam";
pub const ATTRIBUTION_URL: &str = "https://store.steampowered.com";

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Steam",
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

/// No settings of its own: the key is a declared credential.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// Valve allows 100,000 calls a day; a refresh is about 20 pages a day,
/// one a second at most. Back-off after a 401, 403, 429 or 5xx: a minute
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
    credentials: &[CredentialSpec {
        name: API_KEY,
        label: "Steam Web API key",
        kind: CredentialKind::Secret,
        required: true,
    }],
    ..Policy::STRICT
};

/// Lookups are local; the pacing is the host limiter's.
pub const MIN_INTERVAL: Duration = Duration::from_millis(10);

/// Valve's terms (not re-read live; see the decisions file): the data is used inside
/// Lodestar, credited, and not passed on. The list has no adult marker
/// and does include adult-only games, so an unmarked item can match one;
/// the runtime never asks about an Adult item. Flagged to the owner.
pub const TERMS: Terms = Terms {
    background_walk: true,
    redistribute: false,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: ATTRIBUTION,
        link: ATTRIBUTION_URL,
        logo: None,
    }),
    adult_marker_required: false,
};

/// Steam refused the key (401 or 403). Names no key.
pub const REFUSED: &str = "Steam refused the Web API key";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "Steam is limiting requests; backing off";
/// The host is held, or the last fetch failed and its wait lasts.
pub const HELD: &str = "Steam requests are on hold; backing off";
/// No key is stored.
pub const NO_CREDENTIALS: &str = "needs setup: no Steam Web API key is stored";
/// The stored key isn't 32 hex digits.
pub const BAD_CREDENTIALS: &str = "the stored Steam Web API key isn't valid";
const UNREADABLE: &str = "the stored Steam Web API key couldn't be read";
/// A page wasn't whole, or its cursor didn't move forward.
pub const BAD_ANSWER: &str = "Steam's answer wasn't understood";
/// A refresh passed [`Limits`].
pub const TOO_LONG: &str = "Steam's app list is longer than the plugin keeps";
const FAILED: &str = "Steam's request failed";

/// Why a finished list is refused, as the plain warning the plugin keeps
/// as its status, or `None` when it's accepted. `old` is the number of
/// apps in the list in use (`None` before the first list), `new` the
/// number in the list just fetched.
///
/// Refused: an empty list, always; and, once the list in use holds at
/// least [`SHRINK_FLOOR`] apps, one with fewer than half as many. Steam's
/// catalogue only grows by a few percent a month, so a list half the size
/// is a broken or cut-off answer, not real news.
pub fn suspicious(old: Option<usize>, new: usize) -> Option<String> {
    // "Later", not "in an hour": the line stays while later tries fail
    // for other reasons.
    let retry = "it will be tried again later";
    match old {
        None if new == 0 => Some(format!(
            "Steam's app list came back empty, so no games can be matched yet; {retry}"
        )),
        Some(o) if new == 0 => Some(format!(
            "Steam's app list came back empty; the current list of {o} games is kept and {retry}"
        )),
        Some(o) if o >= SHRINK_FLOOR && new.saturating_mul(2) < o => Some(format!(
            "Steam's app list came back with {new} games, under half the current {o}; the current list is kept and {retry}"
        )),
        _ => None,
    }
}

type Names = HashMap<String, Vec<(u32, String)>>;

/// The bounds on one refresh. [`Limits::DEFAULT`] is what the plugin
/// uses; tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Pages fetched at most.
    pub pages: usize,
    /// Apps kept at most.
    pub apps: usize,
    /// Body bytes read at most.
    pub bytes: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        pages: MAX_PAGES,
        apps: MAX_APPS,
        bytes: MAX_LIST_BYTES,
    };
}

/// A refresh under way: what has been folded so far and where the next
/// page starts. Kept between lookups, so a refresh the runtime cancels
/// (its lookup timed out) picks up at the cursor rather than at page 1,
/// unless it was started more than [`REFRESH`] ago.
struct Partial {
    started: Instant,
    names: Names,
    after: Option<u32>,
    pages: usize,
    apps: usize,
    bytes: usize,
}

/// The local index: folded name to the apps that carry it.
#[derive(Default)]
struct Index {
    names: Names,
    /// Apps in the list in use (counted as fetched, before folding).
    apps: usize,
    fetched: Option<Instant>,
    retry_at: Option<Instant>,
    partial: Option<Partial>,
    /// Why the last finished list was refused, until one is accepted.
    warning: Option<String>,
}

impl Partial {
    fn new() -> Partial {
        Partial {
            started: Instant::now(),
            names: Names::default(),
            after: None,
            pages: 0,
            apps: 0,
            bytes: 0,
        }
    }
}

impl Index {
    /// The refresh under way, started afresh when none is or the one kept
    /// is older than [`REFRESH`].
    fn partial(&mut self) -> &mut Partial {
        let now = Instant::now();
        if self
            .partial
            .as_ref()
            .is_none_or(|p| now >= p.started + REFRESH)
        {
            self.partial = Some(Partial::new());
        }
        self.partial.get_or_insert_with(Partial::new)
    }

    fn stale(&self, now: Instant) -> bool {
        self.fetched.is_none_or(|t| now >= t + REFRESH)
    }

    fn waiting(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|t| now < t)
    }

    fn answer(&self, want: &str) -> Lookup {
        choose(self.names.get(want).map(Vec::as_slice))
    }
}

/// The Steam plugin. It holds no credential; it reads the key on each
/// refresh.
pub struct Steam {
    credentials: Arc<dyn Credentials>,
    limits: Limits,
    /// Held only for short reads and writes, never across a request.
    index: std::sync::Mutex<Index>,
    /// Held by the one lookup that is refreshing.
    refreshing: Mutex<()>,
}

impl fmt::Debug for Steam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Steam").finish_non_exhaustive()
    }
}

impl Steam {
    pub fn new(credentials: Arc<dyn Credentials>) -> Steam {
        Steam::with_limits(credentials, Limits::DEFAULT)
    }

    /// A plugin with other refresh bounds (tests).
    pub fn with_limits(credentials: Arc<dyn Credentials>, limits: Limits) -> Steam {
        Steam {
            credentials,
            limits,
            index: std::sync::Mutex::new(Index::default()),
            refreshing: Mutex::new(()),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5).
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs (Account class).
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need_with(&MANIFEST, POLICY.credentials, None, stored)
    }

    fn index(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The plugin's warning, if the last finished list was refused (see
    /// [`suspicious`]). The same text is kept in the plugin's state under
    /// [`WARNING_KEY`], for the status page.
    pub fn warning(&self) -> Option<String> {
        self.index().warning.clone()
    }

    /// Runs (or resumes) a refresh. The caller holds `refreshing`. The
    /// old index answers lookups until the new list is complete; on an
    /// error it stays, the partial list is dropped and the next try
    /// waits [`RETRY`]. A missing or bad key fails at once and sets no
    /// wait: nothing was sent, and a key stored a moment later is used on
    /// the next lookup. A finished list [`suspicious`] refuses is treated
    /// as an error, and its warning is kept until a list is accepted.
    async fn refresh(&self, kit: &Kit) -> Result<(), PluginError> {
        let key = match self.credentials.get(ID, API_KEY).await {
            Ok(Some(k)) => k,
            Ok(None) => return Err(fail(NO_CREDENTIALS)),
            Err(_) => return Err(fail(UNREADABLE)),
        };
        if !valid_key(key.expose()) {
            return Err(fail(BAD_CREDENTIALS));
        }
        let r = self.fetch(kit, &key).await;
        // What to do with the status in the state store, once the index
        // lock is let go: `None` clears it, `Some(w)` sets it.
        let (r, status) = {
            let mut index = self.index();
            match r {
                Ok((names, apps)) => match suspicious(index.fetched.map(|_| index.apps), apps) {
                    None => {
                        index.names = names;
                        index.apps = apps;
                        index.fetched = Some(Instant::now());
                        index.retry_at = None;
                        index.warning = None;
                        // Cleared on every accepted list, not only when one
                        // is held in memory: a warning stored before a
                        // restart, or whose clear was lost, goes too.
                        (Ok(()), Some(None))
                    }
                    Some(w) => {
                        index.retry_at = Some(Instant::now() + RETRY);
                        index.warning = Some(w.clone());
                        (Err(fail(&w)), Some(Some(w)))
                    }
                },
                Err(e) => {
                    index.partial = None;
                    index.retry_at = Some(Instant::now() + RETRY);
                    (Err(e), None)
                }
            }
        };
        // Best effort: a failing state store loses only the status line,
        // and the next accepted list clears it again.
        match status {
            Some(Some(w)) => {
                let _ = kit.state.set(WARNING_KEY, w.into_bytes()).await;
            }
            Some(None) => {
                let _ = kit.state.remove(WARNING_KEY).await;
            }
            None => {}
        }
        r
    }

    /// Fetches the rest of the list, from the saved cursor on, and
    /// returns the whole new index and the number of apps in it.
    async fn fetch(&self, kit: &Kit, key: &Secret) -> Result<(Names, usize), PluginError> {
        let lim = self.limits;
        loop {
            let after = {
                let mut index = self.index();
                let part = index.partial();
                if part.pages >= lim.pages {
                    return Err(fail(TOO_LONG));
                }
                let after = part.after;
                // Set before the request: if the runtime cancels this
                // lookup mid-request, the next try waits a short while.
                // A real error replaces it with `RETRY` in `refresh`.
                index.retry_at = Some(Instant::now() + CANCELLED);
                after
            };
            let res = match kit.http.send(&list_request(after, key)).await {
                Ok(r) => r,
                Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
                Err(_) => return Err(fail(FAILED)),
            };
            match res.status {
                200 => {}
                401 | 403 => return Err(fail(REFUSED)),
                429 => return Err(fail(RATE_LIMITED)),
                _ => return Err(fail(FAILED)),
            }
            let size = res.body.len();
            let body = res.body;
            let page = tokio::task::spawn_blocking(move || steam::parse_page(&body, &LIMITS))
                .await
                .map_err(|_| fail(BAD_ANSWER))?;
            if !page.whole() {
                return Err(fail(BAD_ANSWER));
            }
            // Fold the page into the partial list in one step, so a
            // cancel between pages loses nothing.
            let mut index = self.index();
            let part = index.partial();
            part.pages += 1;
            part.bytes = part.bytes.saturating_add(size);
            if part.bytes > lim.bytes {
                return Err(fail(TOO_LONG));
            }
            part.apps = part.apps.saturating_add(page.apps.len());
            if part.apps > lim.apps {
                return Err(fail(TOO_LONG));
            }
            for app in page.apps {
                let f = fold(&app.name);
                if !f.is_empty() {
                    part.names.entry(f).or_default().push((app.id, app.name));
                }
            }
            match (page.more, page.last_appid) {
                (false, _) => {
                    let done = index.partial.take().unwrap_or_else(Partial::new);
                    return Ok((done.names, done.apps));
                }
                // The cursor must move forward, or the list never ends.
                (true, Some(next)) if part.after.is_none_or(|a| next > a) => {
                    part.after = Some(next);
                    // The page is kept: a cancel from here resumes at once.
                    index.retry_at = None;
                }
                _ => return Err(fail(BAD_ANSWER)),
            }
        }
    }
}

impl Identifier for Steam {
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
        &[MediaKind::Game]
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
            if query.kind() != MediaKind::Game || want.is_empty() {
                return Ok(Lookup::NoMatch);
            }
            loop {
                {
                    let index = self.index();
                    let now = Instant::now();
                    let have = index.fetched.is_some();
                    if !index.stale(now) || (have && index.waiting(now)) {
                        return Ok(index.answer(&want));
                    }
                    if index.waiting(now) {
                        return Err(fail(HELD));
                    }
                }
                // One lookup refreshes. While it runs, the others answer
                // from the old index; with none yet, they wait for it and
                // then read the new one, so N lookups make one fetch.
                let guard = match self.refreshing.try_lock() {
                    Ok(g) => g,
                    Err(_) => {
                        {
                            let index = self.index();
                            if index.fetched.is_some() {
                                return Ok(index.answer(&want));
                            }
                        }
                        let _wait = self.refreshing.lock().await;
                        continue;
                    }
                };
                // Another refresh may have finished between the check
                // and the lock.
                {
                    let index = self.index();
                    let now = Instant::now();
                    if !index.stale(now) || index.waiting(now) {
                        continue;
                    }
                }
                let r = self.refresh(kit).await;
                drop(guard);
                let index = self.index();
                return match r {
                    Ok(()) => Ok(index.answer(&want)),
                    Err(_) if index.fetched.is_some() => Ok(index.answer(&want)),
                    Err(e) => Err(e),
                };
            }
        })
    }
}

/// The one game named `want`. None, or more than one, is no match.
pub fn choose(apps: Option<&[(u32, String)]>) -> Lookup {
    let Some([(id, name)]) = apps else {
        return Lookup::NoMatch;
    };
    Lookup::Match(Identification {
        ids: vec![ExternalId {
            scheme: "steam".into(),
            value: id.to_string(),
        }],
        canonical_title: Some(name.clone()),
        alt_titles: Vec::new(),
        genres: Vec::new(),
        // The list has no years: none is claimed.
        year: None,
        confidence: CONFIDENCE,
        adult: None,
    })
}

/// A page of the list, starting after `after`. The key travels in a
/// header; the URL holds fixed parameters and the cursor only.
pub fn list_request(after: Option<u32>, key: &Secret) -> Request {
    let mut url = format!(
        "https://{HOST}{API}?include_games=true&include_dlc=false&include_software=false&include_videos=false&include_hardware=false&max_results={PAGE}"
    );
    if let Some(a) = after {
        url.push_str(&format!("&last_appid={a}"));
    }
    Request::get(url)
        .header("Accept", "application/json")
        .header("x-webapi-key", key.expose())
}

/// A Web API key is 32 hex digits.
fn valid_key(k: &str) -> bool {
    k.len() == 32 && k.bytes().all(|b| b.is_ascii_hexdigit())
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// A title for comparing: the same fold as Wikidata's, so the two can't
/// drift apart.
pub use crate::wikidata::fold;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_holds_no_key_in_url() {
        let k = Secret::new("0123456789abcdef0123456789ABCDEF");
        let r = list_request(Some(42), &k);
        assert!(!r.url.contains("0123456789abcdef"));
        assert!(r.url.ends_with("&last_appid=42"));
        assert!(valid_key(k.expose()) && !valid_key("short"));
    }

    #[test]
    fn steam_terms() {
        let t = Steam::new(Arc::new(crate::StaticCredentials::default()));
        let t = Identifier::terms(&t);
        assert!(t.background_walk && !t.redistribute);
        assert!(t.accepts(None));
    }
}
