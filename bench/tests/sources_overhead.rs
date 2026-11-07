//! The v0.6 sources overhead benchmark (SRC-G; architecture §18.9: "with
//! all v0.6 sources enabled, the §3 search and ingest budgets still
//! hold"; §3: coordinator ingest ≥ 1,000 new verified torrents/s).
//!
//! The real ingest path (pool → fetch → prepare → writer → SQLite) stores
//! synthetic torrents from a DHT-like source, as `examples/mem-ingest.rs`
//! does, once alone and once with every v0.6 plugin kind on: each plugin's
//! recorded fixture finds, replayed with no network and re-keyed so every
//! one is new, flow through the same writer at one claim per two torrents,
//! into a store that already holds unverified items, so every insert pays
//! the duplicate check against a full table (SRC-2).
//!
//! `sources_on_ingest_smoke` runs in the workspace test run and checks
//! only that everything lands. The measurement is ignored there: run it in
//! release, where the budget applies:
//!
//! ```text
//! cargo test --release -p lodestar-bench --test sources_overhead -- --ignored --nocapture
//! ```
//!
//! Search isn't measured here: no source writes to the search index in
//! v0.6 (unverified items are indexed by SRC-3b, after v0.6), so the only
//! search-side change is SRC-3's default `verified:yes` clause, which the
//! search bench's queries already carry through `api::search`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Found, Kind, Manifest, Network, PluginError, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use lodestar_store::unverified::NewUnverified;
use lodestar_store::{Store, StoreConfig};

#[path = "../../crates/sources-plugins/tests/support/plugins.rs"]
mod plugins;

/// The plugins crate's folder, for the shared fixture runs.
const PLUGINS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/sources-plugins");

type Pending = Arc<Mutex<HashMap<InfoHash, Vec<u8>>>>;

static SYNTH: Manifest = Manifest {
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
    always_adult: false,
    finds_private: false,
};

/// Emits `total` synthetic keys, each with one (fake) peer, as fast as the
/// pipeline takes them.
struct Synth {
    total: u64,
    pending: Pending,
}

