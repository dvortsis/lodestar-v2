//! The runtime contract between a source plugin and the rest of Lodestar
//! (architecture §18.1).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lodestar_metainfo::InfoHash;
use lodestar_store::SourceId;
use tokio::sync::{mpsc, oneshot, watch};

use crate::claims::{Claims, Provenance};
use crate::http::{Client, Fetch, HostLimiter, NoEgress};
use crate::manifest::{Manifest, Policy, UserLimits};
use crate::state::{MemoryState, PluginState};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How a streamed lookup ended (CR-1a-S). It carries no peers: they came
/// as batches. A finder never sends [`Terminal::SourceFailed`]; a dropped
/// terminal sender reads as it, never as success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Complete = 0,
    Deadline = 1,
    Cancelled = 2,
    SourceFailed = 3,
}

impl Terminal {
    pub const ALL: [Terminal; 4] = [
        Terminal::Complete,
        Terminal::Deadline,
        Terminal::Cancelled,
        Terminal::SourceFailed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Terminal::Complete => "complete",
            Terminal::Deadline => "deadline",
            Terminal::Cancelled => "cancelled",
            Terminal::SourceFailed => "source_failed",
        }
    }
}

/// An accepted streaming lookup (CR-1a-S): bounded peer batches as they
/// arrive, and a separate terminal. Dropping it cancels the work.
#[derive(Debug)]
pub struct LookupSession {
    pub batches: mpsc::Receiver<Vec<SocketAddr>>,
    pub terminal: oneshot::Receiver<Terminal>,
}

/// A streaming admission: `Some` acknowledges ownership with the session,
/// allocated only once the finder admits the request; `None` rejects.
pub type SessionAdmission = oneshot::Sender<Option<LookupSession>>;

/// A bounded peer lookup. `Some` acknowledges ownership; `None` rejects
/// without consuming an attempt. Dropping either receiver cancels work.
#[derive(Debug)]
pub struct PeerRequest {
    pub key: InfoHash,
    pub admit: oneshot::Sender<Option<oneshot::Receiver<Vec<SocketAddr>>>>,
    /// Streaming peers (CR-1a-S, `dht.streaming_peers`): the pool asks for
    /// a [`LookupSession`]. A finder that streams answers here and drops
    /// `admit`; one that doesn't answers `admit` as before and drops this.
    /// `None` when the switch is off. Declared before `permit` for the same
    /// reason as `admit`.
    pub session: Option<SessionAdmission>,
    /// Coordinated admission (CR-ADM): this request's slot at the finder.
    /// A finder holds it for as long as the request holds a slot and
    /// drops it to give the slot back. `None` when the switch is off.
    /// Keep it after `admit`: fields drop in order, so a request dropped
    /// whole closes its admission before its permit returns, and the pool
    /// reads the closed admission before it can reuse the permit.
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// A source of keys (and, later, claims, name hints and enrichment).
///
/// Plugins are compiled in. Each runs as one task that [`run`]s until its
/// context's shutdown fires; everything it finds goes out through the
/// context's [`Emitter`].
///
/// [`run`]: SourcePlugin::run
pub trait SourcePlugin: Send + Sync {
    fn manifest(&self) -> &Manifest;

    /// Politeness caps, config schema, placement and schedule (manifest
    /// v2). Plugins that don't say get [`Policy::STRICT`].
    fn policy(&self) -> &Policy {
        &Policy::STRICT
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>>;
}

#[derive(Debug)]
pub struct PluginError(pub String);

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginError {}

/// What a running plugin gets.
#[derive(Debug)]
pub struct SourceCtx {
    pub source: SourceId,
    pub emitter: Emitter,
    /// Keys the fetch pool wants peers for. Set only for plugins of kind
    /// [`PeerFinder`](crate::Kind::PeerFinder); never carries a
    /// `private_source` key.
    pub peer_requests: Option<mpsc::Receiver<PeerRequest>>,
    /// Optional unsolicited peer updates, bypassing the have-filter. Owned
    /// requests return their terminal batch through `PeerRequest::admit`,
    /// never through this lossy emitter.
    pub peer_replies: Option<Emitter>,
    pub shutdown: Shutdown,
    /// State and HTTP pacing.
    pub kit: Kit,
}

/// A plugin's own state store and its HTTP client, paced per host
/// (§18.1).
#[derive(Debug, Clone)]
pub struct Kit {
    pub state: Arc<dyn PluginState>,
    /// The only way the plugin sends HTTP. Its limiter also holds the
    /// poll and full-scrape gates.
    pub http: Client,
}

impl Kit {
    /// The client's limiter takes `policy`'s caps met with the user's
    /// settings.
    pub fn new(
        state: Arc<dyn PluginState>,
        fetch: Arc<dyn Fetch>,
        policy: &Policy,
        user: &UserLimits,
    ) -> Kit {
        Kit::with_limiter(state, fetch, HostLimiter::new(policy.limits(user)))
    }

    /// A kit for `plugin`, on its own policy.
    pub fn for_plugin(
        plugin: &dyn SourcePlugin,
        state: Arc<dyn PluginState>,
        fetch: Arc<dyn Fetch>,
        user: &UserLimits,
    ) -> Kit {
        Kit::new(state, fetch, plugin.policy(), user)
    }

