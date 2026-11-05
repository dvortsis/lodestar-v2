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
//! - **Off by default, External consent** (§18.7): the server refuses to
//!   turn it on until its consent is acknowledged (SRC-4). That
//!   acknowledgement covers the configured cache addresses exactly, path
//!   and query included, so adding or changing one asks again: each
//!   plugin's manifest lists its own config's templates as its endpoints
//!   ([`TorrentCache::manifest`]), and the consent version holds the
//!   manifest's endpoints and is compared exactly
//!   (`docs/decisions/2026-11-05-SRC-13b.md`, `2026-11-05-SRC-13c.md`).
//! - **Settings are written through the API** (`PATCH /sources/{id}`'s
//!   `config`, SRC-13c), checked by [`Config::from_json`] and saved as
//!   [`Config::to_json`]. They apply at the next start.
//! - **The cache list is the user's** and starts empty. [`SUGGESTED`] is
//!   shown beside it in the settings form, never enabled for them.
//!
//! **ToS (Q2, `docs/owner-decisions.md` §2.28).** Lodestar ships no cache
//! the user didn't enter. No suggestion is listed until the owner has
//! checked a cache's terms allow automated lookups
//! (`docs/decisions/2026-11-05-SRC-13.md`).
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
//! within [`LIMITS`]. The info dict is read from its raw span with no
//! encoding hint ([`TorrentFile::raw_info`]), as BEP 9 delivers it: the
//! file's top-level `encoding` key isn't covered by the hash, so a cache
//! can't choose how a name decodes. It's used only when its SHA-1, or its
//! truncated SHA-256 for a v2 key, is the key. A file that hashes to
//! anything else is refused and counted ([`Stats::mismatched`]), and the
//! next cache is asked. A [`Verified`] can only be made here, after that
//! check. It still goes through the full filter pipeline before storage
//! (the pipeline's `torrent_cache::judge`, SRC-2's path).
//!
//! **Host wiring** (SRC-13b, `crates/lodestar/src/torrent_cache.rs`): the
//! host builds the plugin from its saved config with a [`Queue`] it feeds
//! from the pipeline's cursor and from user interest, with each key's
//! `private_source` taken from the store, and takes each [`Verified`]'s
//! raw info dict to the pipeline's judge.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_integrations::net::{self, NetPolicy};
use lodestar_metainfo::{Info, InfoHash, Limits, ParseOptions, TorrentFile};
use lodestar_sources::claims::{self, Claims, Confidence, Provenance};
use lodestar_sources::http::{HostState, StreamLimits, host_key};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, ConfigRefusal, Consent, Distribution, Endpoint, Endpoints,
    HostRate, HttpError, Kind, Manifest, Network, PluginError, Policy, Priority, Request, Risk,
    SourceCtx, SourcePlugin,
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

/// `lodestar-metainfo`'s limits for a cache's reply: the whole file held
/// to [`STREAM`]'s byte cap, and half the default bencode values and
/// files, which still fits any torrent an 8 MiB file can hold.
pub const LIMITS: Limits = Limits {
    max_torrent_bytes: STREAM.max_bytes as usize,
    max_items: 1_000_000,
    max_files: 100_000,
    ..Limits::DEFAULT
};

/// A cache shown in the settings form as a suggestion. Never enabled for
/// the user: they add it to their own list if they choose to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suggestion {
    pub name: &'static str,
    pub template: &'static str,
}

/// Shown, not enabled (Q3). Each must pass [`Config::new`]'s rules (this
/// module's tests) and appear in [`CONFIG_SCHEMA`]'s `examples`. Empty
/// until the owner has checked a cache's terms (Q2,
/// `docs/decisions/2026-11-05-SRC-13.md`).
pub const SUGGESTED: &[Suggestion] = &[];

/// The manifest with no cache listed. A plugin's own
/// ([`SourcePlugin::manifest`]) lists its config's hosts as endpoints.
pub const MANIFEST: Manifest = Manifest {
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
    always_adult: false,
    finds_private: false,
};

