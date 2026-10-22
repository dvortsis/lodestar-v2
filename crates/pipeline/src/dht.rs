//! The Mainline DHT as source plugin #1 (architecture §5, §18.1): it
//! discovers keys (BEP 51 sampling, passive capture) and finds peers for
//! keys the fetch pool asks about (`get_peers` lookups).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{Config, Harvest, Node, NodeHandle, Source, StatsSnapshot};
use lodestar_metainfo::InfoHash;
use lodestar_sources::{
    BoxFuture, Consent, Endpoint, Found, Kind, Manifest, Network, PluginError, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use tokio::sync::mpsc;

/// The DHT's harvest strategy codes, for per-strategy metrics. Persisted
/// in the rollups: append only.
pub mod strategy {
    pub const SAMPLE: u8 = 0;
    pub const GET_PEERS: u8 = 1;
    pub const ANNOUNCE: u8 = 2;
    pub const LOOKUP: u8 = 3;
    /// Not a DHT strategy: an incomplete import queued for its info dict.
    pub const REFETCH: u8 = 200;
}

static MANIFEST: Manifest = Manifest {
    id: "dht",
    name: "Mainline DHT",
    version: env!("CARGO_PKG_VERSION"),
    kinds: &[Kind::Discovery, Kind::PeerFinder],
    networks: &[Network::Bt],
    endpoints: &[
        Endpoint {
            protocol: "udp",
            host: "Mainline DHT nodes (any)",
        },
        Endpoint {
            protocol: "tcp",
            host: "BitTorrent peers (metadata only)",
        },
    ],
    risk: Risk::Low,
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: true,
};

pub struct DhtSource {
    config: Config,
    stats: Arc<Mutex<Option<StatsSnapshot>>>,
    node: Arc<Mutex<Option<NodeHandle>>>,
}

impl DhtSource {
    pub fn new(config: Config) -> DhtSource {
        DhtSource {
            config,
            stats: Arc::default(),
            node: Arc::default(),
        }
    }

    /// The running node, for BEP 33 scrapes: set once it has started and
    /// cleared when it stops. Clone the handle out rather than holding the
    /// lock across a scrape.
    pub fn node_handle(&self) -> Arc<Mutex<Option<NodeHandle>>> {
        self.node.clone()
    }

    /// The node's counters, refreshed every second while it runs.
    pub fn stats(&self) -> Option<StatsSnapshot> {
        *self.stats.lock().expect("dht stats lock")
    }

    /// Shared handle to the counters, for the metrics endpoint.
    pub fn stats_handle(&self) -> Arc<Mutex<Option<StatsSnapshot>>> {
        self.stats.clone()
    }
}

impl std::fmt::Debug for DhtSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtSource")
            .field("bind", &self.config.bind)
            .finish_non_exhaustive()
    }
}

fn found(h: Harvest) -> Found {
    let strategy = match h.source {
        Source::Sample => strategy::SAMPLE,
        Source::GetPeers => strategy::GET_PEERS,
        Source::Announce => strategy::ANNOUNCE,
        Source::Lookup => strategy::LOOKUP,
    };
    Found::key(h.info_hash, strategy).with_peers(h.peers)
}

async fn next_request(rx: &mut Option<mpsc::Receiver<InfoHash>>) -> Option<InfoHash> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl SourcePlugin for DhtSource {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let (tx, mut harvests) = mpsc::channel(4096);
            let node = Node::start(self.config.clone(), Some(tx))
                .await
                .map_err(|e| PluginError(format!("DHT bind failed: {e}")))?;
            tracing::info!(addrs = ?node.local_addrs(), "DHT node started");
            *self.node.lock().expect("dht handle lock") = Some(node.handle());
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut requests_open = true;
            loop {
                tokio::select! {
                    biased;
                    _ = ctx.shutdown.requested() => break,
                    Some(h) = harvests.recv() => {
                        // Lookups answer the pool's requests; the rest is
                        // discovery.
                        match (&ctx.peer_replies, h.source) {
                            (Some(replies), Source::Lookup) => replies.try_emit(found(h)),
                            _ => ctx.emitter.try_emit(found(h)),
                        };
                    }
                    req = next_request(&mut ctx.peer_requests), if requests_open => match req {
                        // A full lookup queue drops the ask; the pool asks
                        // again after its lookup wait.
                        Some(key) => {
                            node.get_peers(key);
                        }
                        None => requests_open = false,
                    },
                    _ = tick.tick() => {
                        *self.stats.lock().expect("dht stats lock") = Some(node.stats());
                    }
                }
            }
            *self.node.lock().expect("dht handle lock") = None;
            node.shutdown()
                .await
                .map_err(|e| PluginError(format!("saving DHT state failed: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harvests_map_to_strategies() {
        let h = |source| Harvest {
            info_hash: InfoHash([1; 20]),
            source,
            peers: vec!["10.0.0.1:1".parse().unwrap()],
        };
        assert_eq!(found(h(Source::Sample)).strategy, strategy::SAMPLE);
        assert_eq!(found(h(Source::GetPeers)).strategy, strategy::GET_PEERS);
        assert_eq!(found(h(Source::Announce)).strategy, strategy::ANNOUNCE);
        let f = found(h(Source::Lookup));
        assert_eq!((f.strategy, f.peers.len()), (strategy::LOOKUP, 1));
        assert!(
            !f.private_source,
            "the public DHT is never a private source"
        );
    }
}
