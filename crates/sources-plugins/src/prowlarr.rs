//! Prowlarr and Jackett, source plugin #1 (architecture §18.2 rank 1).
//!
//! The user's own indexer manager, at the address they enter, with its
//! API key from the per-source credential store (SRC-4). Each poll:
//!
//! 1. lists the manager's indexers: Prowlarr's `GET /api/v1/indexer`
//!    (JSON), or Jackett's Torznab `t=indexers` (XML). Only enabled
//!    BitTorrent indexers are kept, each with whether it's private;
//! 2. asks each indexer for its newest results through the manager's
//!    Torznab endpoint (`t=search` with no query, the RSS view that
//!    Sonarr and Radarr poll), at most `agents` at a time;
//! 3. parses the answer with SRC-6's bounded Torznab parser and emits
//!    one key with claims per item.
//!
//! **Torznab XML, not Prowlarr's `/api/v1/search` JSON.** Both managers
//! speak it, so one code path covers both, and SRC-6's parser is already
//! bounded, fuzzed and tested on their shapes. The JSON search is
//! Prowlarr's alone and would need a second untrusted-input parser.
//!
//! **Private indexers.** An indexer the manager doesn't call public
//! (Prowlarr's `private` and `semiPrivate`, Jackett's `private` and
//! `semi-private`, or anything unrecognised) makes its finds
//! `private_source`. Those carry claims only, with no external reference
//! (Q15): this plugin never downloads a `.torrent`, from any indexer.
//!
//! **Everything the manager sends is untrusted.** Lists are capped by
//! size, count and depth; XML DTDs are refused; ids that aren't plain
//! names are dropped; the info hash comes only from the Torznab
//! `infohash` attribute or a magnet's `xt` (SRC-6), never from a link;
//! sizes and dates outside sane ranges are dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::net::{self, NetPolicy};
use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{ClaimedFile, Claims, Confidence, ExternalId, Provenance};
use lodestar_sources::http::host_key;
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Emitter, Endpoints, Found, HostRate,
    HttpError, Kind, Manifest, Network, PluginError, Policy, Priority, Request, Risk, Schedule,
    SourceCtx, SourcePlugin,
};
use lodestar_sources_feeds::torznab::{self, Item, LIMITS, Limits};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde::Deserialize;
use serde::de::{IgnoredAny, SeqAccess, Visitor};

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "prowlarr";
/// The credential that holds the manager's API key.
pub const API_KEY: &str = "api_key";
/// The strategy code on every key this plugin emits: an indexer's RSS.
pub const STRATEGY_RSS: u8 = 1;

/// Indexers polled per manager; more are ignored.
pub const MAX_INDEXERS: usize = 500;
/// Bytes in an indexer list. Prowlarr's carries every indexer's settings,
/// a few KiB each, so 500 indexers fit with room to spare.
pub const MAX_LIST_BYTES: usize = 8 * 1024 * 1024;
/// Bytes in an indexer id: Prowlarr's are numbers, Jackett's short names.
pub const MAX_ID: usize = 64;
/// Results asked for, and kept, per indexer page.
pub const PAGE_SIZE: usize = 100;
/// The default number of indexers queried at once (§18.4's agents).
pub const DEFAULT_AGENTS: u8 = 2;
pub const MAX_AGENTS: u8 = 8;
/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;
/// The earliest publish date believed: 2001-07-02, BitTorrent's release.
pub const MIN_PUBLISHED: i64 = 994_032_000;
/// How far past now a publish date may be (clock skew, time zones).
pub const MAX_FUTURE: i64 = 24 * 3600;
/// Bytes in an API key. Both managers issue 32 hex digits.
pub const MAX_KEY: usize = 128;

/// The caps for one indexer page: SRC-6's, with fewer items. A page asks
/// for [`PAGE_SIZE`]; an indexer that sends ten times that is cut off.
pub const PAGE_LIMITS: Limits = Limits {
    max_items: PAGE_SIZE * 10,
    ..LIMITS
};

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Prowlarr / Jackett",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    // None built in: the only host is the address the user enters
    // (`Prowlarr::endpoints`).
    endpoints: &[],
    risk: Risk::Medium,
    consent: Consent::Account,
    default_priority: Priority::Normal,
    enabled_by_default: false,
};

