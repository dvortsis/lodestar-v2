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
//!
//! **Usenet results (UN-1), off by default.** With the `usenet` setting
//! on (Prowlarr only), enabled Usenet indexers are polled the same way
//! and each Newznab result becomes a usenet item (UN-0): keyed by the
//! indexer id and the result's GUID, always `private_source`, never
//! fetched or verified. **The NZB link is never kept:** Prowlarr's
//! download links carry its API key, and the indexer's. The item is keyed by
//! the indexer id and the GUID, and only a GUID that can't fetch anything
//! is used ([`usenet_guid`]); no reference is stored (L3), so send's
//! (UN-3) is still to be designed. The poster is never read. See
//! `docs/decisions/2026-11-12-UN-1.md`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::net::{self, NetPolicy};
use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::{ClaimedFile, Claims, Confidence, Details, ExternalId, Provenance};
use lodestar_sources::consent::{CredentialKind, CredentialSpec};
use lodestar_sources::http::host_key;
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, ConfigRefusal, Consent, Distribution, Emitter, Endpoints,
    Found, HostRate, HttpError, Kind, Manifest, Network, PluginError, Policy, Priority, Request,
    Risk, Schedule, SourceCtx, SourcePlugin,
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
/// Bytes in the saved settings, checked before they're parsed.
pub const MAX_CONFIG_BYTES: usize = 4096;
/// Bytes in the address, checked before it's parsed.
pub const MAX_URL: usize = 2048;

/// The caps for one indexer page: SRC-6's, with fewer items. A page asks
/// for [`PAGE_SIZE`]; an indexer that sends ten times that is cut off.
pub const PAGE_LIMITS: Limits = Limits {
    max_items: PAGE_SIZE * 10,
    ..LIMITS
};

/// Bytes in one Usenet indexer page, checked before it's parsed; a larger
/// answer is refused whole. A page of [`PAGE_SIZE`] Newznab results is
/// about 100–300 KiB (a result's attributes and links are under 3 KiB).
pub const USENET_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// Usenet results kept per indexer page: the [`PAGE_SIZE`] asked for.
/// The rest of a longer page is read (so it's still checked) and dropped.
pub const USENET_PAGE_ITEMS: usize = PAGE_SIZE;
/// Usenet results emitted per poll of the whole indexer list. An
/// indexer whose page was cut short isn't marked polled, and one reached
/// after the budget is spent isn't asked at all: both are asked on the
/// next poll (within [`USENET_DAILY_HITS`]). The store takes at most
/// this many usenet items in one write (`MAX_USENET_BATCH`), and 4,096
/// is about 40 busy indexers' newest page.
pub const USENET_POLL_ITEMS: usize = 4096;
/// How often each Usenet indexer is polled when its pages arrive whole:
/// hourly, 24 API hits a day (retries can add as many again, see
/// [`USENET_DAILY_HITS`]). Newznab indexers count API hits per day (free
/// accounts often get 100 or fewer), and Sonarr and Radarr share them,
/// so the torrent indexers' 15 minutes (96 a day) could use up a user's
/// quota.
pub const USENET_POLL_EVERY: Duration = Duration::from_secs(3600);
/// How far short of [`USENET_POLL_EVERY`] a poll may come: half a list
/// round (the poll gate's 15 minutes). Rounds land at slightly different
/// offsets, so without it a poll due "after an hour" would slip to the
/// round after, every 75 minutes.
pub const USENET_POLL_SLACK: Duration =
    Duration::from_secs(POLICY.caps.min_poll_interval.as_secs() / 2);
/// API hits per Usenet indexer in any 24 hours, retries included: 24 for
/// the hourly polls and as many again for retries after a failed,
/// oversized or partly sent page, which come on the normal 15-minute
/// cadence. Past it, the indexer waits until its oldest hit is a day
/// old. A request the limiter never sent (`HttpError::Blocked` after a
/// `Retry-After`) isn't a hit.
pub const USENET_DAILY_HITS: usize = 48;
const DAY: Duration = Duration::from_secs(24 * 3600);

/// When each Usenet indexer last gave a page that was all sent, and its
/// hits in the last day ([`USENET_POLL_EVERY`], [`USENET_DAILY_HITS`]).
/// An entry lives up to a day after its last hit, so a manager whose ids
/// change between lists could leave more than one list's worth: the map
/// is capped at [`MAX_INDEXERS`] entries, and a new id past the cap waits
/// until older entries age out. Each entry holds at most
/// [`USENET_DAILY_HITS`] instants.
#[derive(Debug, Default)]
pub(crate) struct UsenetPolls(std::collections::HashMap<String, UsenetPolled>);

