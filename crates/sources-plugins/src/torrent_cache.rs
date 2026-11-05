//! Torrent-cache enrichment (architecture §18.2 enrichment #3, §18.5,
//! §18.7; v0.6 sources plan SRC-13, §5 Q3 and Q8).
//!
//! A torrent cache serves a `.torrent` file by info hash. The file holds
//! the info dict, so a cached copy whose info dict hashes to the key *is*
//! the torrent's verified metadata (Q8), with the fields BEP 9 never
//! carries (`creation date`, `comment`, `announce-list`, web seeds).
//!
//! **Each lookup tells the cache which hash you're interested in** (Q3),
//! so the plugin is held tight:
//! - **Off by default, External consent** (§18.7): enabling it shows the
//!   cache list, and the server refuses to turn it on until that list is
//!   acknowledged (SRC-4).
//! - **The cache list is the user's** and starts empty. [`SUGGESTED`] is
//!   shown beside it in the settings form, never enabled for them.
//! - **Never a `private_source` hash, at any layer.** A [`Lookup`] can't
//!   be made for one ([`Lookup::new`] says `None`), a request is built
//!   only from a `Lookup` ([`TorrentCache::request`]), and the pipeline's
//!   cursor (`lodestar_pipeline::torrent_cache`) never offers one. A
//!   private torrent's hash leaking to a third party is what BEP 27 and
//!   the trackers' rules forbid.
//! - **Only user-interest and unverified items** are looked up ([`Why`]):
//!   never the crawl's own keys, which BEP 9 fetches anyway.
//!
//! **Politeness and caps.** Every request goes through `ctx.kit.http`
//! (SRC-5's egress, narrowed to the listed hosts on public addresses by
//! [`TorrentCache::endpoints`]). Each cache is one poll unit: at most one
//! lookup a minute per cache, and one request every 10 s per host, with
//! the kit's back-off and "blocked by source" state. The body is streamed
//! under [`STREAM`]: at most 8 MiB, within 30 s, and no stall over 10 s.
//! Redirects aren't followed.
//!
//! **The reply is untrusted.** It's parsed by `lodestar-metainfo` (fuzzed)
//! within [`LIMITS`], and used only when its info dict's SHA-1, or its
//! truncated SHA-256 for a v2 key, is the key. A file that hashes to
//! anything else is refused and counted ([`Stats::mismatched`]), and the
//! next cache is asked. A [`Verified`] can only be made here, after that
//! check. It still goes through the full filter pipeline before storage
//! (the pipeline's `torrent_cache::judge`, SRC-2's path).
//!
//! **Host wiring** (the sources wiring slice): the host builds the
//! plugin from the saved config with a [`Queue`] it feeds from the
//! pipeline's cursor and from user interest, and takes each [`Verified`]
//! to the pipeline's judge.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_integrations::net::{self, NetPolicy};
use lodestar_metainfo::{InfoHash, Limits, TorrentFile};
use lodestar_sources::claims::{self, Claims, Confidence, Provenance};
use lodestar_sources::http::{StreamLimits, host_key};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Endpoints, HostRate, HttpError, Kind,
    Manifest, Network, PluginError, Policy, Priority, Request, Risk, SourceCtx, SourcePlugin,
};
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "torrent_cache";
/// What provenance records for metadata from a cache (Q8).
pub const PROVENANCE: &str = "torrent cache";
/// Caches one instance asks.
pub const MAX_CACHES: usize = 8;
/// The longest address template.
pub const MAX_TEMPLATE: usize = 512;
/// Lookups waiting in each lane of the [`Queue`]. A push past it is
/// dropped and counted: the cursor offers the key again next round.
pub const QUEUE: usize = 256;
/// Where the key goes in a template: lower-case hex.
pub const HASH: &str = "{hash}";
/// Where the key goes in a template: upper-case hex.
pub const HASH_UPPER: &str = "{HASH}";
/// The longest tracker URL kept from a cached file.
pub const MAX_TRACKER_URL: usize = 512;
/// A path segment this long, all letters and digits, is taken for a
/// passkey (as `full_scrape`'s rule): a tracker URL with one isn't kept.
pub const PASSKEY_MIN: usize = 16;
/// Creation dates outside 2001-07-01 (BitTorrent's first release) to
/// 2100 aren't believed.
pub const MIN_CREATED: i64 = 993_945_600;
pub const MAX_CREATED: i64 = 4_102_444_800;