/// The settings form (§10.10). `api_key` is write-only: the host stores it
/// as the `api_key` credential, and [`Config::from_json`] refuses it.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"url":{"type":"string","format":"uri","title":"Address","description":"Your Prowlarr or Jackett address, such as http://prowlarr:9696"},"manager":{"type":"string","enum":["prowlarr","jackett"],"default":"prowlarr","title":"Indexer manager"},"agents":{"type":"integer","minimum":1,"maximum":8,"default":2,"title":"Indexers queried at once"},"api_key":{"type":"string","writeOnly":true,"title":"API key"}},"required":["url","manager","api_key"],"additionalProperties":false}"#;

/// Q4 (runner): each indexer's RSS at most every 15 minutes. The manager
/// is the user's own, usually on the LAN, and paces its indexers itself;
/// one request every 2 s with a burst of 2 keeps a long list from
/// bunching up on it.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(15 * 60),
        per_host: HostRate {
            interval: Duration::from_secs(2),
            burst: 2,
        },
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
    },
    config_schema: CONFIG_SCHEMA,
    // §19.5: Prowlarr shards per indexer.
    distribution: Distribution::ShardedUnit,
    schedule: Schedule::ALWAYS,
};

/// Which indexer manager is at the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Manager {
    Prowlarr,
    Jackett,
}

/// The plugin's settings, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `scheme://host[:port][/path]`, no trailing slash.
    base: String,
    /// The limiter's and the egress host list's key for `base`.
    host: String,
    manager: Manager,
    agents: u8,
}

/// Why settings were refused. Names the rule, never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError(pub &'static str);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    url: String,
    manager: Manager,
    #[serde(default)]
    agents: Option<u8>,
}

impl Config {
    /// Checks `url` and the agent count. The address must be http(s) with
    /// a host, no user name, password, query or fragment, and pass the
    /// SSRF guard's download-client policy (LAN and loopback allowed;
    /// link-local and metadata addresses never).
    pub fn new(url: &str, manager: Manager, agents: Option<u8>) -> Result<Config, ConfigError> {
        let parsed = url::Url::parse(url.trim())
            .map_err(|_| ConfigError("the address isn't a valid URL"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ConfigError("the address has a user name or password in it"));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(ConfigError("the address can't have a query or fragment"));
        }
        net::check_url(&parsed, NetPolicy::download_client())
            .map_err(|_| ConfigError("the address isn't one Lodestar may reach"))?;
        let base = parsed.as_str().trim_end_matches('/').to_owned();
        let host = host_key(&base).ok_or(ConfigError("the address needs an http(s) host"))?;
        let agents = agents.unwrap_or(DEFAULT_AGENTS);
        if !(1..=MAX_AGENTS).contains(&agents) {
            return Err(ConfigError("agents must be 1 to 8"));
        }
        Ok(Config {
            base,
            host,
            manager,
            agents,
        })
    }

    /// The settings as the form saves them (see [`CONFIG_SCHEMA`]). An
    /// `api_key` here is refused: it belongs in the credential store.
    pub fn from_json(json: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = serde_json::from_str(json)
            .map_err(|_| ConfigError("the settings don't match the form"))?;
        Config::new(&raw.url, raw.manager, raw.agents)
    }

    pub fn manager(&self) -> Manager {
        self.manager
    }

    pub fn agents(&self) -> u8 {
        self.agents
    }
}

/// The Prowlarr/Jackett plugin.
pub struct Prowlarr {
    config: Option<Config>,
    credentials: Arc<dyn Credentials>,
}