impl SourcePlugin for Synth {
    fn manifest(&self) -> &Manifest {
        &SYNTH
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut generator = Generator::new(7);
            let peer: SocketAddr = "198.51.100.7:6881".parse().expect("addr");
            for _ in 0..self.total {
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

/// `copy` of `f`, re-keyed so it's a new key: the copy number in the
/// first four bytes.
fn rekey(f: &Found, copy: u32) -> Found {
    let mut key = f.key;
    let k = u32::from_be_bytes(key.0[..4].try_into().unwrap()) ^ copy.wrapping_mul(0x9E37_79B9);
    key.0[..4].copy_from_slice(&k.to_be_bytes());
    Found { key, ..f.clone() }
}

struct Report {
    secs: f64,
    torrents: u64,
    /// Claims sent, and those stored as new unverified items (the rest
    /// merged into one already stored, or were refused).
    sent: u64,
    claims: u64,
    kinds: Vec<&'static str>,
}

impl Report {
    fn per_sec(&self) -> f64 {
        self.torrents as f64 / self.secs
    }
}

/// Stores `torrents` synthetic torrents; with `sources`, every plugin
/// kind's finds alongside (one claim per two torrents) into a store
/// already holding `seeded` unverified items.
async fn run(torrents: u64, sources: bool, seeded: usize) -> Report {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let runs = if sources {
        plugins::fixture_runs()
    } else {
        Vec::new()
    };
    if sources {
        let src = store.register_source("seeded", "1", 0).unwrap();
        let claims = lodestar_store::claims::Claims {
            name: Some("Seeded Item 2020".into()),
            ..Default::default()
        };
        let keys: Vec<[u8; 20]> = (0..seeded as u64)
            .map(|i| {
                let mut k = [0xA5; 20];
                k[..8].copy_from_slice(&i.to_be_bytes());
                k
            })
            .collect();
        for chunk in keys.chunks(10_000) {
            let items: Vec<NewUnverified<'_>> = chunk
                .iter()
                .map(|k| NewUnverified {
                    network: lodestar_store::unverified::Network::Bt,
                    key: k,
                    source: src,
                    private_source: false,
                    claims: Some(&claims),
                    at: 0,
                })
                .collect();
            store.put_unverified(&items).unwrap();
        }
    }
    let pending: Pending = Arc::default();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = vec![Arc::new(Synth {
        total: torrents,
        pending: pending.clone(),
    })];
    // Each kind's share of one claim per two torrents.
    let mut rounds = Vec::new();
    let per_kind = (torrents / 2) / runs.len().max(1) as u64;
    for r in &runs {
        let (p, tx) = plugins::relay(&r.manifest);
        plugins.push(p);
        let finds: Vec<Found> = (0..per_kind)
            .map(|i| {
                let f = &r.finds[i as usize % r.finds.len()];
                rekey(f, 1 + i as u32)
            })
            .collect();
        rounds.push((tx, finds));
    }
    let claims_sent: u64 = rounds.iter().map(|(_, f)| f.len() as u64).sum();
    let started = Instant::now();
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        store,
        plugins,
        Arc::new(Serves(pending)),
    )
    .await
    .unwrap();
    for (tx, finds) in rounds {
        for chunk in finds.chunks(256) {
            tx.send(chunk.to_vec()).await.unwrap();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let w = pipeline.writer_stats();
        let i = pipeline.intake_stats();
        let stored = w.stored.load(Ordering::Relaxed);
        let judged =
            w.unverified.received.load(Ordering::Relaxed) + i.refused.load(Ordering::Relaxed);
        if stored >= torrents * 99 / 100 && judged >= claims_sent {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stalled: {stored} of {torrents} stored, {judged} of {claims_sent} claims"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let secs = started.elapsed().as_secs_f64();
    let w = pipeline.writer_stats();
    let report = Report {
        secs,
        torrents: w.stored.load(Ordering::Relaxed),
        sent: claims_sent,
        claims: w.unverified.stored.load(Ordering::Relaxed),
        kinds: runs.iter().map(|r| r.kind).collect(),
    };
    pipeline.shutdown().await.unwrap();
    report
}

/// Everything lands with every source on: the torrents, and the claims as
/// unverified items. No timing is asserted (debug builds, shared runners).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sources_on_ingest_smoke() {
    let off = run(400, false, 0).await;
    let on = run(400, true, 2_000).await;
    assert!(off.torrents >= 396 && on.torrents >= 396);
    assert_eq!(off.claims, 0);
    assert_eq!(on.kinds.len(), 9, "{:?}", on.kinds);
    assert!(
        on.sent >= 180 && on.claims > 0,
        "{} of {}",
        on.claims,
        on.sent
    );
    println!(
        "smoke: off {:.0}/s, on {:.0}/s ({} of {} claims stored new)",
        off.per_sec(),
        on.per_sec(),
        on.claims,
        on.sent
    );
}

/// The measurement: best of three each way, 20,000 torrents, 200,000
/// unverified items already stored. Fails only on the §3 budget (≥ 1,000
/// new verified torrents/s with every source on); the overhead is
/// reported for the runner.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; run in release"]
async fn sources_overhead() {
    const TORRENTS: u64 = 20_000;
    let best = |a: Option<Report>, b: Report| match a {
        Some(a) if a.per_sec() >= b.per_sec() => Some(a),
        _ => Some(b),
    };
    let (mut off, mut on) = (None, None);
    for _ in 0..3 {
        off = best(off, run(TORRENTS, false, 0).await);
        on = best(on, run(TORRENTS, true, 200_000).await);
    }
    let (off, on) = (off.unwrap(), on.unwrap());
    let overhead = 100.0 * (1.0 - on.per_sec() / off.per_sec());
    println!(
        "sources off: {} torrents in {:.2} s = {:.0}/s",
        off.torrents,
        off.secs,
        off.per_sec()
    );
    println!(
        "sources on:  {} torrents in {:.2} s = {:.0}/s, {} claims sent ({} stored new) from {:?}; overhead {overhead:.1}%",
        on.torrents,
        on.secs,
        on.per_sec(),
        on.sent,
        on.claims,
        on.kinds
    );
    assert!(
        on.per_sec() >= 1_000.0,
        "§3 ingest budget missed with every source on: {:.0}/s",
        on.per_sec()
    );
}