#[derive(Debug, Default)]
struct UsenetPolled {
    done: Option<tokio::time::Instant>,
    hits: VecDeque<tokio::time::Instant>,
}

impl UsenetPolls {
    /// Whether `id` may be asked at `now`: not within [`USENET_POLL_EVERY`]
    /// (less [`USENET_POLL_SLACK`]) of a poll whose page was all sent,
    /// and under [`USENET_DAILY_HITS`] in the last day.
    pub(crate) fn due(&mut self, id: &str, now: tokio::time::Instant) -> bool {
        let wait = USENET_POLL_EVERY.saturating_sub(USENET_POLL_SLACK);
        self.0.retain(|_, p| {
            p.hits.retain(|at| now.duration_since(*at) < DAY);
            p.done = p.done.filter(|at| now.duration_since(*at) < wait);
            p.done.is_some() || !p.hits.is_empty()
        });
        match self.0.get(id) {
            Some(p) => p.done.is_none() && p.hits.len() < USENET_DAILY_HITS,
            None => self.0.len() < MAX_INDEXERS,
        }
    }

    /// A request to `id`, granted at `at`, was sent.
    pub(crate) fn hit(&mut self, id: &str, at: tokio::time::Instant) {
        let p = self.0.entry(id.to_owned()).or_default();
        if p.hits.len() < USENET_DAILY_HITS {
            p.hits.push_back(at);
        }
    }

    /// `id`'s poll granted at `at` got `page`: a hit unless the request
    /// was never sent ([`Failure::NotSent`]).
    fn answered(&mut self, id: &str, at: tokio::time::Instant, page: &Result<Vec<u8>, Failure>) {
        if *page != Err(Failure::NotSent) {
            self.hit(id, at);
        }
    }

    /// `id`'s poll granted at `at` was a 200 and every result was sent.
    /// Stamped with the grant, not the answer, so the next poll is due a
    /// round-aligned hour later.
    pub(crate) fn done(&mut self, id: &str, at: tokio::time::Instant) {
        self.0.entry(id.to_owned()).or_default().done = Some(at);
    }
}

/// Bytes in a Usenet result's GUID, the store's limit on a key part
/// (`USENET_MAX_PART`).
pub const USENET_MAX_GUID: usize = 1024;
/// The earliest posting date believed: 1980-01-01. Binary retention
/// reaches back to about 2008; anything before 1980 is junk.
pub const USENET_MIN_POSTED: i64 = 315_532_800;
/// Bytes of text a usenet item's claims can give the filter to judge at
/// ingest: the title (at most 1,024) on its own and joined with the
/// category (at most 10 digits), with room. The claims carry nothing else
/// that's judged, so a poll judges at most [`USENET_POLL_ITEMS`] × this
/// (16 MiB), well inside the ingest budget.
pub const USENET_MAX_JUDGED: usize = 4 * 1024;

/// The caps for one Usenet page: SRC-6's, with [`USENET_PAGE_BYTES`] and
/// ten times [`PAGE_SIZE`] items read.
pub const USENET_PAGE_LIMITS: Limits = Limits {
    max_bytes: USENET_PAGE_BYTES,
    max_items: PAGE_SIZE * 10,
    ..LIMITS
};

pub(crate) const MANIFEST: Manifest = Manifest {
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
    always_adult: false,
    // What your own indexers list, which may be private trackers.
    finds_private: true,
};

/// The settings form (§10.10). The `usenet` switch isn't on it until UN-3
/// makes usenet items sendable (runner, X-un1-nits): turning it on would
/// spend the indexers' daily API hits on items nobody can see or send.
/// [`Config::from_json`] still reads it, off by default. The API key isn't a setting: it's the
/// `api_key` credential the policy declares (X-sources-api-spec), stored
/// through the credential store, and [`Config::from_json`] refuses it.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"url":{"type":"string","format":"uri","title":"Address","description":"Your Prowlarr or Jackett address, such as http://prowlarr:9696"},"manager":{"type":"string","enum":["prowlarr","jackett"],"default":"prowlarr","title":"Indexer manager"},"agents":{"type":"integer","minimum":1,"maximum":8,"default":2,"title":"Indexers queried at once"}},"required":["url","manager"],"additionalProperties":false}"#;

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
    // One agent: the setting can't be changed (SRC-14a).
    max_agents: 1,
    credentials: &[CredentialSpec {
        name: API_KEY,
        label: "API key",
        kind: CredentialKind::Secret,
        required: true,
    }],
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
    /// The limiter's key for `base` (the egress list takes `base` itself).
    host: String,
    manager: Manager,
    agents: u8,
    /// Keep Usenet indexers' results (UN-1). Off unless the user turns it
    /// on, and Prowlarr only: Jackett has no Usenet indexers.
    usenet: bool,
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
    #[serde(default)]
    usenet: Option<bool>,
}