impl std::fmt::Debug for Prowlarr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prowlarr")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Prowlarr {
    pub fn new(config: Config, credentials: Arc<dyn Credentials>) -> Prowlarr {
        Prowlarr {
            config: Some(config),
            credentials,
        }
    }

    /// Listed in Settings → Sources before it's set up; fails at once if
    /// run.
    pub fn unconfigured(credentials: Arc<dyn Credentials>) -> Prowlarr {
        Prowlarr {
            config: None,
            credentials,
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the
    /// configured host only, under the download-client policy. `None`
    /// until it's set up, so nothing may be reached.
    pub fn endpoints(&self) -> Option<Endpoints> {
        let c = self.config.as_ref()?;
        Some(Endpoints::user_entered(NetPolicy::download_client()).only_hosts(&[&c.host]))
    }
}

impl SourcePlugin for Prowlarr {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(self.run_inner(ctx))
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// The manager refused the API key: the plugin stops and says so, rather
/// than knocking every 15 minutes.
const REFUSED: &str = "the indexer manager refused the API key";
const REDIRECTED: &str = "needs setup: the indexer manager redirected the request; check the address (http or https, and any base path)";
const NOT_FOUND: &str = "needs setup: the address has no indexer list; check the address, any base path, and whether it's Prowlarr or Jackett";
const NOT_A_LIST: &str = "needs setup: the address didn't answer with an indexer list; check the address and whether it's Prowlarr or Jackett";

impl Prowlarr {
    async fn run_inner(&self, ctx: SourceCtx) -> Result<(), PluginError> {
        let SourceCtx {
            emitter,
            mut shutdown,
            kit,
            ..
        } = ctx;
        let config = self
            .config
            .clone()
            .ok_or_else(|| fail("needs setup: no indexer manager address is set"))?;
        let key = tokio::select! {
            _ = shutdown.requested() => return Ok(()),
            k = self.credentials.get(ID, API_KEY) => k,
        };
        let key = key
            .map_err(|_| fail("the stored API key couldn't be read"))?
            .ok_or_else(|| fail("needs setup: no API key is stored"))?;
        if !valid_key(key.expose()) {
            return Err(fail("the stored API key isn't a valid key"));
        }
        let job = Arc::new(Job {
            config,
            key,
            http: kit.http,
            emitter,
        });
        loop {
            let listed = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                r = job.list() => r,
            };
            let indexers = match listed {
                Ok(list) => list,
                Err(Failure::Refused) => return Err(fail(REFUSED)),
                Err(Failure::Setup(why)) => return Err(fail(why)),
                // The next list waits for the poll gate.
                Err(Failure::Other) => continue,
                Err(Failure::Closed) => return Ok(()),
            };
            let polled = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                r = poll_all(&job, indexers) => r,
            };
            match polled {
                Ok(()) | Err(Failure::Other) => {}
                Err(Failure::Refused) => return Err(fail(REFUSED)),
                Err(Failure::Setup(why)) => return Err(fail(why)),
                Err(Failure::Closed) => return Ok(()),
            }
        }
    }
}

/// An API key goes into a header or a query, so only ASCII letters,
/// digits, `-` and `_`.
fn valid_key(k: &str) -> bool {
    (1..=MAX_KEY).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// 401, 403, or a Torznab error 100–102 (bad key, suspended, no
    /// rights).
    Refused,
    /// Anything else: logged by the limiter's back-off, tried next poll.
    Other,
    /// The pipeline shut down.
    Closed,
    /// The list's answer shows the address or the manager type is wrong:
    /// a redirect (never followed), a 404, or a 200 that isn't an indexer
    /// list. Retrying every 15 minutes would only hide it.
    Setup(&'static str),
}

/// One indexer, as the manager listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexer {
    /// ASCII letters, digits, `-` and `_`, at most [`MAX_ID`] bytes.
    pub id: String,
    /// Anything but public.
    pub private: bool,
}

struct Job {
    config: Config,
    key: Secret,
    http: Client,
    emitter: Emitter,
}

impl Job {
    fn list_request(&self) -> Request {
        let base = &self.config.base;
        match self.config.manager {
            Manager::Prowlarr => Request::get(format!("{base}/api/v1/indexer"))
                .header("Accept", "application/json")
                .header("X-Api-Key", self.key.expose()),
            Manager::Jackett => Request::get(format!(
                "{base}/api/v2.0/indexers/all/results/torznab/api?apikey={}&t=indexers&configured=true",
                self.key.expose()
            )),
        }
    }

    fn page_request(&self, ix: &Indexer) -> Request {
        let (base, id) = (&self.config.base, &ix.id);
        match self.config.manager {
            Manager::Prowlarr => Request::get(format!(
                "{base}/{id}/api?t=search&extended=1&limit={PAGE_SIZE}"
            ))
            .header("X-Api-Key", self.key.expose()),
            Manager::Jackett => Request::get(format!(
                "{base}/api/v2.0/indexers/{id}/results/torznab/api?apikey={}&t=search&extended=1&limit={PAGE_SIZE}",
                self.key.expose()
            )),
        }
    }

    async fn list(&self) -> Result<Vec<Indexer>, Failure> {
        let res = self.http.poll("indexers", &self.list_request()).await;
        match &res {
            Ok(r) if (300..400).contains(&r.status) => return Err(Failure::Setup(REDIRECTED)),
            Ok(r) if r.status == 404 => return Err(Failure::Setup(NOT_FOUND)),
            _ => {}
        }
        let res = answer(res)?;
        parse_indexers(self.config.manager, &res).map_err(|e| match e {
            ListError::Refused => Failure::Refused,
            // Over the size cap is a real (if huge) list: not a setup error.
            ListError::TooBig => Failure::Other,
            _ => Failure::Setup(NOT_A_LIST),
        })
    }

