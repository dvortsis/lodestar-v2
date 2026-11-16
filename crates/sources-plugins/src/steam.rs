//! Steam, a games enrichment plugin (ENR-steam; architecture §18.1,
//! §18.2, §18.7; `docs/decisions/2026-11-16-ENR-steam.md`).
//!
//! **No title ever leaves the install** (owner). The plugin fetches
//! Steam's whole app list, page by page (`IStoreService/GetAppList/v1`,
//! games only), folds each page into a local index of names and drops the
//! page, then answers every [`Query`] from that index. A request carries
//! the paging cursor and fixed parameters only, never anything from an
//! item. The list is fetched again at most once per [`REFRESH`]; a failed
//! fetch keeps the old index and waits [`RETRY`] before trying again.
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
/// Apps kept in the index at most.
pub const MAX_APPS: usize = 500_000;
/// Body bytes read in one refresh at most, so the index (names kept
/// twice: folded and as written) stays under about 200 MiB even when
/// every name is at its cap.
pub const MAX_LIST_BYTES: usize = 96 << 20;
/// How long a fetched list is used before it's fetched again.
pub const REFRESH: Duration = Duration::from_secs(24 * 3600);
/// The wait after a failed fetch before the next try.
pub const RETRY: Duration = Duration::from_secs(3600);
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

/// Valve's terms (checked 2026-11-16): the data is used inside
/// Lodestar, credited, and not passed on. The list has no adult marker;
/// the runtime never asks about an Adult item.
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
const BAD_CREDENTIALS: &str = "the stored Steam Web API key isn't valid";
const UNREADABLE: &str = "the stored Steam Web API key couldn't be read";
const BAD_ANSWER: &str = "Steam's answer wasn't understood";
const TOO_LONG: &str = "Steam's app list is longer than the plugin keeps";
const FAILED: &str = "Steam's request failed";

/// The local index: folded name to the apps that carry it.
#[derive(Default)]
struct Index {
    names: HashMap<String, Vec<(u32, String)>>,
    fetched: Option<Instant>,
    retry_at: Option<Instant>,
}

/// The Steam plugin. It holds no credential; it reads the key on each
/// refresh.
pub struct Steam {
    credentials: Arc<dyn Credentials>,
    index: Mutex<Index>,
}

impl fmt::Debug for Steam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Steam").finish_non_exhaustive()
    }
}

impl Steam {
    pub fn new(credentials: Arc<dyn Credentials>) -> Steam {
        Steam {
            credentials,
            index: Mutex::new(Index::default()),
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

    /// Fetches the whole list into a new index; the old one is kept
    /// until the new one is complete.
    async fn fetch(&self, kit: &Kit) -> Result<HashMap<String, Vec<(u32, String)>>, PluginError> {
        let key = match self.credentials.get(ID, API_KEY).await {
            Ok(Some(k)) => k,
            Ok(None) => return Err(fail(NO_CREDENTIALS)),
            Err(_) => return Err(fail(UNREADABLE)),
        };
        if !valid_key(key.expose()) {
            return Err(fail(BAD_CREDENTIALS));
        }
        let mut names: HashMap<String, Vec<(u32, String)>> = HashMap::new();
        let (mut after, mut kept, mut bytes) = (None, 0usize, 0usize);
        for _ in 0..MAX_PAGES {
            let res = match kit.http.send(&list_request(after, &key)).await {
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
            bytes = bytes.saturating_add(res.body.len());
            if bytes > MAX_LIST_BYTES {
                return Err(fail(TOO_LONG));
            }
            let body = res.body;
            let page = tokio::task::spawn_blocking(move || steam::parse_page(&body, &LIMITS))
                .await
                .map_err(|_| fail(BAD_ANSWER))?;
            if !page.whole() {
                return Err(fail(BAD_ANSWER));
            }
            for app in page.apps {
                kept += 1;
                if kept > MAX_APPS {
                    return Err(fail(TOO_LONG));
                }
                let f = fold(&app.name);
                if !f.is_empty() {
                    names.entry(f).or_default().push((app.id, app.name));
                }
            }
            match (page.more, page.last_appid) {
                (false, _) => return Ok(names),
                // The cursor must move forward, or the list never ends.
                (true, Some(next)) if after.is_none_or(|a| next > a) => after = Some(next),
                _ => return Err(fail(BAD_ANSWER)),
            }
        }
        Err(fail(TOO_LONG))
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
            // One lookup at a time refreshes; the others wait for it and
            // then read the same index, so N lookups make one fetch.
            let mut index = self.index.lock().await;
            let now = Instant::now();
            let stale = index.fetched.is_none_or(|t| now >= t + REFRESH);
            let waiting = index.retry_at.is_some_and(|t| now < t);
            if stale && !waiting {
                match self.fetch(kit).await {
                    Ok(names) => {
                        index.names = names;
                        index.fetched = Some(Instant::now());
                        index.retry_at = None;
                    }
                    Err(e) => {
                        index.retry_at = Some(Instant::now() + RETRY);
                        if index.fetched.is_none() {
                            return Err(e);
                        }
                    }
                }
            } else if index.fetched.is_none() {
                return Err(fail(HELD));
            }
            Ok(choose(query, index.names.get(&want).map(Vec::as_slice)))
        })
    }
}

/// The one game named `want`. None, or more than one, is no match.
pub fn choose(query: &Query, apps: Option<&[(u32, String)]>) -> Lookup {
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
        year: query.year(),
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