    pub fn with_limiter(
        state: Arc<dyn PluginState>,
        fetch: Arc<dyn Fetch>,
        limiter: HostLimiter,
    ) -> Kit {
        Kit {
            state,
            http: Client::new(fetch, limiter),
        }
    }

    pub fn limiter(&self) -> &HostLimiter {
        self.http.limiter()
    }
}

impl Default for Kit {
    /// In-memory state, no egress ([`NoEgress`]: every request fails and
    /// nothing leaves the machine) and [`Policy::STRICT`]'s caps.
    fn default() -> Kit {
        Kit::new(
            Arc::new(MemoryState::new()),
            Arc::new(NoEgress),
            &Policy::STRICT,
            &UserLimits::default(),
        )
    }
}

/// One key as a plugin reports it: the discovery event (§18.1). The DHT
/// sends bare keys; feeds add claims and provenance, boxed so a bare key
/// stays small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub key: InfoHash,
    /// Peers known to have it, if any. Used for this fetch only, never
    /// stored (§18.7).
    pub peers: Vec<SocketAddr>,
    /// The plugin's own harvest strategy code, for per-strategy metrics.
    pub strategy: u8,
    /// Found only through a private tracker, Usenet account, private
    /// announce channel or your own client: never sent to public peer
    /// finding (§18.7, BEP 27).
    pub private_source: bool,
    /// A user is waiting on it: it jumps the fetch queue.
    pub interest: bool,
    /// What the source asserted about the key. The [`Emitter`] clamps it
    /// to the [`claims`](crate::claims) limits.
    pub claims: Option<Box<Claims>>,
    pub provenance: Option<Box<Provenance>>,
}

impl Found {
    pub fn key(key: InfoHash, strategy: u8) -> Found {
        Found {
            key,
            peers: Vec::new(),
            strategy,
            private_source: false,
            interest: false,
            claims: None,
            provenance: None,
        }
    }

    pub fn with_peers(mut self, peers: Vec<SocketAddr>) -> Found {
        self.peers = peers;
        self
    }

    /// Attaches claims; empty ones are dropped.
    pub fn with_claims(mut self, claims: Claims) -> Found {
        self.claims = (!claims.is_empty()).then(|| Box::new(claims));
        self
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Found {
        self.provenance = Some(Box::new(provenance));
        self
    }

    /// Cuts claims and provenance to their limits.
    pub fn clamp(&mut self) {
        if let Some(c) = &mut self.claims {
            c.clamp();
            if c.is_empty() {
                self.claims = None;
            }
        }
        if let Some(p) = &mut self.provenance {
            p.clamp();
        }
    }
}

/// A [`Found`] stamped with its source and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub source: SourceId,
    pub found: Found,
    /// Unix seconds.
    pub at: i64,
    /// Peers answering the pool's request (through
    /// [`SourceCtx::peer_replies`]), not a discovery.
    pub reply: bool,
}

/// Sends discoveries to the pipeline.
#[derive(Debug, Clone)]
pub struct Emitter {
    source: SourceId,
    tx: mpsc::Sender<Discovery>,
    dropped: Arc<AtomicU64>,
    replies: bool,
}

/// The pipeline has shut down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl Emitter {
    pub fn new(source: SourceId, tx: mpsc::Sender<Discovery>) -> Emitter {
        Emitter {
            source,
            tx,
            dropped: Arc::default(),
            replies: false,
        }
    }

    /// An emitter for peer replies (see [`SourceCtx::peer_replies`]).
    pub fn replies(source: SourceId, tx: mpsc::Sender<Discovery>) -> Emitter {
        Emitter {
            replies: true,
            ..Emitter::new(source, tx)
        }
    }

    pub fn source(&self) -> SourceId {
        self.source
    }

    /// Waits for room. Importers use this, so a flood backs up into the
    /// importer rather than into memory.
    pub async fn emit(&self, found: Found) -> Result<(), Closed> {
        self.tx.send(self.stamp(found)).await.map_err(|_| Closed)
    }

    /// Never waits: live sources use this, and a key that finds the
    /// pipeline full is dropped and counted.
    pub fn try_emit(&self, found: Found) -> bool {
        match self.tx.try_send(self.stamp(found)) {
            Ok(()) => true,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Keys dropped by [`try_emit`](Emitter::try_emit) so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn stamp(&self, mut found: Found) -> Discovery {
        found.clamp();
        Discovery {
            source: self.source,
            found,
            at: unix_now(),
            reply: self.replies,
        }
    }
}

/// Resolves once shutdown has been requested.
#[derive(Debug, Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// A trigger and the receiver plugins hold.
    pub fn new() -> (watch::Sender<bool>, Shutdown) {
        let (tx, rx) = watch::channel(false);
        (tx, Shutdown(rx))
    }

    pub fn is_requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Returns when shutdown is requested or the trigger is dropped.
    pub async fn requested(&mut self) {
        let _ = self.0.wait_for(|v| *v).await;
    }
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