impl Config {
    /// Checks `url` and the agent count. The address must be http(s) with
    /// a host, no user name, password, query or fragment, and pass the
    /// SSRF guard's download-client policy (LAN and loopback allowed;
    /// link-local and metadata addresses never).
    pub fn new(url: &str, manager: Manager, agents: Option<u8>) -> Result<Config, ConfigError> {
        if url.len() > MAX_URL {
            return Err(ConfigError("the address is too long"));
        }
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
            usenet: false,
        })
    }

    /// The same settings with Usenet results on or off. Jackett has no
    /// Usenet indexers, so turning them on for it is refused.
    pub fn with_usenet(mut self, on: bool) -> Result<Config, ConfigError> {
        if on && self.manager != Manager::Prowlarr {
            return Err(ConfigError("Usenet results need Prowlarr"));
        }
        self.usenet = on;
        Ok(self)
    }

    /// The settings as the form saves them (see [`CONFIG_SCHEMA`]). An
    /// `api_key` here is refused: it belongs in the credential store.
    pub fn from_json(json: &str) -> Result<Config, ConfigError> {
        if json.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError("the settings are too long"));
        }
        let raw: RawConfig = serde_json::from_str(json)
            .map_err(|_| ConfigError("the settings don't match the form"))?;
        Config::new(&raw.url, raw.manager, raw.agents)?.with_usenet(raw.usenet.unwrap_or(false))
    }

    /// The settings in the form [`Config::from_json`] reads back as the
    /// same [`Config`]: the checked address, the manager, the agent count
    /// and, only when it's on, `usenet` (so forms saved before UN-1 read
    /// back unchanged). Never holds a credential.
    pub fn to_json(&self) -> String {
        let manager = match self.manager {
            Manager::Prowlarr => "prowlarr",
            Manager::Jackett => "jackett",
        };
        let mut v =
            serde_json::json!({"url": self.base, "manager": manager, "agents": self.agents});
        if self.usenet {
            v["usenet"] = serde_json::Value::Bool(true);
        }
        v.to_string()
    }

    pub fn manager(&self) -> Manager {
        self.manager
    }

    pub fn agents(&self) -> u8 {
        self.agents
    }

    pub fn usenet(&self) -> bool {
        self.usenet
    }
}

/// The Prowlarr/Jackett plugin.
pub struct Prowlarr {
    config: Option<Config>,
    credentials: Arc<dyn Credentials>,
    key_at_start: KeyAtStart,
}

