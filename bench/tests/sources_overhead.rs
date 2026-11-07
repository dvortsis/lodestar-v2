//! The v0.6 sources overhead benchmark (SRC-G; architecture §18.9: "with
//! all v0.6 sources enabled, the §3 search and ingest budgets still
//! hold"; §3: coordinator ingest ≥ 1,000 new verified torrents/s).
//!
//! The real ingest path (pool → fetch → prepare → writer → SQLite, with
//! the live search index following it, as §3's "commit + index") stores
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
use lodestar_search::{LiveConfig, Ranking, Serving, ServingConfig};
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
    /// Until every torrent was committed and indexed.
    secs: f64,
    /// Until every claim had been judged too.
    all_secs: f64,
    /// When the start's re-judge sweep finished, if it did in the run.
    sweep_at: Option<f64>,
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

/// What "every source on" adds: unverified items already stored (each
/// insert pays the duplicate check against them), and each plugin kind's
/// claims at a rate, paced through the run.
struct Load {
    name: &'static str,
    seeded: usize,
    /// Claims per second by plugin kind (`plugins::Run::kind`).
    rates: &'static [(&'static str, f64)],
}

/// (a) The stress load: one claim per two torrents at the budget's 1,000/s,
/// shared evenly by the nine kinds.
const STRESS: Load = Load {
    name: "(a) stress",
    seeded: 200_000,
    rates: &[
        ("academic", 55.5),
        ("archive_org", 55.5),
        ("nyaa", 55.5),
        ("sukebei", 55.5),
        ("prowlarr", 55.5),
        ("clients/qbittorrent", 55.5),
        ("clients/transmission", 55.5),
        ("full_scrape", 55.5),
        ("arr", 55.5),
    ],
};

/// (b) Every source on at its defaults: each plugin's per-poll cap over its
/// minimum poll interval, every item new (`docs/decisions/2026-11-07-SRC-G.md`
/// gives the derivation). The full scrape is at its night-window rate (one
/// tracker's 4M-entry cap over the 6 h window), the busiest hours of a day.
const REALISTIC: Load = Load {
    name: "(b) realistic",
    seeded: 200_000,
    rates: &[
        ("academic", 150.0 / 3600.0),
        ("archive_org", 10_000.0 / 3600.0),
        ("nyaa", 300.0 / 300.0),
        ("sukebei", 300.0 / 300.0),
        ("prowlarr", 10.0 * 1000.0 / 900.0),
        ("clients/qbittorrent", 20.0 / 60.0),
        ("clients/transmission", 0.0),
        ("full_scrape", 4_000_000.0 / 21_600.0),
        ("arr", 1.0),
    ],
};

/// The breakdown's factors alone: the duplicate check only, and the stress
/// claims into an empty unverified table.
const DUPLICATE_CHECK_ONLY: Load = Load {
    name: "200k unverified items, no claims",
    seeded: 200_000,
    rates: &[],
};
const CLAIMS_ONLY: Load = Load {
    name: "stress claims, no unverified items",
    seeded: 0,
    rates: STRESS.rates,
};

/// Stores `torrents` synthetic torrents, with `load` on or with no source
/// but the DHT-like one.
async fn run(torrents: u64, load: Option<&Load>) -> Report {
    let sources = load.is_some();
    let seeded = load.map_or(0, |l| l.seeded);
    // On disk, with the live search index following it: §3's budget is
    // for commit + index.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let (hot, _) = lodestar_pipeline::open_hot(&mut store, dir.path()).unwrap();
    let hot = Arc::new(hot);
    let serving = Serving::open_or_rebuild(
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
    .unwrap();
    let runs = if sources {
        plugins::fixture_runs()
    } else {
        Vec::new()
    };
    if seeded > 0 {
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
    // Each kind's claims for a run at the budget's pace (torrents / 1,000
    // seconds), sent in tenth-of-a-second slices at its rate.
    let budget_secs = torrents as f64 / 1_000.0;
    let mut rounds = Vec::new();
    for r in &runs {
        let rate = load
            .and_then(|l| l.rates.iter().find(|(k, _)| *k == r.kind))
            .map_or(0.0, |(_, rate)| *rate);
        let (p, tx) = plugins::relay(&r.manifest);
        plugins.push(p);
        let count = (rate * budget_secs).round() as u64;
        let finds: Vec<Found> = (0..count)
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
        PipelineConfig {
            hot: Some(hot),
            gate_follows_index: true,
            ..PipelineConfig::default()
        },
        store,
        plugins,
        Arc::new(Serves(pending)),
    )
    .await
    .unwrap();
    // The pacer: a sleep that only paces a writer (standards §2).
    let slices = (budget_secs * 10.0).ceil().max(1.0) as usize;
    let pacer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        for slice in 0..slices {
            tick.tick().await;
            for (tx, finds) in &rounds {
                let per = finds.len().div_ceil(slices);
                let from = (slice * per).min(finds.len());
                let to = ((slice + 1) * per).min(finds.len());
                if from < to {
                    tx.send(finds[from..to].to_vec()).await.unwrap();
                }
            }
        }
        // Keep the relays' channels open until the pipeline stops.
        rounds
    });
    let deadline = Instant::now() + Duration::from_secs(600);
    // When the torrents were all committed and indexed (the §3 figure),
    // and when every claim had been judged too.
    let mut torrents_at = None;
    // When the start's re-judge sweep over the stored unverified items
    // finished (it competes with ingest for the writer until then).
    let mut sweep_at = None;
    loop {
        let w = pipeline.writer_stats();
        let i = pipeline.intake_stats();
        let stored = w.stored.load(Ordering::Relaxed);
        let indexed = serving.stats().indexed.load(Ordering::Relaxed);
        let judged =
            w.unverified.received.load(Ordering::Relaxed) + i.refused.load(Ordering::Relaxed);
        if sweep_at.is_none() && w.unverified.sweeps.load(Ordering::Relaxed) > 0 {
            sweep_at = Some(started.elapsed().as_secs_f64());
        }
        if torrents_at.is_none() && stored >= torrents * 99 / 100 && indexed >= stored {
            torrents_at = Some(started.elapsed().as_secs_f64());
        }
        if torrents_at.is_some() && judged >= claims_sent {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stalled: {stored} of {torrents} stored, {indexed} indexed, {judged} of {claims_sent} claims"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let all_secs = started.elapsed().as_secs_f64();
    let secs = torrents_at.unwrap();
    drop(pacer.await.unwrap());
    let torrents = pipeline.writer_stats().stored.load(Ordering::Relaxed);
    // Read from the store once the writer has stopped: every claim's batch
    // is committed by then.
    let store = pipeline.shutdown().await.unwrap();
    let items: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM unverified_items", [], |r| r.get(0))
        .unwrap();
    Report {
        secs,
        all_secs,
        sweep_at,
        torrents,
        sent: claims_sent,
        claims: (items as u64).saturating_sub(if sources { seeded as u64 } else { 0 }),
        kinds: runs.iter().map(|r| r.kind).collect(),
    }
}

/// Everything lands with every source on: the torrents, and the claims as
/// unverified items. No timing is asserted (debug builds, shared runners).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sources_on_ingest_smoke() {
    let small = Load {
        seeded: 2_000,
        ..STRESS
    };
    let off = run(400, None).await;
    let on = run(400, Some(&small)).await;
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

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn range(v: &[f64]) -> (f64, f64) {
    v.iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)))
}

