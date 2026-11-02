//! `mem-ingest` (R-mem-triage): drives the real ingest path (intake → fetch
//! pool → prepare → writer → SQLite, with the live search index and the
//! hot columns following it, as `serve` wires them) with synthetic
//! torrents, and reports anonymous memory per stored torrent.
//!
//! The DHT and the peer wire are stood in for: a source emits synthetic
//! keys with a peer, and a fetcher hands back their info dictionaries. So
//! it measures what grows per *stored torrent* in the pipeline, store and
//! search; it doesn't measure DHT churn, peers or failed lookups.
//!
//! ```sh
//! cargo run --release -p lodestar-bench --example mem-ingest -- [TORRENTS] [PER_SEC] [nosearch]
//! # top allocation sites (writes dhat-heap.json):
//! cargo run --release -p lodestar-bench --features dhat-heap --example mem-ingest -- 20000 300
//! ```
//!
//! Linux only for the memory figures: it reads `Anonymous` from
//! `/proc/self/smaps_rollup`, which is what the soak's cgroup `anon`
//! counts for this process.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_search::{LiveConfig, Ranking, Serving, ServingConfig};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Found, Kind, Manifest, Network, PluginError, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use lodestar_store::{Store, StoreConfig};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(not(any(feature = "dhat-heap", feature = "system-alloc")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

type Pending = Arc<Mutex<HashMap<InfoHash, Vec<u8>>>>;

static MANIFEST: Manifest = Manifest {
    id: "synth",
    name: "Synthetic torrents",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: true,
};

/// Emits `total` synthetic keys at `per_sec`, each with one (fake) peer.
struct Synth {
    total: u64,
    per_sec: u64,
    pending: Pending,
}

impl SourcePlugin for Synth {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut generator = Generator::new(7);
            let peer: SocketAddr = "198.51.100.7:6881".parse().expect("addr");
            let step = Duration::from_secs(1) / self.per_sec.max(1) as u32;
            let mut tick = tokio::time::interval(step.max(Duration::from_micros(100)));
            for _ in 0..self.total {
                tick.tick().await;
                let t = generator.next_torrent();
                let Ok(info) = Info::parse(&t.info) else {
                    continue;
                };
                let key = info.primary_hash();
                self.pending.lock().expect("pending").insert(key, t.info);
                ctx.emitter
                    .emit(Found::key(key, 0).with_peers(vec![peer]))
                    .await
                    .map_err(|_| PluginError("closed".into()))?;
            }
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

/// Hands back the dictionary the source made (and forgets it).
struct Serves(Pending);

impl Fetcher for Serves {
    fn fetch(
        &self,
        _peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let info = self.0.lock().expect("pending").remove(&key);
        Box::pin(async move {
            let info = info.ok_or(FailureKind::Refused)?;
            Ok(Fetched {
                info,
                v2: false,
                encrypted: false,
                pex: Vec::new(),
                client: None,
            })
        })
    }
}

/// `Anonymous` from smaps_rollup, in bytes (0 off Linux).
fn anon() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Anonymous:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

/// The largest anonymous mappings (`Anonymous` kB), to tell the Rust
/// allocator's arena from libc's heap (SQLite's and zstd's allocations).
fn print_mappings() {
    let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps") else {
        return;
    };
    let mut maps: Vec<(u64, String)> = Vec::new();
    let mut head = String::new();
    for line in smaps.lines() {
        if let Some(kb) = line.strip_prefix("Anonymous:") {
            let kb: u64 = kb
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            maps.push((kb, head.clone()));
        } else if line
            .split_whitespace()
            .next()
            .is_some_and(|f| f.contains('-') && !f.ends_with(':'))
        {
            head = line.to_owned();
        }
    }
    maps.sort_by_key(|m| std::cmp::Reverse(m.0));
    for (kb, head) in maps.iter().take(12) {
        let fields: Vec<&str> = head.split_whitespace().collect();
        let range = fields.first().copied().unwrap_or("");
        let size = range
            .split_once('-')
            .and_then(|(a, b)| {
                Some((u64::from_str_radix(b, 16).ok()? - u64::from_str_radix(a, 16).ok()?) >> 20)
            })
            .unwrap_or(0);
        println!(
            "# {:>8} kB anon in a {size} MiB mapping {}",
            kb,
            fields.get(5).copied().unwrap_or("")
        );
    }
}