/// The settings form (§10.10). The list starts empty.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"caches":{"type":"array","title":"Torrent caches","description":"Addresses that serve a .torrent by its info hash, with {hash} (lower case) or {HASH} (upper case) where the hash goes, such as https://cache.example/torrent/{HASH}.torrent. Each lookup tells the cache which torrent you want. Private torrents are never looked up.","items":{"type":"string","format":"uri-template","maxLength":512},"maxItems":8,"uniqueItems":true,"default":[],"examples":[]}},"required":["caches"],"additionalProperties":false}"#;

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
    /// The template as entered, trimmed: what's saved, what the consent
    /// dialog shows and what an acknowledgement covers
    /// ([`TorrentCache::manifest`]).
    template: String,
    /// The address as sent, host in lower case, and the placeholder's
    /// case: two templates that send the same requests are one cache.
    same: (String, bool),
}

impl Cache {
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The address template, as saved and shown.
    pub fn template(&self) -> &str {
        &self.template
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
    /// - printable ASCII only, starting `https://`: the consent dialog
    ///   shows it as written, so it can't hide a part behind a control,
    ///   bidirectional or look-alike character (a non-ASCII host is
    ///   written in its `xn--` form), and with no `\`, which the URL
    ///   parser reads as `/`;
    /// - the address as written is the address requested: no `%`, no `.`
    ///   or `..` path segment, nothing the parser rewrites, and the host
    ///   as the host contacted: no percent-encoding,
    ///   no numeric form the parser rewrites (`0x08080808`), and no
    ///   explicit `:443`;
    /// - https, with a host, no user name or password and no fragment: a
    ///   lookup over plain http would show the hash to the whole path;
    /// - not a literal LAN, loopback or link-local address: these are
    ///   public caches;
    /// - at most [`MAX_CACHES`], none repeated (two that send the same
    ///   requests, such as `A.example` and `a.example`, are one).
    ///
    /// An empty list is valid: it's where the list starts.
    pub fn new<S: AsRef<str>>(templates: &[S]) -> Result<Config, ConfigError> {
        if templates.len() > MAX_CACHES {
            return Err(ConfigError("at most 8 caches"));
        }
        let mut caches: Vec<Cache> = Vec::with_capacity(templates.len());
        for t in templates {
            let c = cache(t.as_ref().trim())?;
            if caches.iter().any(|d| d.same == c.same) {
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

    /// The settings as they're saved: `{"caches":[...]}`, each template
    /// trimmed. [`Config::from_json`] reads it back to the same config.
    pub fn to_json(&self) -> String {
        let caches: Vec<&str> = self.caches.iter().map(|c| c.template.as_str()).collect();
        serde_json::json!({ "caches": caches }).to_string()
    }
}

fn cache(raw: &str) -> Result<Cache, ConfigError> {
    if raw.len() > MAX_TEMPLATE {
        return Err(ConfigError("a cache address is too long"));
    }
    if !raw.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ConfigError(
            "a cache address is plain ASCII with no spaces: write a non-ASCII host in its xn-- form",
        ));
    }
    // The URL parser reads `\` as `/`, so `https://a.example\@b.example/`
    // would show one host and reach another.
    // A `%` is refused anywhere: the shown text must be what's sent, and
    // a server can decode `%2F` or `%2e` into a path it never showed.
    if raw.contains('%') {
        return Err(ConfigError(
            "a cache address can't hold a %: write it without percent-encoding",
        ));
    }
    if raw.contains('\\') {
        return Err(ConfigError("a cache address can't hold a backslash"));
    }
    if !raw.starts_with("https://") {
        return Err(ConfigError("a cache address must start with https://"));
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
    let Some(authority_end) = authority_end else {
        return Err(ConfigError("{hash} must be in the path or the query"));
    };
    // The host as written must be the host contacted: the consent list
    // shows the template as typed, while the parser decodes `%2E`, maps
    // full-width dots and reads `0x08080808` as 8.8.8.8. So the authority
    // must already be in the parser's plain form (any case).
    let written = &head["https://".len()..authority_end];
    let parsed = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        (None, _) => return Err(ConfigError("a cache address has no host")),
    };
    if !written.eq_ignore_ascii_case(&parsed) {
        return Err(ConfigError(
            "write a cache address's host in its plain form, with no encoding, and no port 443",
        ));
    }
    // The path and query as written must be what's requested: the parser
    // resolves `.` and `..` segments (`/~alice/../~bob/` requests
    // `/~bob/`, a different party on a per-user host) and may encode
    // characters. Only the host's case may differ.
    let rest = &probe["https://".len() + written.len()..];
    if url.as_str() != format!("https://{parsed}{rest}") {
        return Err(ConfigError(
            "write a cache address as it's sent: no . or .. path segments, and nothing the \
             browser would rewrite",
        ));
    }
    net::check_url(&url, NetPolicy::notification(false))
        .map_err(|_| ConfigError("a cache address isn't a public one"))?;
    let host = host_key(url.as_str()).ok_or(ConfigError("a cache address has no host"))?;
    Ok(Cache {
        head: head.to_owned(),
        tail: tail.to_owned(),
        upper: is_upper,
        host,
        template: raw.to_owned(),
        same: (url.as_str().to_owned(), is_upper),
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
/// Its `Debug` leaves the key out, so a log line can't leak it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Lookup {
    key: InfoHash,
    why: Why,
}

impl std::fmt::Debug for Lookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lookup")
            .field("why", &self.why)
            .finish_non_exhaustive()
    }
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
    /// How many more lookups `why`'s lane takes now, so a feeder can page
    /// no more than fits rather than have the rest dropped.
    pub fn room(&self, why: Why) -> usize {
        match why {
            Why::Interest => self.interest.capacity(),
            Why::Unverified => self.unverified.capacity(),
        }
    }

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
    /// Caches passed over because they were backing off or blocked.
    pub skipped: AtomicU64,
    /// Lookups dropped because their lane was full.
    pub dropped: AtomicU64,
}

/// A cached `.torrent` whose info dict hashes to its lookup's key. Made
/// only by [`verify_body`], after that check. Its `Debug` shows
/// neither the key nor the file.
#[derive(Clone)]
pub struct Verified {
    lookup: Lookup,
    /// Its `info` is parsed from `info_raw` with no encoding hint.
    torrent: TorrentFile,
    /// The info dict's bytes as the file holds them: what the hash covers.
    info_raw: Vec<u8>,
    v2: bool,
    host: String,
}

impl std::fmt::Debug for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verified")
            .field("why", &self.lookup.why)
            .field("v2", &self.v2)
            .finish_non_exhaustive()
    }
}