/// One reply: 8 MiB within 30 s, no wait over 10 s for the headers or
/// any chunk. A `.torrent` is rarely over 1 MiB; a v2 file with piece
/// layers for a very large torrent is the reason for the headroom.
pub const STREAM: StreamLimits = StreamLimits {
    max_bytes: 8 * 1024 * 1024,
    timeout: Duration::from_secs(30),
    idle: Duration::from_secs(10),
};

/// `lodestar-metainfo`'s limits with the whole file held to [`STREAM`]'s
/// byte cap.
pub const LIMITS: Limits = Limits {
    max_torrent_bytes: STREAM.max_bytes as usize,
    ..Limits::DEFAULT
};

/// A cache shown in the settings form as a suggestion. Never enabled for
/// the user: they add it to their own list if they choose to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suggestion {
    pub name: &'static str,
    pub template: &'static str,
}

/// Shown, not enabled (Q3). Each is checked by [`Config::new`]'s rules in
/// this module's tests. Its terms are the owner's to check before the
/// plugin ships (Q2, `docs/decisions/2026-11-05-SRC-13.md`).
pub const SUGGESTED: &[Suggestion] = &[Suggestion {
    name: "iTorrents",
    template: "https://itorrents.org/torrent/{HASH}.torrent",
}];

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Torrent cache",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    // None built in: the caches are the ones the user lists
    // (`TorrentCache::endpoints`).
    endpoints: &[],
    // A lookup tells a third party which hashes you want.
    risk: Risk::Medium,
    // §18.7: "torrent cache" is in the External class.
    consent: Consent::External,
    default_priority: Priority::Low,
    enabled_by_default: false,
};

/// The settings form (§10.10). The list starts empty.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"caches":{"type":"array","title":"Torrent caches","description":"Addresses that serve a .torrent by its info hash, with {hash} (lower case) or {HASH} (upper case) where the hash goes, such as https://cache.example/torrent/{HASH}.torrent. Each lookup tells the cache which torrent you want. Private torrents are never looked up.","items":{"type":"string","format":"uri-template","maxLength":512},"maxItems":8,"uniqueItems":true,"default":[],"examples":[["https://itorrents.org/torrent/{HASH}.torrent"]]}},"required":["caches"],"additionalProperties":false}"#;

/// One lookup a minute per cache, one request every 10 s per host, and a
/// back-off from a minute to six hours: a third party's free service.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(60),
        per_host: HostRate::every(Duration::from_secs(10)),
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
    },
    config_schema: CONFIG_SCHEMA,
    // Only the coordinator: more workers would multiply what each cache
    // learns (§19.5).
    distribution: Distribution::Coordinator,
    max_agents: 1,
    ..Policy::STRICT
};

/// One cache, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    /// The template up to the placeholder.
    head: String,
    /// The template after it.
    tail: String,
    upper: bool,
    /// The limiter's, the poll unit's and the egress host list's key.
    host: String,
}

impl Cache {
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The address for `key`.
    fn url(&self, key: &InfoHash) -> String {
        let hex = key.to_string();
        let hex = if self.upper {
            hex.to_ascii_uppercase()
        } else {
            hex
        };
        format!("{}{hex}{}", self.head, self.tail)
    }
}

/// The plugin's settings, checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    caches: Vec<Cache>,
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
    caches: Vec<String>,
}