/// The API key as it was stored when the plugin was built at Lodestar's
/// start ([`Prowlarr::at_start`]), so the key and the address it's sent
/// to always come from the same start. A run whose stored key differs
/// from it stops with [`KEY_CHANGED`] until Lodestar restarts: a key
/// saved for a new address (a companion hand-over writes the key, then
/// the address) never goes to the old one (X-prowlarr-settings).
enum KeyAtStart {
    /// Built by [`Prowlarr::new`]: each run uses the key stored then.
    Unpinned,
    /// The key stored at the start (`None` when there was none), or
    /// `Err` when it couldn't be read then.
    Pinned(Result<Option<Secret>, ()>),
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
            key_at_start: KeyAtStart::Unpinned,
        }
    }

    /// The plugin as Lodestar builds it at its start: from its saved
    /// settings (`None`, or settings [`Config::from_json`] refuses, leave
    /// it unconfigured) and with the API key stored now, which every run
    /// until the next start must still find (see [`KeyAtStart`]). Nothing
    /// about either is logged.
    pub async fn at_start(saved: Option<&str>, credentials: Arc<dyn Credentials>) -> Prowlarr {
        let config = saved.and_then(|json| Config::from_json(json).ok());
        let key = credentials.get(ID, API_KEY).await.map_err(|_| ());
        Prowlarr {
            config,
            credentials,
            key_at_start: KeyAtStart::Pinned(key),
        }
    }

    /// Listed in Settings → Sources before it's set up; fails at once if
    /// run.
    pub fn unconfigured(credentials: Arc<dyn Credentials>) -> Prowlarr {
        Prowlarr {
            config: None,
            credentials,
            key_at_start: KeyAtStart::Unpinned,
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the
    /// configured host only, under the download-client policy. `None`
    /// until it's set up, so nothing may be reached.
    pub fn endpoints(&self) -> Option<Endpoints> {
        let c = self.config.as_ref()?;
        Some(Endpoints::user_entered(NetPolicy::download_client()).only_origins(&[&c.base]))
    }
}

impl SourcePlugin for Prowlarr {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    /// The egress list is the configured host only (SRC-5), so the
    /// pipeline must be given it: the trait's default would read the
    /// manifest's empty list.
    fn endpoints(&self) -> Option<Endpoints> {
        Prowlarr::endpoints(self)
    }

    fn config_editable(&self) -> bool {
        true
    }

    /// Built by [`Prowlarr::at_start`], it keeps the key stored then
    /// ([`KeyAtStart`]).
    fn pins_credentials(&self) -> bool {
        matches!(self.key_at_start, KeyAtStart::Pinned(_))
    }

    /// `None` until it's set up: there's nothing to compare a saved form
    /// with.
    fn config_json(&self) -> Option<String> {
        self.config.as_ref().map(Config::to_json)
    }

    /// [`Config::from_json`]'s rules (the address through the SSRF guard's
    /// download-client policy); what's saved is [`Config::to_json`]. The
    /// API key is refused here: it's a credential.
    fn check_config(&self, json: &str) -> Result<String, ConfigRefusal> {
        Config::from_json(json)
            .map(|c| c.to_json())
            .map_err(|e| ConfigRefusal(e.0))
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
/// The stored key couldn't be read when Lodestar started.
pub const KEY_UNREAD_AT_START: &str =
    "the stored API key couldn't be read when Lodestar started; restart Lodestar";
/// The stored key isn't the one Lodestar started with ([`KeyAtStart`]).
pub const KEY_CHANGED: &str =
    "needs setup: the API key changed since Lodestar started; restart Lodestar to use it";
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
        match &self.key_at_start {
            KeyAtStart::Unpinned => {}
            KeyAtStart::Pinned(Err(())) => return Err(fail(KEY_UNREAD_AT_START)),
            KeyAtStart::Pinned(Ok(at_start)) => {
                if at_start.as_ref().map(Secret::expose) != Some(key.expose()) {
                    return Err(fail(KEY_CHANGED));
                }
            }
        }
        if !valid_key(key.expose()) {
            return Err(fail("the stored API key isn't a valid key"));
        }
        let job = Arc::new(Job {
            config,
            key,
            http: kit.http,
            emitter,
            usenet_left: AtomicUsize::new(0),
            usenet_polled: Mutex::default(),
        });
        loop {
            job.usenet_left.store(USENET_POLL_ITEMS, Ordering::Relaxed);
            let listed = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                r = job.list() => r,
            };
            let indexers = match listed {
                Ok(list) => list,
                Err(Failure::Refused) => return Err(fail(REFUSED)),
                Err(Failure::Setup(why)) => return Err(fail(why)),
                // The next list waits for the poll gate.
                Err(Failure::Other | Failure::NotSent) => continue,
                Err(Failure::Closed) => return Ok(()),
            };
            let polled = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                r = poll_all(&job, indexers) => r,
            };
            match polled {
                Ok(()) | Err(Failure::Other | Failure::NotSent) => {}
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
    /// The limiter or the egress refused the request before sending it
    /// (a `Retry-After` block, no egress): not an API hit. Otherwise
    /// handled as [`Failure::Other`].
    NotSent,
}

impl Failure {
    /// [`Failure::NotSent`] as [`Failure::Other`], for callers that don't
    /// count hits.
    fn sent_or_not(self) -> Failure {
        match self {
            Failure::NotSent => Failure::Other,
            f => f,
        }
    }
}

/// One indexer, as the manager listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexer {
    /// ASCII letters, digits, `-` and `_`, at most [`MAX_ID`] bytes.
    pub id: String,
    /// Anything but public.
    pub private: bool,
    /// A Usenet indexer (Prowlarr's `protocol: usenet`), listed only when
    /// the `usenet` setting is on.
    pub usenet: bool,
}

struct Job {
    config: Config,
    key: Secret,
    http: Client,
    emitter: Emitter,
    /// Usenet results this poll may still emit ([`USENET_POLL_ITEMS`]).
    usenet_left: AtomicUsize,
    /// When each Usenet indexer may be asked again ([`UsenetPolls`]).
    usenet_polled: Mutex<UsenetPolls>,
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
        let res = answer(res).map_err(Failure::sent_or_not)?;
        parse_indexers_with(self.config.manager, &res, self.config.usenet).map_err(|e| match e {
            ListError::Refused => Failure::Refused,
            // Over the size cap is a real (if huge) list: not a setup error.
            ListError::TooBig => Failure::Other,
            _ => Failure::Setup(NOT_A_LIST),
        })
    }

    async fn poll(&self, ix: &Indexer) -> Result<(), Failure> {
        let granted = tokio::time::Instant::now();
        // A Usenet indexer is asked only when it's due, and not once the
        // poll's budget is spent: a request then would cost an API hit for
        // nothing. It's asked on the next poll.
        if ix.usenet
            && (self.usenet_left.load(Ordering::Relaxed) == 0
                || !self.usenet_polls().due(&ix.id, granted))
        {
            return Ok(());
        }
        let page = self.page(ix).await;
        if ix.usenet {
            self.usenet_polls().answered(&ix.id, granted, &page);
        }
        let body = page.map_err(Failure::sent_or_not)?;
        let now = unix_now();
        if ix.usenet {
            // Marked polled only when the whole page went out: a failed,
            // refused or oversized page, or one the budget cut short, is
            // asked again on the next poll.
            if self.emit_usenet(ix, &body, now).await? {
                self.usenet_polls().done(&ix.id, granted);
            }
            return Ok(());
        }
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

impl Job {
    /// An indexer's page: the body of a 200, or why not.
    async fn page(&self, ix: &Indexer) -> Result<Vec<u8>, Failure> {
        let unit = format!("indexer/{}", ix.id);
        let body = answer(self.http.poll(&unit, &self.page_request(ix)).await)?;
        Ok(body)
    }

    fn usenet_polls(&self) -> std::sync::MutexGuard<'_, UsenetPolls> {
        self.usenet_polled.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One Usenet indexer's page as usenet items: at most
    /// [`USENET_PAGE_ITEMS`] of it, and only while the poll's
    /// [`USENET_POLL_ITEMS`] last. A page over [`USENET_PAGE_BYTES`] is
    /// refused whole. `true` when every result was sent. Nothing about a
    /// result is logged.
    async fn emit_usenet(&self, ix: &Indexer, body: &[u8], now: i64) -> Result<bool, Failure> {
        let found = match usenet_page(body, ix, &self.config.host, self.key.expose(), now) {
            Ok(found) => found,
            Err(UsenetPage::Refused) => return Err(Failure::Refused),
            Err(UsenetPage::TooBig) => return Err(Failure::Other),
        };
        for u in found {
            let Some(f) = u.into_found() else { continue };
            if !take_one(&self.usenet_left) {
                return Ok(false);
            }
            self.emitter.emit(f).await.map_err(|_| Failure::Closed)?;
        }
        Ok(true)
    }
}

/// Takes one from `n` unless it's 0: agents share a poll's budget, and
/// it never wraps.
fn take_one(n: &AtomicUsize) -> bool {
    let mut cur = n.load(Ordering::Relaxed);
    while cur > 0 {
        match n.compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}

/// The body of a 200, or why not. A request the limiter or the egress
/// refused before sending is [`Failure::NotSent`].
fn answer(r: Result<lodestar_sources::Response, HttpError>) -> Result<Vec<u8>, Failure> {
    match r {
        Ok(res) if res.status == 200 => Ok(res.body),
        Ok(res) if matches!(res.status, 401 | 403) => Err(Failure::Refused),
        Err(HttpError::Blocked { .. } | HttpError::BadUrl | HttpError::NoEgress) => {
            Err(Failure::NotSent)
        }
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
                    Ok(()) | Err(Failure::Other | Failure::NotSent) => {}
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
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
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

// ---- Usenet results (UN-1) ----

/// Why a Usenet page gave nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsenetPage {
    /// Over [`USENET_PAGE_BYTES`]: refused whole, tried next poll.
    TooBig,
    /// The manager's Newznab error 100–102: the key was refused.
    Refused,
}

/// A Usenet page's results as usenet items, at most
/// [`USENET_PAGE_ITEMS`]. Results that aren't Newznab items, or that
/// [`to_usenet`] refuses, are skipped.
pub fn usenet_page(
    body: &[u8],
    ix: &Indexer,
    manager_host: &str,
    manager_key: &str,
    now: i64,
) -> Result<Vec<UsenetFind>, UsenetPage> {
    if body.len() > USENET_PAGE_BYTES {
        return Err(UsenetPage::TooBig);
    }
    let mut found = Vec::new();
    let summary = torznab::parse_each(body, &USENET_PAGE_LIMITS, |item| {
        if found.len() < USENET_PAGE_ITEMS {
            found.extend(to_usenet(item, ix, manager_host, manager_key, now));
        }
    });
    if matches!(summary.error, Some(100..=102)) {
        return Err(UsenetPage::Refused);
    }
    Ok(found)
}

/// One Newznab result, checked, as it will be stored: the indexer id and
/// GUID it's keyed and found again by, and its claims. It never holds the
/// result's link: [`to_usenet`] drops it before this is built.
#[derive(Clone, PartialEq, Eq)]
pub struct UsenetFind {
    /// Prowlarr's indexer id, in decimal without leading zeros (UN-0).
    pub indexer: String,
    /// The GUID, as [`usenet_guid`] passed it.
    pub guid: String,
    /// The indexer isn't public. The store makes every usenet item
    /// private either way (UN-0); this keeps the plugin's own word.
    pub private: bool,
    pub claims: Claims,
}

/// Lengths only: a title can be one a rule holds, and a GUID can be a
/// details page's address.
impl std::fmt::Debug for UsenetFind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsenetFind")
            .field("indexer", &self.indexer)
            .field("guid_len", &self.guid.len())
            .field("private", &self.private)
            .field("title_len", &self.claims.name.as_ref().map(String::len))
            .finish_non_exhaustive()
    }
}

/// One Newznab result as a usenet item, or `None` for one that isn't a
/// Newznab item or can't be kept. Every field stays a claim:
///
/// - **the title** is required: send finds the result again by it;
/// - **the GUID** must pass [`usenet_guid`], or nothing is kept;
/// - **the link** (Prowlarr's download URL, with its API key and the
///   indexer's) is dropped here, read only to compare with the GUID;
/// - **the size**, 1 byte to [`MAX_SIZE`];
/// - **the category**, the most specific Newznab id ([`usenet_category`]);
/// - **the posting date**, [`USENET_MIN_POSTED`] to a day past `now`;
/// - **the external ids** the details section allows (imdb, tmdb, tvdb);
///   the claims' clamp drops any other;
/// - **the poster is never read** (the parser doesn't keep it): it's
///   often an email address. Nor is a description or a publisher set,
///   which the details section forbids beside it (ENR-9).
///
/// No external reference is set: every usenet find is private, and a
/// private find stores none (L3). Send's reference is UN-3's design.
pub fn to_usenet(
    item: Item,
    ix: &Indexer,
    manager_host: &str,
    manager_key: &str,
    now: i64,
) -> Option<UsenetFind> {
    if item.kind != torznab::Kind::Usenet || !ix.usenet || !canonical_indexer(&ix.id) {
        return None;
    }
    let title = item
        .title
        .filter(|t| !t.is_empty() && !holds_key(t, manager_key))?;
    let guid = usenet_guid(
        item.guid.as_deref(),
        item.link.as_deref(),
        manager_host,
        manager_key,
    )?;
    let posted = item.published.filter(|t| {
        let last = now
            .saturating_add(MAX_FUTURE)
            .min(lodestar_sources::claims::MAX_POSTED_AT);
        (USENET_MIN_POSTED..=last).contains(t)
    });
    let details = Details {
        source_category: usenet_category(&item.categories),
        posted_at: posted,
        ..Details::default()
    };
    let claims = Claims {
        name: Some(title),
        size: item.size.filter(|s| (1..=MAX_SIZE).contains(s)),
        ids: item
            .ids
            .into_iter()
            .map(|(scheme, value)| ExternalId {
                scheme: scheme.to_owned(),
                value,
            })
            .collect(),
        details: (details != Details::default()).then(|| Box::new(details)),
        ..Claims::default()
    };
    Some(UsenetFind {
        indexer: ix.id.clone(),
        guid,
        private: ix.private,
        claims,
    })
}

impl UsenetFind {
    /// The find the pipeline stores as a usenet item, or `None` when the
    /// key can't be made (a part out of range, already refused above).
    pub fn into_found(self) -> Option<Found> {
        let posted = self.claims.details.as_ref().and_then(|d| d.posted_at);
        let mut found = Found::usenet(&self.indexer, &self.guid, STRATEGY_RSS)
            .ok()?
            .with_claims(self.claims)
            .with_provenance(Provenance {
                // None: a private find stores no reference (L3), and
                // every usenet find is private.
                external_ref: None,
                published_at: posted,
                confidence: Confidence::Low,
            });
        found.private_source = true;
        Some(found)
    }
}

/// Prowlarr's id form, the one UN-0 keys by: decimal digits, no leading
/// zero.
fn canonical_indexer(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && !(id.len() > 1 && id.starts_with('0'))
}

/// Whether `text` holds the manager's API key, in any case.
fn holds_key(text: &str, key: &str) -> bool {
    !key.is_empty()
        && text
            .as_bytes()
            .windows(key.len())
            .any(|w| w.eq_ignore_ascii_case(key.as_bytes()))
}

/// Words that mark a credential anywhere in a GUID.
const CREDENTIAL_WORDS: [&str; 6] = [
    "apikey", "api_key", "passkey", "token", "password", "secret",
];
/// Path segments that mark an address that fetches rather than shows.
const FETCH_SEGMENTS: [&str; 8] = [
    "api", "getnzb", "download", "dl", "nzb", "get", "rss", "sendnzb",
];

/// The GUID a usenet item is keyed and found again by, or `None` when it
/// can't be kept. `guid` and `link` are the parser's (credentials already
/// taken out of an http(s) URL). A GUID is kept only when it can't fetch
/// anything and carries no key:
///
/// - 1–[`USENET_MAX_GUID`] bytes, never equal to the link, never holding
///   the manager's key or a credential word ([`CREDENTIAL_WORDS`]);
/// - an opaque id (the parser keeps only letters, digits and `.:_-`), or
/// - an http(s) details page: no user info or fragment, not on the
///   manager's host, not a manager download ([`is_manager_download`]),
///   no path segment that fetches ([`FETCH_SEGMENTS`]) and no `.nzb`,
///   and no query but `guid=` (NZBGeek's `geekseek.php?guid=…`).
pub fn usenet_guid(
    guid: Option<&str>,
    link: Option<&str>,
    manager_host: &str,
    manager_key: &str,
) -> Option<String> {
    let g = guid?;
    if g.is_empty() || g.len() > USENET_MAX_GUID || Some(g) == link || holds_key(g, manager_key) {
        return None;
    }
    let lower = g.to_ascii_lowercase();
    if CREDENTIAL_WORDS.iter().any(|w| lower.contains(w)) || made_up(&lower) {
        return None;
    }
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        let opaque = g
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(&b));
        return opaque.then(|| g.to_owned());
    }
    let u = url::Url::parse(g).ok()?;
    if !u.username().is_empty() || u.password().is_some() || u.fragment().is_some() {
        return None;
    }
    if host_key(g).is_none_or(|h| h == manager_host) || is_manager_download(g) {
        return None;
    }
    let fetches = u.path_segments().is_none_or(|mut segs| {
        segs.any(|s| {
            let s = s.to_ascii_lowercase();
            FETCH_SEGMENTS.contains(&s.as_str()) || s.ends_with(".nzb")
        })
    });
    if fetches {
        return None;
    }
    if let Some(q) = u.query()
        && !q.split('&').all(|p| {
            p.split_once('=')
                .is_some_and(|(k, v)| k == "guid" && !v.is_empty())
        })
    {
        return None;
    }
    Some(g.to_owned())
}

