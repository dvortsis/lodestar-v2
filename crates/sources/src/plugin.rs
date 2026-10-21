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
use tokio::sync::{mpsc, watch};

use crate::manifest::Manifest;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A source of keys (and, later, claims, name hints and enrichment).
///
/// Plugins are compiled in. Each runs as one task that [`run`]s until its
/// context's shutdown fires; everything it finds goes out through the
/// context's [`Emitter`].
///
/// [`run`]: SourcePlugin::run
pub trait SourcePlugin: Send + Sync {
    fn manifest(&self) -> &Manifest;
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
    pub peer_requests: Option<mpsc::Receiver<InfoHash>>,
    pub shutdown: Shutdown,
}

/// One key as a plugin reports it.
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
    /// How strongly this sighting suggests a live swarm (see [`evidence`]).
    /// With the number of sightings, it orders peer lookups and eviction
    /// in the fetch pool (§18.4).
    pub evidence: u8,
    /// The answer to the pool's peer request for this key. With no peers
    /// it says the lookup came up empty, so the key doesn't hold a lookup
    /// slot until the pool's timer runs out. It isn't a new sighting.
    pub answers_lookup: bool,
}

/// Levels for [`Found::evidence`]. Each extra sighting while a key is
/// queued adds one more, up to [`pool::MAX_RANK`](crate::pool::MAX_RANK).
pub mod evidence {
    /// Only that the key exists, e.g. a BEP 51 sample.
    pub const NONE: u8 = 0;
    /// Someone is looking for it now, e.g. a DHT `get_peers` query.
    pub const WANTED: u8 = 2;
    /// A peer says it has it, e.g. a DHT announce.
    pub const ANNOUNCED: u8 = 3;
}

impl Found {
    pub fn key(key: InfoHash, strategy: u8) -> Found {
        Found {
            key,
            peers: Vec::new(),
            strategy,
            private_source: false,
            interest: false,
            evidence: evidence::NONE,
            answers_lookup: false,
        }
    }

    pub fn with_peers(mut self, peers: Vec<SocketAddr>) -> Found {
        self.peers = peers;
        self
    }

    pub fn with_evidence(mut self, evidence: u8) -> Found {
        self.evidence = evidence;
        self
    }

    /// Marks this as the answer to a peer request.
    pub fn answering_lookup(mut self) -> Found {
        self.answers_lookup = true;
        self
    }
}

/// A [`Found`] stamped with its source and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub source: SourceId,
    pub found: Found,
    /// Unix seconds.
    pub at: i64,
}

/// Sends discoveries to the pipeline.
#[derive(Debug, Clone)]
pub struct Emitter {
    source: SourceId,
    tx: mpsc::Sender<Discovery>,
    dropped: Arc<AtomicU64>,
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

    fn stamp(&self, found: Found) -> Discovery {
        Discovery {
            source: self.source,
            found,
            at: unix_now(),
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