impl Config {
    /// Checks each template:
    /// - at most [`MAX_TEMPLATE`] bytes, and exactly one placeholder,
    ///   [`HASH`] or [`HASH_UPPER`], after the host;
    /// - https, with a host, no user name or password and no fragment: a
    ///   lookup over plain http would show the hash to the whole path;
    /// - not a literal LAN, loopback or link-local address: these are
    ///   public caches;
    /// - at most [`MAX_CACHES`], none repeated.
    ///
    /// An empty list is valid: it's where the list starts.
    pub fn new<S: AsRef<str>>(templates: &[S]) -> Result<Config, ConfigError> {
        if templates.len() > MAX_CACHES {
            return Err(ConfigError("at most 8 caches"));
        }
        let mut caches: Vec<Cache> = Vec::with_capacity(templates.len());
        for t in templates {
            let c = cache(t.as_ref().trim())?;
            if caches.contains(&c) {
                return Err(ConfigError("a cache is listed twice"));
            }
            caches.push(c);
        }
        Ok(Config { caches })
    }

    /// The settings as the form saves them (see [`CONFIG_SCHEMA`]).
    pub fn from_json(json: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = serde_json::from_str(json)
            .map_err(|_| ConfigError("the settings don't match the form"))?;
        Config::new(&raw.caches)
    }

    pub fn caches(&self) -> &[Cache] {
        &self.caches
    }
}

fn cache(raw: &str) -> Result<Cache, ConfigError> {
    if raw.len() > MAX_TEMPLATE {
        return Err(ConfigError("a cache address is too long"));
    }
    let lower = raw.matches(HASH).count();
    let upper = raw.matches(HASH_UPPER).count();
    if lower + upper != 1 {
        return Err(ConfigError(
            "a cache address needs {hash} or {HASH} exactly once",
        ));
    }
    let (mark, is_upper) = if upper == 1 {
        (HASH_UPPER, true)
    } else {
        (HASH, false)
    };
    let at = raw
        .find(mark)
        .ok_or(ConfigError("a cache address has no {hash}"))?;
    let (head, tail) = (&raw[..at], &raw[at + mark.len()..]);
    // A stand-in key: what the address is for every real one, apart from
    // the hex itself.
    let probe = format!("{head}{}{tail}", "0".repeat(40));
    let url = url::Url::parse(&probe).map_err(|_| ConfigError("a cache isn't a valid address"))?;
    if url.scheme() != "https" {
        return Err(ConfigError("a cache address must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError(
            "a cache address has a user name or password in it",
        ));
    }
    if url.fragment().is_some() {
        return Err(ConfigError("a cache address has a fragment"));
    }
    // The placeholder must be past the host: in the path or the query.
    let authority_end = head
        .find("://")
        .and_then(|s| head[s + 3..].find(['/', '?']).map(|e| s + 3 + e));
    if authority_end.is_none() {
        return Err(ConfigError("{hash} must be in the path or the query"));
    }
    net::check_url(&url, NetPolicy::notification(false))
        .map_err(|_| ConfigError("a cache address isn't a public one"))?;
    let host = host_key(url.as_str()).ok_or(ConfigError("a cache address has no host"))?;
    Ok(Cache {
        head: head.to_owned(),
        tail: tail.to_owned(),
        upper: is_upper,
        host,
    })
}

/// Why a key may be looked up (Q3): nothing else is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A user is waiting on it (a search hit, a saved search, a detail
    /// view). Looked up before any unverified item.
    Interest,
    /// An unverified item with no metadata yet.
    Unverified,
}

/// A key that may be sent to a cache. The only way a request is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lookup {
    key: InfoHash,
    why: Why,
}

impl Lookup {
    /// `None` for a `private_source` key (§18.7): a lookup leaks the hash,
    /// so no `Lookup`, and so no request, can exist for one.
    pub fn new(key: InfoHash, why: Why, private_source: bool) -> Option<Lookup> {
        (!private_source).then_some(Lookup { key, why })
    }

    pub fn key(&self) -> InfoHash {
        self.key
    }