/// A dashed UUID, bare or as `urn:uuid:…`. The *arr parsers make one up
/// (`Guid.NewGuid()`) for an item with no `<guid>`, a new one on every
/// poll, so it can't key an item or find it again. Newznab's own GUIDs
/// are undashed hex or a details page.
fn made_up(lower: &str) -> bool {
    let u = lower.strip_prefix("urn:uuid:").unwrap_or(lower);
    let groups: Vec<&str> = u.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The most specific Newznab category the result names: the first
/// sub-category (an id that isn't a multiple of 1,000, like `5040`
/// TV/HD), else the first one (`5000` TV). In decimal, as the feed
/// writes it.
pub fn usenet_category(categories: &[u32]) -> Option<String> {
    categories
        .iter()
        .find(|c| **c % 1000 != 0)
        .or(categories.first())
        .map(u32::to_string)
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
    parse_indexers_with(manager, body, false)
}

/// [`parse_indexers`], and with `usenet` Prowlarr's enabled Usenet
/// indexers too (UN-1), under the same [`MAX_INDEXERS`]. Jackett lists
/// none.
pub fn parse_indexers_with(
    manager: Manager,
    body: &[u8],
    usenet: bool,
) -> Result<Vec<Indexer>, ListError> {
    if body.len() > MAX_LIST_BYTES {
        return Err(ListError::TooBig);
    }
    let mut list = match manager {
        Manager::Prowlarr => prowlarr_list(body, usenet)?,
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
fn prowlarr_list(body: &[u8], usenet: bool) -> Result<Vec<Indexer>, ListError> {
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

    struct ListVisitor {
        usenet: bool,
    }

    impl<'de> serde::de::DeserializeSeed<'de> for ListVisitor {
        type Value = List;

        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<List, D::Error> {
            d.deserialize_seq(self)
        }
    }

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
                let is_usenet = match raw.protocol.as_deref() {
                    Some("torrent") => false,
                    Some("usenet") if self.usenet => true,
                    _ => continue,
                };
                if raw.supports_rss == Some(false) {
                    continue;
                }
                let id = id.to_string();
                // Deduplicated before the cap, so repeats can't crowd
                // out real indexers.
                if out.iter().all(|i: &Indexer| i.id != id) {
                    out.push(Indexer {
                        id,
                        private: raw.privacy.as_deref() != Some("public"),
                        usenet: is_usenet,
                    });
                }
            }
            // The rest is read, unkept, so the document is still checked.
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(List(out))
        }
    }

    let mut de = serde_json::Deserializer::from_slice(body);
    let list = serde::de::DeserializeSeed::deserialize(ListVisitor { usenet }, &mut de)
        .and_then(|l| de.end().map(|()| l));
    list.map(|l| l.0).map_err(|_| ListError::Malformed)
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
                usenet: false,
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

#[cfg(test)]
mod usenet_poll_tests {
    use super::*;
    use tokio::time::Instant;

    const MIN: Duration = Duration::from_secs(60);

    /// One poll of `id` at `at`: due, then a sent request.
    fn ask(p: &mut UsenetPolls, id: &str, at: Instant) -> bool {
        let due = p.due(id, at);
        if due {
            p.hit(id, at);
        }
        due
    }

    #[test]
    fn a_whole_page_waits_an_hour_and_a_failure_retries_within_a_daily_bound() {
        let mut p = UsenetPolls::default();
        let t0 = Instant::now();
        // Asked, all sent: not again within the hour.
        assert!(ask(&mut p, "7", t0));
        p.done("7", t0);
        assert!(!ask(&mut p, "7", t0 + 15 * MIN));
        assert!(!ask(&mut p, "7", t0 + 45 * MIN));
        assert!(ask(&mut p, "7", t0 + 60 * MIN));
        // Not done (a failure, or a page cut short): the next poll asks
        // again, 15 minutes on.
        assert!(ask(&mut p, "7", t0 + 75 * MIN));
        // Another indexer is counted apart.
        assert!(ask(&mut p, "8", t0 + 75 * MIN));

        // Failing every 15 minutes: at most USENET_DAILY_HITS in a day.
        let mut q = UsenetPolls::default();
        let mut asked = 0;
        for i in 0..(4 * 24) {
            if ask(&mut q, "9", t0 + i * 15 * MIN) {
                asked += 1;
            }
        }
        assert_eq!(asked, USENET_DAILY_HITS);
        assert!(!ask(&mut q, "9", t0 + 24 * 60 * MIN - MIN));
        // A day after the first hit, one more is allowed.
        assert!(ask(&mut q, "9", t0 + 24 * 60 * MIN));
    }

    #[test]
    fn polls_stay_hourly_whatever_the_answers_latency() {
        // The grant is stamped, not the answer, and rounds that land a
        // minute early or late still find the indexer due an hour on.
        let mut p = UsenetPolls::default();
        let t0 = Instant::now();
        let mut polls = 0;
        for round in 0..(4 * 24) {
            // Every other polled round (0, 8, 16, …) lands a minute late,
            // so the next one comes 59 minutes after it: due only with
            // the slack.
            let jitter = if (round / 4) % 2 == 0 {
                MIN
            } else {
                Duration::ZERO
            };
            let at = t0 + round * 15 * MIN + jitter;
            if ask(&mut p, "7", at) {
                polls += 1;
                p.done("7", at);
            }
        }
        assert_eq!(polls, 24, "hourly, not every 75 minutes");
    }

    #[test]
    fn a_request_never_sent_isnt_a_hit() {
        let blocked = Err(HttpError::Blocked {
            until: Instant::now(),
        });
        assert_eq!(answer(blocked), Err(Failure::NotSent));
        assert_eq!(answer(Err(HttpError::NoEgress)), Err(Failure::NotSent));
        let timeout = Err(HttpError::Transport("timeout".into()));
        assert_eq!(answer(timeout), Err(Failure::Other));
        // Blocked for half a day: every round is due and answered
        // `NotSent`, nothing is counted, and the day's hits are all there
        // when the block lifts.
        let mut p = UsenetPolls::default();
        let t0 = Instant::now();
        for i in 0..USENET_DAILY_HITS as u32 {
            let at = t0 + i * 15 * MIN;
            assert!(p.due("7", at));
            p.answered("7", at, &Err(Failure::NotSent));
        }
        assert!(p.due("7", t0 + 12 * 60 * MIN));
        // Any answer that went out counts: a failure as much as a page.
        let mut q = UsenetPolls::default();
        for i in 0..USENET_DAILY_HITS as u32 {
            let at = t0 + i * 15 * MIN;
            assert!(q.due("7", at));
            let page = if i % 2 == 0 {
                Err(Failure::Other)
            } else {
                Ok(Vec::new())
            };
            q.answered("7", at, &page);
        }
        assert!(!q.due("7", t0 + 12 * 60 * MIN), "48 hits counted");
    }

    #[test]
    fn the_map_is_capped() {
        let mut p = UsenetPolls::default();
        let t0 = Instant::now();
        for i in 0..MAX_INDEXERS {
            assert!(ask(&mut p, &i.to_string(), t0));
        }
        // A new id past the cap waits; a known one is still asked.
        assert!(!p.due("new", t0 + 15 * MIN));
        assert!(p.due("1", t0 + 15 * MIN));
        // A day on, the old entries have aged out.
        assert!(p.due("new", t0 + 24 * 60 * MIN));
    }
}
