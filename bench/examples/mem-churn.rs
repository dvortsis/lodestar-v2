//! `mem-churn` (R-mem-triage): drives the fetch pool's eviction path the
//! way a busy DHT does. A source emits fresh keys with no peers; a
//! stand-in peer finder refuses most lookups (as the DHT does past its
//! 32 concurrent get_peers) and answers the rest with no peers. The pool
//! fills to its cap and then evicts a key for every new one. Nothing is
//! ever stored, so any anonymous growth after the cap is per-key churn.
//!
//! ```sh
//! cargo run --release -p lodestar-bench --example mem-churn -- [KEYS_PER_SEC] [SECS]
//! ```

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::pool::PeerWireFetcher;
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Loss, Manifest, Network, PluginError, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use lodestar_store::{Store, StoreConfig};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn manifest(id: &'static str, kinds: &'static [Kind]) -> Manifest {
    Manifest {
        id,
        name: id,
        version: "1",
        kinds,
        networks: &[Network::Bt],
        endpoints: &[],
        risk: Risk::Low,
        consent: Consent::None,
        default_priority: Priority::Normal,
        enabled_by_default: true,
        always_adult: false,
    }
}

struct Keys {
    manifest: Manifest,
    per_sec: u64,
}

impl SourcePlugin for Keys {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            let per_tick = (self.per_sec / 100).max(1);
            let mut n: u64 = 0;
            loop {
                tick.tick().await;
                for _ in 0..per_tick {
                    n += 1;
                    let mut key = [0u8; 20];
                    key[..8].copy_from_slice(&n.to_le_bytes());
                    key[8..16]
                        .copy_from_slice(&n.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
                    ctx.emitter
                        .emit(Found::key(InfoHash(key), 0))
                        .await
                        .map_err(|_| PluginError("closed".into()))?;
                }
            }
        })
    }
}

/// Accepts one lookup in eight and answers it with no peers; refuses the
/// rest.
struct Finder {
    manifest: Manifest,
}

impl SourcePlugin for Finder {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut requests = ctx.peer_requests.take().expect("a peer finder");
            let mut n: u64 = 0;
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(request) = requests.recv() => {
                        n += 1;
                        if n.is_multiple_of(8) {
                            let (done, rx) = tokio::sync::oneshot::channel();
                            let _ = request.admit.send(Some(rx));
                            let _ = done.send(Vec::new());
                        } else {
                            let _ = request.admit.send(None);
                        }
                    }
                }
            }
        })
    }
}

fn anon_mib() -> f64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Anonymous:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(0.0, |kb| kb as f64 / 1024.0)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut args = std::env::args().skip(1);
    let per_sec: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(2000);
    let secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(600);
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).expect("store");
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        store,
        vec![
            Arc::new(Keys {
                manifest: manifest("keys", &[Kind::Discovery]),
                per_sec,
            }),
            Arc::new(Finder {
                manifest: manifest("finder", &[Kind::PeerFinder]),
            }),
        ],
        Arc::new(PeerWireFetcher::default()),
    )
    .await
    .expect("pipeline");
    let started = Instant::now();
    println!("secs,anon_mib,pending,backlog,leases,evicted,unaccepted,lost_nopeers");
    while started.elapsed() < Duration::from_secs(secs) {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let p = pipeline.pool_stats();
        let m = pipeline.monitor();
        println!(
            "{},{:.1},{},{},{},{},{},{}",
            started.elapsed().as_secs(),
            anon_mib(),
            p.pending.load(Ordering::Relaxed),
            p.lookup_backlog.load(Ordering::Relaxed),
            m.lease_stats().outstanding.load(Ordering::Relaxed),
            p.lost_count(Loss::Evicted),
            p.lookup_unaccepted.load(Ordering::Relaxed),
            p.lost_count(Loss::NoPeers),
        );
    }
    pipeline.shutdown().await.expect("shutdown");
}