    async fn poll(&self, ix: &Indexer) -> Result<(), Failure> {
        let unit = format!("indexer/{}", ix.id);
        let body = answer(self.http.poll(&unit, &self.page_request(ix)).await)?;
        let now = unix_now();
        let mut found = Vec::new();
        let summary = torznab::parse_each(&body, &PAGE_LIMITS, |item| {
            found.extend(to_found(item, ix.private, &self.config.host, now));
        });
        if matches!(summary.error, Some(100..=102)) {
            return Err(Failure::Refused);
        }
        for f in found {
            self.emitter.emit(f).await.map_err(|_| Failure::Closed)?;
        }
        Ok(())
    }
}

/// The body of a 200, or why not.
fn answer(r: Result<lodestar_sources::Response, HttpError>) -> Result<Vec<u8>, Failure> {
    match r {
        Ok(res) if res.status == 200 => Ok(res.body),
        Ok(res) if matches!(res.status, 401 | 403) => Err(Failure::Refused),
        _ => Err(Failure::Other),
    }
}

/// Polls every indexer, `agents` at a time. Returns at the first refusal;
/// dropping the set aborts the rest, so shutdown is prompt.
async fn poll_all(job: &Arc<Job>, indexers: Vec<Indexer>) -> Result<(), Failure> {
    let queue = Arc::new(Mutex::new(VecDeque::from(indexers)));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..job.config.agents {
        let (job, queue) = (Arc::clone(job), Arc::clone(&queue));
        set.spawn(async move {
            loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                let Some(ix) = next else { return Ok(()) };
                match job.poll(&ix).await {
                    Ok(()) | Err(Failure::Other) => {}
                    Err(f) => return Err(f),
                }
            }
        });
    }
    while let Some(done) = set.join_next().await {
        // A worker that panicked is one indexer's loss, not the plugin's.
        if let Ok(Err(f)) = done {
            return Err(f);
        }
    }
    Ok(())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// One Torznab item as a discovery, or `None` for an item that isn't a
/// torrent. Every field stays a claim; the key is the parser's, from the
/// `infohash` attribute or a magnet only.
///
/// - `private`: the find is `private_source`, and carries no external
///   reference (a private tracker's URLs can hold passkeys in the path).
/// - The item's `link` (the manager's own download URL) is never kept.
///   Its `guid` is the reference for a public find, unless it points at
///   the manager itself (`manager_host`), equals the link, or has the
///   shape of a manager's download URL on any host (see
///   [`is_manager_download`]): the manager's links can name another host
///   than the one configured (a reverse proxy, a Docker service name).
pub fn to_found(item: Item, private: bool, manager_host: &str, now: i64) -> Option<Found> {
    let torznab::Kind::Torrent(hashes) = item.kind else {
        return None;
    };
    let key = hashes.key()?;
    let claims = Claims {
        name: item.title,
        size: item.size.filter(|s| (1..=MAX_SIZE).contains(s)),
        ids: item
            .ids
            .into_iter()
            .map(|(scheme, value)| ExternalId {
                scheme: scheme.to_owned(),
                value,
            })
            .collect(),
        files: Vec::<ClaimedFile>::new(),
        ..Claims::default()
    };
    let external_ref = if private {
        None
    } else {
        let link = item.link;
        item.guid.filter(|g| {
            Some(g) != link.as_ref()
                && host_key(g).is_none_or(|h| h != manager_host)
                && !is_manager_download(g)
        })
    };
    let published_at = item
        .published
        .filter(|t| (MIN_PUBLISHED..=now.saturating_add(MAX_FUTURE)).contains(t));
    let mut found = Found::key(key, STRATEGY_RSS)
        .with_claims(claims)
        .with_provenance(Provenance {
            external_ref,
            published_at,
            confidence: Confidence::Low,
        });
    found.private_source = private;
    Some(found)
}

/// Whether `url` has the shape of a manager's proxied download link:
/// Prowlarr's `…/<indexer id>/download` or Jackett's `…/dl/<indexer>/…`,
/// on any host. The link points into the user's own manager, so it's
/// never kept as a reference. Matching is on path segments, ignoring
/// case; a URL that doesn't parse isn't a link.
pub fn is_manager_download(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Some(segs) = u.path_segments() else {
        return false;
    };
    let segs: Vec<&str> = segs.collect();
    segs.windows(2).any(|w| {
        (w[1].eq_ignore_ascii_case("download")
            && !w[0].is_empty()
            && w[0].bytes().all(|b| b.is_ascii_digit()))
            || (w[0].eq_ignore_ascii_case("dl") && !w[1].is_empty())
    })
}

