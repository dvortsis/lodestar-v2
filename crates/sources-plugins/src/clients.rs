//! Your download clients as a source (architecture §18.2 rank 17, §18.7).
//!
//! The download client already set up in Settings → Integrations
//! (qBittorrent, Transmission or Deluge), with its stored credentials:
//! the host hands the plugin that [`ClientConfig`], opened from the
//! integrations [`SecretStore`](lodestar_integrations::SecretStore), so
//! nothing is entered twice. Each poll, at most once a minute (§5 Q4):
//!
//! 1. lists the client's torrents (`DownloadClient::list`'s protocol,
//!    [`Lister`]), over the plugin's own egress (SRC-5): every request goes
//!    through `ctx.kit.http`, the first of each cycle (qBittorrent's
//!    login, the others' list call) through `Client::poll`;
//! 2. for each torrent not seen yet, asks the client for its `.torrent`
//!    where it can (qBittorrent 4.5 and later), at most
//!    [`EXPORTS_PER_POLL`] a poll. A new torrent over that budget is found
//!    at once with the list's claims, and its export waits for a later
//!    poll; a failed export is tried again for up to [`MAX_EXPORT_TRIES`]
//!    polls;
//! 3. emits one key per new torrent ([`to_found`]), and again when a
//!    retried export brings the `.torrent`'s claims.
//!
//! Each cycle starts at least a poll interval after the last, whichever
//! request fails, and a host the limiter has blocked is waited out.
//!
//! **Everything it finds is `private_source`** (§18.7): it is never sent to
//! DHT `get_peers`, PEX or a public tracker (the pool refuses such keys),
//! never served over Torznab or Stremio and never exported. A torrent in
//! your client may come from a private tracker, and a public lookup would
//! leak its hash (BEP 27).
//!
//! **Everything the client sends is untrusted.** The list is parsed within
//! byte, entry and string bounds and malformed entries are skipped and
//! counted ([`Clients::skipped`]). Exported bytes go through
//! `lodestar-metainfo`, and their info dict is used only when its hash is
//! the listed key; otherwise the listing's own name and size are the
//! claims. Trackers are never kept: a private tracker's announce URL holds
//! the user's passkey.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use lodestar_integrations::clients::{
    self, AuthFailure, ClientConfig, ClientError, Feature, Listed, Lister, Wire, WireMethod,
    WireRequest, WireResponse,
};
use lodestar_integrations::net::NetPolicy;
use lodestar_metainfo::{InfoHash, TorrentFile};
use lodestar_sources::claims::{self, ClaimedFile, Claims, Confidence, Provenance};
use lodestar_sources::http::{Method, host_key};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Emitter, Endpoints, Found, HostRate,
    HttpError, Kind, Manifest, Network, PluginError, Policy, Priority, Request, Risk, SourceCtx,
    SourcePlugin,
};

pub use lodestar_integrations::clients::{
    ClientKind, Listing, MAX_LIST_BYTES, MAX_LISTED, MAX_LISTED_NAME, TorrentState, parse_list,
};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "download_clients";
/// The strategy code of a key known from the client's list only.
pub const STRATEGY_LIST: u8 = 1;
/// The strategy code of a key whose claims come from the client's own
/// `.torrent`, checked against the key.
pub const STRATEGY_EXPORT: u8 = 2;
/// The poll unit: the client's torrent list.
pub const UNIT: &str = "torrents";
/// `.torrent` exports asked for per poll.
pub const EXPORTS_PER_POLL: usize = 20;
/// Polls a torrent's export is tried on while the client has no
/// metadata for it yet (a magnet still fetching), about an hour.
pub const MAX_EXPORT_TRIES: u32 = 60;
/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Your download clients",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    // None built in: the only host is the client's own address
    // (`Clients::endpoints`).
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::Account,
    default_priority: Priority::Normal,
    enabled_by_default: false,
    always_adult: false,
    // What your download clients hold (§18.7).
    finds_private: true,
};