/// The paired arms' figures: off and on per pair (torrents/s), and each
/// pair's drop (%).
struct Paired {
    off: Vec<f64>,
    on: Vec<f64>,
    drops: Vec<f64>,
}

/// `pairs` alternating off/on runs of `load`, back to back.
async fn paired(torrents: u64, load: &Load, pairs: usize) -> Paired {
    let mut p = Paired {
        off: Vec::new(),
        on: Vec::new(),
        drops: Vec::new(),
    };
    for i in 0..pairs {
        let off = run(torrents, None).await.per_sec();
        let on_run = run(torrents, Some(load)).await;
        let on = on_run.per_sec();
        let drop = 100.0 * (1.0 - on / off);
        println!(
            "{} pair {}: off {off:.0}/s, on {on:.0}/s ({} claims, torrents done at {:.2} s, claims judged at {:.2} s, start sweep done at {}), drop {drop:.1}%",
            load.name,
            i + 1,
            on_run.sent,
            on_run.secs,
            on_run.all_secs,
            on_run.sweep_at.map_or("-".into(), |t| format!("{t:.2} s"))
        );
        p.off.push(off);
        p.on.push(on);
        p.drops.push(drop);
    }
    let (ol, oh) = range(&p.off);
    let (nl, nh) = range(&p.on);
    let (dl, dh) = range(&p.drops);
    println!(
        "{}: off median {:.0}/s [{ol:.0}, {oh:.0}], on median {:.0}/s [{nl:.0}, {nh:.0}], paired drop median {:.1}% [{dl:.1}, {dh:.1}]",
        load.name,
        median(&p.off),
        median(&p.on),
        median(&p.drops)
    );
    p
}

/// The measurement (runner, 2026-11-07): five alternating off/on pairs for
/// (a) the stress load and (b) every source on at its defaults, 20,000
/// torrents each run, commit + index. §3 is judged on (b): it fails only
/// when the off arm holds the budget and the on arm doesn't, every pair
/// dropping. Below the budget with sources off, the host can't judge it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; run in release"]
async fn sources_overhead() {
    const TORRENTS: u64 = 20_000;
    let _stress = paired(TORRENTS, &STRESS, 5).await;
    let b = paired(TORRENTS, &REALISTIC, 5).await;
    let (off, on) = (median(&b.off), median(&b.on));
    if off < 1_000.0 {
        println!("(b) not decided: the host's own median, sources off, is {off:.0}/s");
        return;
    }
    assert!(
        on >= 1_000.0 || b.drops.iter().any(|d| *d <= 0.0),
        "§3 ingest budget missed with every source on: median {on:.0}/s, off {off:.0}/s"
    );
}

/// Where the overhead comes from: each factor alone, in pairs with sources
/// off, no assert. For the runner.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; run in release"]
async fn sources_overhead_breakdown() {
    const TORRENTS: u64 = 20_000;
    for load in [&DUPLICATE_CHECK_ONLY, &CLAIMS_ONLY] {
        paired(TORRENTS, load, 5).await;
    }
}

/// One pair of the duplicate-check load, for a quick look.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; run in release"]
async fn sources_overhead_probe() {
    paired(20_000, &DUPLICATE_CHECK_ONLY, 1).await;
}