impl Verified {
    pub fn key(&self) -> InfoHash {
        self.lookup.key
    }

    pub fn why(&self) -> Why {
        self.lookup.why
    }

    /// The parsed file. Its `info` is the verified metadata, parsed from
    /// [`info_raw`](Self::info_raw) with no encoding hint.
    pub fn torrent(&self) -> &TorrentFile {
        &self.torrent
    }

    pub fn into_torrent(self) -> TorrentFile {
        self.torrent
    }

    /// The info dict's raw bytes, exactly as hashed: what the pipeline's
    /// judge parses and stores.
    pub fn info_raw(&self) -> &[u8] {
        &self.info_raw
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
    /// comment. Neither is kept for a torrent marked `private`: a private
    /// tracker's address holds the uploader's passkey, and its comment
    /// often names the tracker. Otherwise only trackers with no user name,
    /// query or passkey-shaped part are kept, and none on a literal LAN,
    /// loopback or link-local address: the file is a third party's. A kept
    /// tracker is the address as checked (the `url` crate's form), never
    /// the file's own text, which a looser parser could read as another
    /// host (X-fuzz-cache-1105). Web
    /// seeds and `created by` have no claim field yet
    /// (`docs/decisions/2026-11-05-SRC-13.md`).
    pub fn claims(&self) -> Claims {
        let trackers = if self.torrent.info.private {
            Vec::new()
        } else {
            let mut out: Vec<String> = Vec::new();
            for url in self.torrent.announce.iter().flatten() {
                if out.len() >= claims::MAX_TRACKERS {
                    break;
                }
                if let Some(url) = public_tracker(url)
                    && !out.contains(&url)
                {
                    out.push(url);
                }
            }
            out
        };
        let mut c = Claims {
            trackers,
            description: self
                .torrent
                .comment
                .clone()
                .filter(|c| !c.is_empty() && !self.torrent.info.private),
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

/// `raw` as the address that was checked, when it's a public tracker: no
/// user name, query, fragment or passkey-shaped path segment, and not on
/// a literal LAN, loopback or link-local address. The normalised form is
/// what's kept, so what's stored is what was judged.
fn public_tracker(raw: &str) -> Option<String> {
    if raw.len() > MAX_TRACKER_URL {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?;
    // `check_url` takes http(s) only: a UDP tracker's host is checked as
    // the same host over http.
    let public = match url.scheme() {
        "http" | "https" => net::check_url(&url, NetPolicy::notification(false)).is_ok(),
        "udp" => url::Url::parse(&format!("http://{host}/"))
            .is_ok_and(|u| net::check_url(&u, NetPolicy::notification(false)).is_ok()),
        _ => false,
    };
    let kept = public
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url
            .path_segments()
            .into_iter()
            .flatten()
            .any(|s| s.len() >= PASSKEY_MIN && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        && url.as_str().len() <= MAX_TRACKER_URL;
    kept.then(|| url.into())
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
    /// [`MANIFEST`] with the config's hosts as its endpoints.
    manifest: Manifest,
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
        let manifest = Manifest {
            endpoints: endpoints_of(&config),
            ..MANIFEST
        };
        let plugin = TorrentCache {
            config,
            manifest,
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

    /// The counters, shared, for the status page.
    pub fn stats_handle(&self) -> Arc<Stats> {
        Arc::clone(&self.stats)
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
            // A cache backing off or blocked is passed over, not waited
            // out: one cache's six-hour back-off mustn't hold up every
            // lookup behind it. Its poll interval is still waited out.
            let now = tokio::time::Instant::now();
            let held = |s: HostState| !matches!(s, HostState::Ready);
            if held(http.limiter().state(&cache.host, now))
                || held(http.limiter().unit_state(&cache.host, now))
            {
                self.stats.skipped.fetch_add(1, Ordering::Relaxed);
                last = Miss::Failed;
                continue;
            }
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
        verify_body(body, lookup, &cache.host).map_err(fail)
    }
}

/// A cache's reply body, checked: parsed as a `.torrent` within
/// [`LIMITS`], its info dict re-parsed from its raw span with no encoding
/// hint ([`TorrentFile::raw_info`]; the file's top-level `encoding` key
/// isn't covered by the hash), and kept only when that dict hashes to
/// `lookup`'s key ([`verify`]). `host` is the cache it came from.
///
/// Sync and free of I/O, so it's fuzzed directly (`fuzz/fuzz_targets/
/// torrent_cache.rs`); [`TorrentCache::lookup`] calls it on each body it
/// fetches. The body is taken by value and dropped once the raw dict is
/// copied out, so a file near the 8 MiB cap isn't held while its dict is
/// parsed again.
///
/// # Errors
/// [`Miss::Invalid`] when either parse fails, [`Miss::Mismatch`] when
/// the dict hashes to another key. Nothing is counted here: the caller
/// counts the miss.
pub fn verify_body(body: Vec<u8>, lookup: &Lookup, host: &str) -> Result<Verified, Miss> {
    let mut torrent = TorrentFile::parse_with(&body, &LIMITS).map_err(|_| Miss::Invalid)?;
    // The info dict as BEP 9 would deliver it: its own bytes, no hint
    // from the file's unhashed `encoding` key.
    let info_raw = TorrentFile::raw_info(&body, &LIMITS)
        .map_err(|_| Miss::Invalid)?
        .to_vec();
    drop(body);
    torrent.info = Info::parse_with(
        &info_raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    )
    .map_err(|_| Miss::Invalid)?;
    let Some(v2) = verify(&torrent, &lookup.key) else {
        return Err(Miss::Mismatch);
    };
    Ok(Verified {
        lookup: *lookup,
        torrent,
        info_raw,
        v2,
        host: host.to_owned(),
    })
}

/// `config`'s templates as manifest endpoints: `https` and the template
/// after `https://`, sorted and once each, so the consent dialog shows,
/// and an acknowledgement covers, each full address, path and query
/// included (SRC-13c: a host with per-user paths, such as `/~user/`, is a
/// different party per path). The manifest's list is `&'static`, so it's
/// leaked: at most [`MAX_CACHES`] short strings, once per plugin built
/// (once per start).
fn endpoints_of(config: &Config) -> &'static [Endpoint] {
    let mut list: Vec<&str> = config
        .caches
        .iter()
        .map(|c| c.template.strip_prefix("https://").unwrap_or(&c.template))
        .collect();
    list.sort_unstable();
    list.dedup();
    if list.is_empty() {
        return &[];
    }
    let list: Vec<Endpoint> = list
        .into_iter()
        .map(|t| Endpoint {
            protocol: "https",
            host: Box::leak(t.to_owned().into_boxed_str()),
        })
        .collect();
    Box::leak(list.into_boxed_slice())
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
    /// [`MANIFEST`] with each listed cache's address template as an
    /// `https` endpoint: the consent version holds them exactly, so the
    /// acknowledgement covers exactly these addresses, and adding or
    /// changing one asks again.
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn config_editable(&self) -> bool {
        true
    }

    fn config_json(&self) -> Option<String> {
        Some(self.config.to_json())
    }

    /// [`Config::from_json`]'s rules; what's saved is [`Config::to_json`].
    fn check_config(&self, json: &str) -> Result<String, ConfigRefusal> {
        Config::from_json(json)
            .map(|c| c.to_json())
            .map_err(|e| ConfigRefusal(e.0))
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    /// The listed caches' hosts, public addresses only; `None` while the
    /// list is empty, so nothing is reachable.
    fn endpoints(&self) -> Option<Endpoints> {
        TorrentCache::endpoints(self)
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
    fn only_public_trackers_are_kept() {
        for ok in [
            "udp://tracker.example:1337/announce",
            "https://tracker.example/announce",
        ] {
            assert_eq!(public_tracker(ok).as_deref(), Some(ok));
        }
        for bad in [
            "http://192.168.1.1/announce",
            "udp://127.0.0.1:6969/announce",
            "udp://[::1]:6969/announce",
            "http://169.254.169.254/announce",
            "https://t.example/announce?passkey=x",
            "https://t.example/abcdef0123456789abcd/announce",
            "wss://t.example/announce",
            "https://user@t.example/announce",
        ] {
            assert_eq!(public_tracker(bad), None, "{bad}");
        }
    }

    /// The `verified_claims` fuzz target's first find (X-fuzz-cache-1105):
    /// `http:1.1/…` is read as `http://1.0.0.1/…`, a public host, while a
    /// looser parser could read the loopback address later in the text.
    /// What's kept is the address as checked, never the file's text.
    #[test]
    fn a_tracker_is_kept_as_checked_not_as_written() {
        let raw = "http:1.1/announce^udp://127.0.0.1:6969/announce";
        let kept = public_tracker(raw).unwrap();
        assert!(kept.starts_with("http://1.0.0.1/"), "{kept}");
        assert_eq!(public_tracker(&kept).as_deref(), Some(kept.as_str()));
        assert_eq!(
            public_tracker("HTTPS://Tracker.Example:443/announce").as_deref(),
            Some("https://tracker.example/announce")
        );
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