/// Least-squares slope of y on x.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (mx, my) = points
        .iter()
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x / n, b + y / n));
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(a, b), (x, y)| {
        (a + (x - mx) * (y - my), b + (x - mx) * (x - mx))
    });
    if sxx == 0.0 { 0.0 } else { sxy / sxx }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    #[cfg(feature = "dhat-heap")]
    let profiler = dhat::Profiler::new_heap();

    let mut args = std::env::args().skip(1);
    let total: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(25_000);
    let per_sec: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(200);
    let every = (total / 25).max(1);
    let search = args.next().as_deref() != Some("nosearch");

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).expect("store");
    let (hot, _) = lodestar_pipeline::open_hot(&mut store, dir.path()).expect("hot");
    let hot = Arc::new(hot);
    let serving = search.then(|| {
        Serving::open_or_rebuild(
            &dir.path().join("index"),
            &db,
            ServingConfig {
                live: LiveConfig::default(),
                ranking: Ranking {
                    hot: Some(hot.clone()),
                    ..Ranking::default()
                },
                ..ServingConfig::default()
            },
        )
        .expect("index")
    });

    let pending: Pending = Arc::default();
    let pipeline = Pipeline::start_with(
        PipelineConfig {
            hot: Some(hot),
            gate_follows_index: search,
            ..PipelineConfig::default()
        },
        store,
        vec![Arc::new(Synth {
            total,
            per_sec,
            pending: pending.clone(),
        })],
        Arc::new(Serves(pending)),
    )
    .await
    .expect("pipeline");
    let gate = pipeline.monitor().gate().clone();
    if let Some(serving) = &serving {
        serving
            .commit_hook()
            .add(move |c| gate.searchable(c.max_torrent_id, c.at as i64));
    }

    let started = Instant::now();
    let base = anon();
    println!("stored,indexed,commits,anon_mib,secs");
    println!("0,0,0,{:.1},0", base as f64 / 1048576.0);
    let mut points = Vec::new();
    let mut next = every;
    loop {
        let stored = pipeline.writer_stats().stored.load(Ordering::Relaxed);
        if stored >= next {
            let a = anon();
            let indexed = serving
                .as_ref()
                .map_or(0, |s| s.stats().indexed.load(Ordering::Relaxed));
            let commits = serving
                .as_ref()
                .map_or(0, |s| s.stats().commits.load(Ordering::Relaxed));
            println!(
                "{stored},{indexed},{commits},{:.1},{}",
                a as f64 / 1048576.0,
                started.elapsed().as_secs()
            );
            points.push((stored as f64, a as f64));
            next = stored + every;
        }
        if stored >= total * 99 / 100 {
            break;
        }
        if started.elapsed() > Duration::from_secs(total / per_sec.max(1) * 4 + 120) {
            eprintln!("stalled at {stored} stored");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let the index catch up and a title rebuild run.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let end = anon();
    let half = &points[points.len() / 2..];
    println!(
        "# total stored {}, anon {:.1} → {:.1} MiB; slope over the second half {:.0} bytes/torrent, over all {:.0}",
        pipeline.writer_stats().stored.load(Ordering::Relaxed),
        base as f64 / 1048576.0,
        end as f64 / 1048576.0,
        slope(half),
        slope(&points),
    );
    print_mappings();
    #[cfg(feature = "dhat-heap")]
    drop(profiler);
    pipeline.shutdown().await.expect("shutdown");
    if let Some(serving) = serving {
        serving.shutdown().expect("serving");
    }
}