/// No settings of its own: the client and its credentials are Settings →
/// Integrations' (§18.7's Account consent).
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// Q4 (runner): download clients at least a minute apart. The client is
/// the user's own, on the LAN; a request a second with a burst of 4 lets
/// a login, the list and a poll's exports through without bunching.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(60),
        per_host: HostRate {
            interval: Duration::from_secs(1),
            burst: 4,
        },
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(3600),
        },
    },
    config_schema: CONFIG_SCHEMA,
    // The client is on the coordinator's network; a remote worker can't
    // reach it (§19.5).
    distribution: Distribution::Coordinator,
    max_agents: 1,
    ..Policy::STRICT
};

/// The download-clients plugin.
pub struct Clients {
    config: Option<ClientConfig>,
    skipped: Arc<AtomicU64>,
}

impl std::fmt::Debug for Clients {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ClientConfig`'s Debug shows the scheme, host and port only.
        f.debug_struct("Clients")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Clients {
    /// The plugin for the client set up in Settings → Integrations.
    pub fn new(config: ClientConfig) -> Clients {
        Clients {
            config: Some(config),
            skipped: Arc::default(),
        }
    }

    /// Listed in Settings → Sources before a client is set up; fails at
    /// once if run.
    pub fn unconfigured() -> Clients {
        Clients {
            config: None,
            skipped: Arc::default(),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the client
    /// address's origin only (scheme, host and port), under the
    /// download-client policy (LAN and loopback allowed; link-local and
    /// metadata addresses never). `None` until a client is set up, so
    /// nothing may be reached.
    pub fn endpoints(&self) -> Option<Endpoints> {
        let url = url_of(self.config.as_ref()?);
        host_key(url)?;
        Some(Endpoints::user_entered(NetPolicy::download_client()).only_origins(&[url]))
    }

    /// List entries the last poll skipped: malformed, or past
    /// [`MAX_LISTED`].
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}

fn url_of(config: &ClientConfig) -> &str {
    match config {
        ClientConfig::QBittorrent { url, .. }
        | ClientConfig::Transmission { url, .. }
        | ClientConfig::Deluge { url, .. }
        | ClientConfig::RTorrent { url, .. }
        | ClientConfig::Aria2 { url, .. } => url,
    }
}

impl SourcePlugin for Clients {
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

/// A [`Wire`] over the plugin's paced client: the request marked
/// [`WireRequest::poll`] through `Client::poll`, everything else through
/// `Client::send`.
struct KitWire(Client);

impl Wire for KitWire {
    fn exchange<'a>(
        &'a self,
        req: &'a WireRequest,
    ) -> BoxFuture<'a, Result<WireResponse, ClientError>> {
        Box::pin(async move {
            let request = Request {
                method: match req.method {
                    WireMethod::Get => Method::Get,
                    WireMethod::Post => Method::Post,
                },
                url: req.url.clone(),
                headers: req
                    .headers
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), v.clone()))
                    .collect(),
                body: req.body.clone(),
            };
            let res = if req.poll {
                self.0.poll(UNIT, &request).await
            } else {
                self.0.send(&request).await
            };
            match res {
                Ok(res) if res.body.len() > req.limit => {
                    Err(ClientError::Protocol("the response was too large"))
                }
                Ok(res) => Ok(WireResponse {
                    status: res.status,
                    headers: res.headers,
                    body: res.body,
                }),
                // No text: a transport error can name the URL. A blocked
                // host, a down route and no egress are all "unreachable"
                // to the poll loop, which tries again a poll later.
                Err(HttpError::BadUrl) => Err(ClientError::InvalidRequest("the client's URL")),
                // The limiter has the host blocked (a 401 or 403, or a
                // Retry-After): wait it out here, so a caller that tries
                // again at once can't spin.
                Err(HttpError::Blocked { until }) => {
                    tokio::time::sleep_until(until).await;
                    Err(ClientError::Unreachable)
                }
                Err(_) => Err(ClientError::Unreachable),
            }
        })
    }
}

/// What a poll's failure means for the run.
enum Next {
    /// Poll again (the poll gate spaces it).
    Again,
    /// Stop with this error: retrying a minute later can't help.
    Stop(PluginError),
    /// The pipeline or the host shut down.
    Done,
}

impl Clients {
    async fn run_inner(&self, ctx: SourceCtx) -> Result<(), PluginError> {
        let SourceCtx {
            emitter,
            mut shutdown,
            kit,
            ..
        } = ctx;
        let config = self.config.as_ref().ok_or_else(|| {
            fail("needs setup: no download client is set up in Settings → Integrations")
        })?;
        let every = kit.http.limiter().limits().poll_interval;
        let lister = match clients::lister(config, Arc::new(KitWire(kit.http))) {
            Ok(l) => l,
            Err(ClientError::Unsupported(_)) => return Err(fail(UNSUPPORTED)),
            Err(_) => {
                return Err(fail(
                    "needs setup: the download client's address isn't usable",
                ));
            }
        };
        let mut state = PollState::default();
        let mut last: Option<Instant> = None;
        loop {
            // Every cycle starts at least a poll interval after the last,
            // whatever request it fails on: a login that fails before the
            // list's poll gate mustn't retry at once (Q4).
            if let Some(last) = last {
                tokio::select! {
                    _ = shutdown.requested() => return Ok(()),
                    _ = tokio::time::sleep_until(last + every) => {}
                }
            }
            last = Some(Instant::now());
            let next = tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                n = self.poll(&*lister, &emitter, &mut state) => n,
            };
            match next {
                Next::Again => {}
                Next::Stop(e) => return Err(e),
                Next::Done => return Ok(()),
            }
        }
    }

    async fn poll(&self, lister: &dyn Lister, emitter: &Emitter, state: &mut PollState) -> Next {
        let listing = match lister.list().await {
            Ok(l) => l,
            Err(e) => return on_error(&e),
        };
        self.skipped.store(
            (listing.malformed + listing.over_cap) as u64,
            Ordering::Relaxed,
        );
        // Forget what the client no longer has, so the sets stay within
        // MAX_LISTED and a re-added torrent is found again.
        let listed: HashSet<InfoHash> = listing.torrents.iter().filter_map(key_of).collect();
        state.seen.retain(|k| listed.contains(k));
        state.pending.retain(|k, _| listed.contains(k));
        let mut budget = if lister.can_export() {
            EXPORTS_PER_POLL
        } else {
            0
        };
        // New torrents first, then the ones whose export is still wanted;
        // each at most once a poll.
        let mut handled = HashSet::new();
        // Retries go fewest failures first, so a torrent not asked yet
        // isn't starved by ones whose metadata is slow to arrive.
        let mut by_tries: Vec<&Listed> = listing.torrents.iter().collect();
        by_tries.sort_by_key(|t| {
            key_of(t)
                .and_then(|k| state.pending.get(&k).copied())
                .unwrap_or(0)
        });
        for retry in [false, true] {
            let order = if retry {
                &by_tries
            } else {
                &listing.torrents.iter().collect::<Vec<_>>()
            };
            for &t in order {
                let Some(key) = key_of(t) else { continue };
                let new = !state.seen.contains(&key);
                let wanted = if retry {
                    !new && state.pending.contains_key(&key)
                } else {
                    new
                };
                if !wanted || handled.contains(&key) {
                    continue;
                }
                // `tried`: an export was asked for this poll.
                let (exported, tried) = if !lister.can_export() {
                    (None, false)
                } else if budget == 0 {
                    if !new {
                        // A retry over this poll's budget: a later poll.
                        continue;
                    }
                    // A new torrent over the budget is found now, with the
                    // list's claims; its export waits for a later poll.
                    (None, false)
                } else {
                    budget -= 1;
                    match lister.export(&t.hash).await {
                        Ok(bytes) => (Some(bytes), true),
                        Err(e @ ClientError::Auth(_)) => return on_error(&e),
                        Err(_) => (None, true),
                    }
                };
                handled.insert(key);
                let Some(found) = to_found(t, exported.as_deref()) else {
                    continue;
                };
                if found.strategy == STRATEGY_EXPORT || exported.is_some() {
                    // Used, or the bytes are another torrent's: done.
                    state.pending.remove(&key);
                } else if lister.can_export() {
                    // Not asked yet, no metadata yet, or unreachable: try
                    // again later, up to MAX_EXPORT_TRIES failed polls.
                    let tries = state.pending.entry(key).or_insert(0);
                    *tries += u32::from(tried);
                    if *tries >= MAX_EXPORT_TRIES {
                        state.pending.remove(&key);
                    }
                }
                // A retry is emitted again only when it brings the
                // `.torrent`'s claims.
                if (new || found.strategy == STRATEGY_EXPORT) && emitter.emit(found).await.is_err()
                {
                    return Next::Done;
                }
                state.seen.insert(key);
            }
        }
        // End the client session the poll opened (qBittorrent's): the next
        // poll logs in afresh, so none is left to expire on the client.
        lister.finish().await;
        Next::Again
    }
}

/// What the run remembers between polls.
#[derive(Debug, Default)]
struct PollState {
    /// Torrents already emitted.
    seen: HashSet<InfoHash>,
    /// Emitted with the list's claims while their export is still wanted
    /// (not asked yet, or failed), by the polls it failed on.
    pending: HashMap<InfoHash, u32>,
}

const UNSUPPORTED: &str = "listing torrents works with qBittorrent, Transmission and Deluge only";

fn on_error(e: &ClientError) -> Next {
    match e {
        // A session refused straight after logging in (Transmission's
        // second 409, a daemon restarting): try again next poll.
        ClientError::Auth(AuthFailure::SessionRefused) => Next::Again,
        // Fixed text, no URL or secret (`ClientError`'s Display).
        ClientError::Auth(_) => Next::Stop(PluginError(format!("the download client: {e}"))),
        ClientError::InvalidRequest(_) => Next::Stop(fail(
            "needs setup: the download client's address isn't one Lodestar may reach",
        )),
        ClientError::Unsupported(Feature::List) => Next::Stop(fail(UNSUPPORTED)),
        _ => Next::Again,
    }
}

fn key_of(t: &Listed) -> Option<InfoHash> {
    t.hash.parse().ok()
}

/// One listed torrent as a discovery: always `private_source`, with no
/// peers, no external reference and no trackers.
///
/// `exported` is the client's `.torrent`, if it gave one. It's parsed with
/// `lodestar-metainfo`'s limits, and its name, size and files become the
/// claims (strategy [`STRATEGY_EXPORT`]) only when its info dict's hash is
/// the listed key. Otherwise the list's name and size are the claims
/// (strategy [`STRATEGY_LIST`]). Sizes outside 1 B to 1 PiB are dropped;
/// padding files are left out. `None` for a hash that isn't one.
pub fn to_found(t: &Listed, exported: Option<&[u8]>) -> Option<Found> {
    let key = key_of(t)?;
    let size = |s: u64| Some(s).filter(|s| (1..=MAX_SIZE).contains(s));
    let verified = exported
        .and_then(|b| TorrentFile::parse(b).ok())
        .filter(|f| f.info.primary_hash() == key);
    let (claims, strategy) = match verified {
        Some(file) => {
            let info = &file.info;
            let files = info
                .files
                .iter()
                .filter(|f| !f.is_padding())
                .take(claims::MAX_FILES)
                .map(|f| ClaimedFile {
                    path: info.path_string(f),
                    size: Some(f.size),
                })
                .collect();
            let claims = Claims {
                name: Some(info.name.decode().into_owned()).filter(|n| !n.is_empty()),
                size: size(info.total_size),
                files,
                ..Claims::default()
            };
            (claims, STRATEGY_EXPORT)
        }
        None => {
            let claims = Claims {
                name: t.name.clone(),
                size: t.size.and_then(size),
                ..Claims::default()
            };
            (claims, STRATEGY_LIST)
        }
    };
    let mut found = Found::key(key, strategy)
        .with_claims(claims)
        .with_provenance(Provenance {
            external_ref: None,
            published_at: None,
            confidence: Confidence::Medium,
        });
    found.private_source = true;
    found.clamp();
    Some(found)
}