/// Why an indexer list was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListError {
    /// Over [`MAX_LIST_BYTES`].
    TooBig,
    /// Not the shape the manager sends.
    Malformed,
    /// A DTD: refused, so no entity is ever defined.
    Doctype,
    /// Nested past [`MAX_DEPTH`].
    Depth,
    /// More than [`MAX_ELEMENTS`] elements.
    Elements,
    /// The manager's Torznab error 100–102: the key was refused.
    Refused,
}

/// XML nesting in a Jackett list: `indexers > indexer > caps > …` is 5.
pub const MAX_DEPTH: usize = 16;
/// Elements in a Jackett list: each indexer's caps are a few dozen.
pub const MAX_ELEMENTS: usize = 500_000;
/// Attributes read per element.
const MAX_ATTRS: usize = 16;
/// Bytes of an indexer's `type` text compared.
const MAX_TYPE: usize = 32;

/// The enabled BitTorrent indexers in `manager`'s list, at most
/// [`MAX_INDEXERS`], without duplicates.
pub fn parse_indexers(manager: Manager, body: &[u8]) -> Result<Vec<Indexer>, ListError> {
    if body.len() > MAX_LIST_BYTES {
        return Err(ListError::TooBig);
    }
    let mut list = match manager {
        Manager::Prowlarr => prowlarr_list(body)?,
        Manager::Jackett => jackett_list(body)?,
    };
    let mut seen = std::collections::HashSet::new();
    list.retain(|i| seen.insert(i.id.clone()));
    Ok(list)
}

/// An indexer id that is safe in a URL path: 1–[`MAX_ID`] ASCII letters,
/// digits, `-` and `_`. Jackett's `all` is its aggregate of every
/// indexer, which would hide each one's privacy, so it's never polled.
fn valid_id(id: &[u8]) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        && !id.eq_ignore_ascii_case(b"all")
}

/// Prowlarr's `/api/v1/indexer`: a JSON array of indexer resources.
/// Fields other than these four are skipped unread (serde's
/// `IgnoredAny`), and serde_json's recursion limit bounds nesting.
fn prowlarr_list(body: &[u8]) -> Result<Vec<Indexer>, ListError> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        id: Option<u32>,
        #[serde(default)]
        enable: bool,
        #[serde(default)]
        protocol: Option<String>,
        #[serde(default)]
        privacy: Option<String>,
        #[serde(default, rename = "supportsRss")]
        supports_rss: Option<bool>,
    }

    struct List(Vec<Indexer>);

    impl<'de> Deserialize<'de> for List {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<List, D::Error> {
            d.deserialize_seq(ListVisitor)
        }
    }

    struct ListVisitor;

    impl<'de> Visitor<'de> for ListVisitor {
        type Value = List;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list of indexers")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<List, A::Error> {
            let mut out = Vec::new();
            while out.len() < MAX_INDEXERS {
                let Some(raw) = seq.next_element::<Raw>()? else {
                    return Ok(List(out));
                };
                let Some(id) = raw.id.filter(|_| raw.enable) else {
                    continue;
                };
                // An indexer without RSS answers an empty search with
                // nothing; asking every 15 minutes only costs it.
                if raw.protocol.as_deref() != Some("torrent") || raw.supports_rss == Some(false) {
                    continue;
                }
                let id = id.to_string();
                // Deduplicated before the cap, so repeats can't crowd
                // out real indexers.
                if out.iter().all(|i: &Indexer| i.id != id) {
                    out.push(Indexer {
                        id,
                        private: raw.privacy.as_deref() != Some("public"),
                    });
                }
            }
            // The rest is read, unkept, so the document is still checked.
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(List(out))
        }
    }

    serde_json::from_slice::<List>(body)
        .map(|l| l.0)
        .map_err(|_| ListError::Malformed)
}