    pub fn why(&self) -> Why {
        self.why
    }
}

/// Where the host hands the plugin its lookups: two bounded lanes, user
/// interest first. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Queue {
    interest: mpsc::Sender<Lookup>,
    unverified: mpsc::Sender<Lookup>,
    stats: Arc<Stats>,
}

impl Queue {
    /// Queues `lookup` without waiting. `false` when its lane is full or
    /// the plugin is gone; a full lane counts [`Stats::dropped`].
    pub fn push(&self, lookup: Lookup) -> bool {
        let lane = match lookup.why {
            Why::Interest => &self.interest,
            Why::Unverified => &self.unverified,
        };
        match lane.try_send(lookup) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

struct Lanes {
    interest: mpsc::Receiver<Lookup>,
    unverified: mpsc::Receiver<Lookup>,
}

impl Lanes {
    /// The next lookup, interest first; `None` once the host has dropped
    /// every [`Queue`] and both lanes are empty.
    async fn next(&mut self) -> Option<Lookup> {
        tokio::select! {
            biased;
            Some(l) = self.interest.recv() => Some(l),
            Some(l) = self.unverified.recv() => Some(l),
            else => None,
        }
    }
}

/// What the plugin has done, for the status page. Never holds a hash.
#[derive(Debug, Default)]
pub struct Stats {
    /// Lookups taken from the queue.
    pub lookups: AtomicU64,
    /// Lookups that came back verified.
    pub verified: AtomicU64,
    /// Files whose info dict hashed to something other than the key.
    pub mismatched: AtomicU64,
    /// Replies that didn't parse as a `.torrent` within [`LIMITS`].
    pub invalid: AtomicU64,
    /// Replies over [`STREAM`]'s caps, transport failures, refusals and
    /// statuses other than 200 and 404.
    pub failed: AtomicU64,
    /// Lookups no cache had.
    pub missed: AtomicU64,
    /// Lookups dropped because their lane was full.
    pub dropped: AtomicU64,
}

/// A cached `.torrent` whose info dict hashes to its lookup's key. Made
/// only by [`TorrentCache::lookup`], after that check.
#[derive(Debug, Clone)]
pub struct Verified {
    lookup: Lookup,
    torrent: TorrentFile,
    v2: bool,
    host: String,
}

impl Verified {
    pub fn key(&self) -> InfoHash {
        self.lookup.key
    }

    pub fn why(&self) -> Why {
        self.lookup.why
    }

    /// The parsed file; its info dict is the verified metadata.
    pub fn torrent(&self) -> &TorrentFile {
        &self.torrent
    }

    pub fn into_torrent(self) -> TorrentFile {
        self.torrent
    }

    /// The key is the info dict's truncated SHA-256 (a v2 swarm), not its
    /// SHA-1.
    pub fn is_v2(&self) -> bool {
        self.v2
    }

    /// The cache's host, for the status page.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The fields outside the info dict, as claims: the trackers and the
    /// comment. Trackers are kept only for a torrent that isn't marked
    /// `private`, and only those with no user name, query or
    /// passkey-shaped part, since a private tracker's address holds the
    /// uploader's passkey. Web seeds and `created by` have no claim field
    /// yet (`docs/decisions/2026-11-05-SRC-13.md`).
    pub fn claims(&self) -> Claims {
        let trackers = if self.torrent.info.private {
            Vec::new()
        } else {
            let mut out: Vec<String> = Vec::new();
            for url in self.torrent.announce.iter().flatten() {
                if out.len() >= claims::MAX_TRACKERS {
                    break;
                }
                if public_tracker(url) && !out.contains(url) {
                    out.push(url.clone());
                }
            }
            out
        };
        let mut c = Claims {
            trackers,
            description: self.torrent.comment.clone().filter(|c| !c.is_empty()),
            ..Claims::default()
        };
        c.clamp();
        c
    }

    /// Where it came from: the file's `creation date` when it's believable
    /// (the uploader's claim), at Medium confidence, since the fields
    /// outside the info dict aren't covered by the hash.
    pub fn provenance(&self) -> Provenance {
        Provenance {
            external_ref: None,
            published_at: self
                .torrent
                .creation_date
                .filter(|d| (MIN_CREATED..=MAX_CREATED).contains(d)),
            confidence: Confidence::Medium,
        }
    }
}

fn public_tracker(raw: &str) -> bool {
    if raw.len() > MAX_TRACKER_URL {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https" | "udp")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url
            .path_segments()
            .into_iter()
            .flatten()
            .any(|s| s.len() >= PASSKEY_MIN && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Why one cache didn't give a verified file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    /// 404: it doesn't have it.
    NotFound,
    /// The file hashed to another key.
    Mismatch,
    /// Not a `.torrent` within [`LIMITS`].
    Invalid,
    /// Too large, too slow, refused, blocked or another status.
    Failed,
    /// The plugin's egress is down: no cache can be asked.
    EgressDown,
}

/// The torrent-cache plugin.
pub struct TorrentCache {
    config: Config,
    lanes: Arc<Mutex<Lanes>>,
    out: mpsc::Sender<Verified>,
    stats: Arc<Stats>,
}

impl std::fmt::Debug for TorrentCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorrentCache")
            .field("caches", &self.config.caches.len())
            .finish_non_exhaustive()
    }
}

impl TorrentCache {
    /// The plugin, the [`Queue`] that feeds it, and the channel each
    /// [`Verified`] file comes out on. The plugin waits for room there, so
    /// a slow judge slows the lookups rather than filling memory.
    pub fn new(config: Config) -> (TorrentCache, Queue, mpsc::Receiver<Verified>) {
        let (interest_tx, interest) = mpsc::channel(QUEUE);
        let (unverified_tx, unverified) = mpsc::channel(QUEUE);
        let (out, verified) = mpsc::channel(16);
        let stats = Arc::new(Stats::default());
        let queue = Queue {
            interest: interest_tx,
            unverified: unverified_tx,
            stats: Arc::clone(&stats),
        };
        let plugin = TorrentCache {
            config,
            lanes: Arc::new(Mutex::new(Lanes {
                interest,
                unverified,
            })),
            out,
            stats,
        };
        (plugin, queue, verified)
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Where the plugin's egress may send requests (SRC-5): the listed
    /// caches' hosts, on public addresses only. `None` while the list is
    /// empty, so nothing may be reached.
    pub fn endpoints(&self) -> Option<Endpoints> {
        if self.config.caches.is_empty() {
            return None;
        }
        let hosts: Vec<&str> = self.config.caches.iter().map(|c| c.host.as_str()).collect();
        Some(Endpoints::user_entered(NetPolicy::notification(false)).only_hosts(&hosts))
    }

    /// The request for `lookup` at `cache`. It takes a [`Lookup`], so it
    /// can't be built for a `private_source` key.
    pub fn request(cache: &Cache, lookup: &Lookup) -> Request {
        Request::get(cache.url(&lookup.key)).header("Accept", "application/x-bittorrent")
    }

    /// Asks each cache in turn until one gives a file that hashes to the
    /// key. `Err` carries the last cache's miss.
    pub async fn lookup(&self, http: &Client, lookup: &Lookup) -> Result<Verified, Miss> {
        let mut last = Miss::NotFound;
        for cache in &self.config.caches {
            match self.ask(http, cache, lookup).await {
                Ok(v) => return Ok(v),
                Err(Miss::EgressDown) => return Err(Miss::EgressDown),
                Err(m) => last = m,
            }
        }
        Err(last)
    }

    async fn ask(&self, http: &Client, cache: &Cache, lookup: &Lookup) -> Result<Verified, Miss> {
        let req = TorrentCache::request(cache, lookup);
        let fail = |m: Miss| {
            let c = match m {
                Miss::Mismatch => &self.stats.mismatched,
                Miss::Invalid => &self.stats.invalid,
                Miss::Failed | Miss::EgressDown => &self.stats.failed,
                Miss::NotFound => return m,
            };
            c.fetch_add(1, Ordering::Relaxed);
            m
        };
        let mut res = match http.poll_stream(&cache.host, &req, STREAM).await {
            Ok(r) => r,
            Err(HttpError::EgressDown(_) | HttpError::NoEgress) => {
                return Err(fail(Miss::EgressDown));
            }
            Err(_) => return Err(fail(Miss::Failed)),
        };
        match res.status {
            200 => {}
            404 | 410 => return Err(Miss::NotFound),
            _ => return Err(fail(Miss::Failed)),
        }
        let mut body = Vec::new();
        loop {
            match res.next_chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(_) => return Err(fail(Miss::Failed)),
            }
        }
        let torrent = TorrentFile::parse_with(&body, &LIMITS).map_err(|_| fail(Miss::Invalid))?;
        let v2 = match verify(&torrent, &lookup.key) {
            Some(v2) => v2,
            None => return Err(fail(Miss::Mismatch)),
        };
        Ok(Verified {
            lookup: *lookup,
            torrent,
            v2,
            host: cache.host.clone(),
        })
    }
}

/// Whether `torrent`'s info dict is `key`'s: `Some(false)` when its SHA-1
/// is the key, `Some(true)` when its truncated SHA-256 is (Q8), `None`
/// otherwise.
pub fn verify(torrent: &TorrentFile, key: &InfoHash) -> Option<bool> {
    let info = &torrent.info;
    if info.v1.as_ref() == Some(key) {
        Some(false)
    } else if info.v2.map(|h| h.truncated()).as_ref() == Some(key) {
        Some(true)
    } else {
        None
    }
}

impl SourcePlugin for TorrentCache {
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

impl TorrentCache {
    async fn run_inner(&self, ctx: SourceCtx) -> Result<(), PluginError> {
        let SourceCtx {
            mut shutdown, kit, ..
        } = ctx;
        if self.config.caches.is_empty() {
            return Err(PluginError(
                "needs setup: no torrent cache is listed".to_owned(),
            ));
        }
        // One run at a time holds the lanes; a second waits for the first
        // to stop.
        let mut lanes = tokio::select! {
            _ = shutdown.requested() => return Ok(()),
            l = self.lanes.lock() => l,
        };
        loop {
            let lookup = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                l = lanes.next() => l,
            };
            let Some(lookup) = lookup else {
                return Ok(());
            };
            self.stats.lookups.fetch_add(1, Ordering::Relaxed);
            let found = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                r = self.lookup(&kit.http, &lookup) => r,
            };
            match found {
                Ok(v) => {
                    self.stats.verified.fetch_add(1, Ordering::Relaxed);
                    tokio::select! {
                        _ = shutdown.requested() => return Ok(()),
                        sent = self.out.send(v) => if sent.is_err() { return Ok(()) },
                    }
                }
                Err(Miss::NotFound) => {
                    self.stats.missed.fetch_add(1, Ordering::Relaxed);
                }
                // The egress paces its own refusals (SRC-14b), and each
                // cache's poll gate spaces the next try.
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_pass_the_rules() {
        for s in SUGGESTED {
            Config::new(&[s.template]).unwrap();
        }
    }

    #[test]
    fn placeholder_goes_in_hex() {
        let c = Config::new(&[
            "https://a.example/t/{hash}.torrent",
            "https://b.example/get?h={HASH}",
        ])
        .unwrap();
        let key = InfoHash([0xab; 20]);
        assert_eq!(
            c.caches()[0].url(&key),
            format!("https://a.example/t/{}.torrent", "ab".repeat(20))
        );
        assert_eq!(
            c.caches()[1].url(&key),
            format!("https://b.example/get?h={}", "AB".repeat(20))
        );
    }
}