/// Jackett's `t=indexers&configured=true`:
///
/// ```xml
/// <indexers>
///   <indexer id="example" configured="true">
///     <title>Example</title> … <type>public</type> <caps>…</caps>
///   </indexer>
/// </indexers>
/// ```
///
/// Only the `id` and `configured` attributes and the `type` text are
/// read, compared as raw bytes: an entity in them leaves the id invalid
/// or the type unrecognised (so private), never expanded.
fn jackett_list(body: &[u8]) -> Result<Vec<Indexer>, ListError> {
    struct Draft {
        id: Option<String>,
        configured: bool,
        kind: Vec<u8>,
        in_type: bool,
        type_seen: bool,
    }

    fn open(e: &BytesStart<'_>) -> Result<Draft, ListError> {
        let mut d = Draft {
            id: None,
            configured: false,
            kind: Vec::new(),
            in_type: false,
            type_seen: false,
        };
        for (i, a) in e.attributes().with_checks(false).enumerate() {
            if i >= MAX_ATTRS {
                return Err(ListError::Malformed);
            }
            let a = a.map_err(|_| ListError::Malformed)?;
            match a.key.as_ref() {
                "id" if d.id.is_none() && valid_id(a.value.as_bytes()) => {
                    d.id = Some(a.value.to_string());
                }
                "configured" => d.configured = a.value == "true",
                _ => {}
            }
        }
        Ok(d)
    }

    fn close(d: Draft, out: &mut Vec<Indexer>) {
        if let (Some(id), true) = (d.id, d.configured)
            && out.len() < MAX_INDEXERS
            && out.iter().all(|i| i.id != id)
        {
            out.push(Indexer {
                id,
                private: !(d.type_seen && d.kind == b"public"),
            });
        }
    }

    let text = std::str::from_utf8(body).map_err(|_| ListError::Malformed)?;
    let mut reader = Reader::from_str(text);
    let (mut depth, mut elements, mut root) = (0usize, 0usize, false);
    let mut draft: Option<Draft> = None;
    let mut out = Vec::new();
    loop {
        let ev = reader.read_event().map_err(|_| ListError::Malformed)?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) if depth == 0 => {
                elements += 1;
                match e.name().as_ref() {
                    _ if root => return Err(ListError::Malformed),
                    "indexers" => root = true,
                    "error" => return Err(torznab_error(e)),
                    _ => return Err(ListError::Malformed),
                }
                if matches!(ev, Event::Start(_)) {
                    depth = 1;
                }
            }
            Event::Start(e) => {
                elements += 1;
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(ListError::Depth);
                }
                if elements > MAX_ELEMENTS {
                    return Err(ListError::Elements);
                }
                match (depth, e.name().as_ref(), &mut draft) {
                    (2, "indexer", _) => draft = Some(open(&e)?),
                    (3, "type", Some(d)) => {
                        d.in_type = !d.type_seen;
                        d.type_seen = true;
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                elements += 1;
                if depth + 1 > MAX_DEPTH {
                    return Err(ListError::Depth);
                }
                if elements > MAX_ELEMENTS {
                    return Err(ListError::Elements);
                }
                match (depth + 1, e.name().as_ref(), &mut draft) {
                    (2, "indexer", _) => close(open(&e)?, &mut out),
                    (3, "type", Some(d)) => d.type_seen = true,
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some(d) = draft.as_mut().filter(|d| d.in_type) {
                    if d.kind.len() + t.len() <= MAX_TYPE {
                        d.kind.extend_from_slice(t.as_ref().as_bytes());
                    } else {
                        // Too long to be a known type: private.
                        d.kind.clear();
                        d.kind.push(b'?');
                    }
                }
            }
            Event::CData(_) | Event::GeneralRef(_) => {
                if let Some(d) = draft.as_mut().filter(|d| d.in_type) {
                    d.kind.push(b'?');
                }
            }
            Event::End(e) => {
                match (depth, e.name().as_ref()) {
                    (2, "indexer") => {
                        if let Some(d) = draft.take() {
                            close(d, &mut out);
                        }
                    }
                    (3, "type") => {
                        if let Some(d) = draft.as_mut() {
                            d.in_type = false;
                        }
                    }
                    _ => {}
                }
                depth = depth.checked_sub(1).ok_or(ListError::Malformed)?;
            }
            Event::DocType(_) => return Err(ListError::Doctype),
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
        }
    }
    if !root || depth != 0 {
        return Err(ListError::Malformed);
    }
    Ok(out)
}

/// A Torznab `<error code="…">` answer: 100–102 are a refused key.
fn torznab_error(e: &BytesStart<'_>) -> ListError {
    let code = e
        .attributes()
        .with_checks(false)
        .take(MAX_ATTRS)
        .flatten()
        .find(|a| a.key.as_ref() == "code")
        .map(|a| a.value);
    match code.as_deref() {
        Some("100" | "101" | "102") => ListError::Refused,
        _ => ListError::Malformed,
    }
}
