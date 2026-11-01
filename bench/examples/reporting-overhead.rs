//! Reporting overhead (architecture §3, §14 M5 exit): what Insights costs
//! the crawler and search, and what its rollups cost on disk.
//!
//! - **Search p95** through the real `/api/v1/search` route, on a synthetic
//!   corpus whose store also holds a year of rollups, with and without an
//!   Insights load beside it: the overview and the Fetch page (`range=7d`)
//!   rebuilt after every flush, as an open tab refetches on each
//!   `insights` event, plus a digest preview (the digest job's whole
//!   computation: a `24h` compared overview, rendered). Budget: +5%.
//! - **Ingest throughput**: storing, enriching and indexing new torrents
//!   in the writer's batches, with and without the same load. Budget: −2%.
//! - **Rollup storage per year**: the bench fixture's busiest hour of
//!   rollup and cohort rows, repeated for a year, compacted as the recorder
//!   does (30 days hourly, the rest daily) and vacuumed; and the same for
//!   a widened B11a hour (~160 rollup + 30 cohort rows), a busy
//!   crawler's bound. Budget: 50 MB.
//!
//! The Insights load runs one round per `--every`, by default the UI's
//! rate (one `insights` event per 30 s; `crates/api/src/events.rs`), and
//! its first round starts with each loaded phase. A faster rate measures
//! collisions with the overview's turn on the shared store reader, not
//! an open tab. Searches run on a fixed corpus first, then the ingest
//! phases; base and loaded phases alternate which goes first, so drift on
//! a shared runner lands on both. At that rate a round works about 0.1%
//! of the time, so the timed deltas are run-to-run noise and their gates
//! (budget plus a noise allowance) catch only gross regressions; the
//! operative gate is the round's duty cycle, [`DUTY_BUDGET`]. `--gate`
//! exits non-zero when any gate fails; the allowance is explained in
//! `docs/decisions/2026-11-01-G-m5a.md`.

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::insights::{InsightsSource, OverviewCache};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source as OpsSource, SourcePatch, StatsResponse,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bench::fixture;
use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_pipeline::metric;
use lodestar_search::{SearchIndex, index_dir};
use lodestar_store::rusqlite::{Connection, params};
use lodestar_store::{CompactOptions, NewTorrent, SourceId, Store, StoreConfig};
use rand::SeedableRng;
use rand::seq::IndexedRandom;
use rand_chacha::ChaCha8Rng;
use tower::ServiceExt;

/// §3: search p95 may rise this much with Insights open.
const SEARCH_BUDGET: f64 = 0.05;
/// §3: ingest throughput may fall this much.
const INGEST_BUDGET: f64 = 0.02;
/// §3: rollup storage per year.
const STORAGE_BUDGET: u64 = 50_000_000;
/// The share of the time one Insights round may take at the UI's rate.
/// This bounds what Insights can take from search or ingest, and unlike
/// the timed deltas it carries no run-to-run noise: the operative gate.
const DUTY_BUDGET: f64 = 0.01;
/// What a run-to-run spread on a shared runner can add to a measured
/// delta: the gate is the budget plus this (runner's call, §2.26).
const NOISE_ALLOWANCE: f64 = 0.03;
/// Distinct search queries, cycled through each phase.
const SEARCHES: usize = 400;
/// One Insights round: an open tab's refetch after an `insights` event
/// (the overview and a stage page), and the digest job's computation.
const ROUND: [&str; 3] = [
    "/api/v1/insights/overview?range=7d",
    "/api/v1/insights/fetch?range=7d",
    "/api/v1/digest/preview?period=daily",
];
/// The writer's batch.
const BATCH: usize = 500;

const USAGE: &str = "\
reporting-overhead [--torrents N] [--ingest N] [--runs N] [--phase S] [--every MS] [--gate]
  --torrents N  corpus size (default 100000)
  --ingest N    torrents stored per ingest phase (default 10000)
  --runs N      base/loaded pairs of each kind, even (default 6)
  --phase S     seconds each search phase runs (default 30)
  --every MS    one Insights round per MS (default 30000, the UI's rate)
  --gate        exit 1 when a budget is missed";

struct Args {
    torrents: usize,
    ingest: usize,
    runs: usize,
    phase: Duration,
    every: Duration,
    gate: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        torrents: 100_000,
        ingest: 10_000,
        runs: 6,
        phase: Duration::from_secs(30),
        every: Duration::from_millis(30_000),
        gate: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut num = || -> Result<usize, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or(format!("{flag} needs a number"))
        };
        match flag.as_str() {
            "--torrents" => a.torrents = num()?,
            "--ingest" => a.ingest = num()?,
            // Even, so loaded goes first in half the pairs: ingest slows
            // as each phase grows the corpus.
            "--runs" => a.runs = num()?.max(1).next_multiple_of(2),
            "--phase" => a.phase = Duration::from_secs(num()? as u64),
            "--every" => a.every = Duration::from_millis(num()? as u64),
            "--gate" => a.gate = true,
            _ => return Err(USAGE.into()),
        }
    }
    Ok(a)
}

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The binary's `InsightsSource`, minus the pipeline: `flush` stands in
/// for the writer's flush, which is what makes a cached overview stale.
#[derive(Debug, Default)]
struct Source {
    flush: AtomicI64,
    cache: OverviewCache,
}

impl InsightsSource for Source {
    fn last_flush(&self) -> Option<i64> {
        Some(self.flush.load(Ordering::Relaxed))
    }
    fn started_at(&self) -> i64 {
        unix_now() - 86_400
    }
    fn last_recorded(&self) -> i64 {
        0
    }
    fn imported(&self) -> Option<u64> {
        Some(0)
    }
    fn cache(&self) -> &OverviewCache {
        &self.cache
    }
}

/// The process's `OpsApi`, idle: the overview and stage pages read their
/// snapshots from it.
#[derive(Debug)]
struct Ops;

impl OpsApi for Ops {
    fn crawler(&self) -> CrawlerRuntime {
        CrawlerRuntime {
            since: 0,
            dht: None,
            fetch: FetchStatus {
                in_flight: 0,
                lookups: 0,
                queued: 0,
            },
        }
    }
    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        Box::pin(async {
            StatsResponse {
                data_bytes: None,
                disk_free_bytes: None,
                fetched_per_hour: None,
                harvested_per_hour: None,
                index_lag_s: Some(0),
                paused: false,
                queue_depth: 0,
                sampling_blocked: None,
                searchable_torrents: None,
                stored_torrents: 0,
                updated_at: unix_now(),
                uptime_s: 0,
            }
        })
    }
    fn sources(&self) -> BoxFuture<'_, Result<Vec<OpsSource>, OpsError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn patch_source(&self, _: u8, _: SourcePatch) -> BoxFuture<'_, Result<OpsSource, OpsError>> {
        Box::pin(async { Err(OpsError::UnknownSource) })
    }
    fn network(&self) -> NetworkCheck {
        NetworkCheck {
            checked_at: 0,
            external: ExternalAddress {
                agreeing: 0,
                ip: None,
                port: None,
                voters: 0,
            },
            inbound_queries: None,
            mapping: PortMapping::Unavailable,
            measured_s: 0,
            min_window_s: 120,
            reachable: None,
            reason: ReachReason::DhtOff,
            unsolicited_queries: None,
            unsolicited_sources: None,
            window_s: 600,
            window_started_at: None,
        }
    }
    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        Err(OpsError::Internal("not here".into()))
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs() as i64
}

/// Stores, enriches and indexes `n` torrents from `g` in the writer's
/// batches, committing the index after each one (`live`) or once.
fn ingest(
    store: &mut Store,
    index: &SearchIndex,
    g: &mut Generator,
    n: usize,
    live: bool,
) -> Result<(), Error> {
    let mut indexer = index.writer(64 << 20)?;
    let mut left = n;
    while left > 0 {
        let take = left.min(BATCH);
        left -= take;
        let synth: Vec<_> = (0..take).map(|_| g.next_torrent()).collect();
        let infos: Vec<Info> = synth
            .iter()
            .map(|t| Info::parse(&t.info))
            .collect::<Result<_, _>>()?;
        let now = unix_now();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .map(|info| NewTorrent {
                info,
                raw_info: None,
                discovered_at: now - 60,
                metadata_at: now,
                extra_flags: 0,
                source: SourceId::new(0).expect("valid"),
            })
            .collect();
        let first = match store.insert(&batch)?.first() {
            Some(lodestar_store::Inserted::New(id)) => *id,
            other => return Err(format!("not a new torrent: {other:?}").into()),
        };
        // Enrichment (§6), as the writer's background pass stores it.
        let mut after = first - 1;
        while let Some((last, _)) = lodestar_pipeline::enrich::enrich_stored(store, after, 5000)? {
            after = last;
        }
        while indexer.catch_up(store, 20_000)? > 0 {}
        if live {
            indexer.commit()?;
        }
    }
    indexer.commit()?;
    indexer.finish()?;
    Ok(())
}

/// The bench fixture's busiest hour: its rollup and cohort rows.
type Hour = (Vec<(u32, u32, i64, i64)>, Vec<(i64, u8, u8, i64)>);

fn fixture_hour() -> Result<Hour, Error> {
    let dir = tempfile::tempdir()?;
    let fx = dir.path().join("fx");
    fixture::build(&fx, fixture::anchor(unix_now()))?;
    let c = Connection::open(fx.join(fixture::DB))?;
    let busiest: i64 = c.query_row(
        "SELECT h FROM (SELECT hour h, count(*) n FROM rollup_hour GROUP BY hour
                        UNION ALL SELECT cohort, count(*) FROM cohort_hour GROUP BY cohort)
         GROUP BY h ORDER BY sum(n) DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let rollups = c
        .prepare("SELECT metric, dim, key, value FROM rollup_hour WHERE hour = ?1")?
        .query_map([busiest], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let cohorts = c
        .prepare("SELECT source, stage, reason, count FROM cohort_hour WHERE cohort = ?1")?
        .query_map([busiest], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok((rollups, cohorts))
}

/// B11a's wide hour (`crates/api/tests/insights.rs` `year_of_rollups`,
/// about 110 rollup rows and 28 cohort rows) widened with every other
/// metric code (6–8, 13–15) and all ten loss reasons: a busy crawler's
/// bound.
fn wide_hour() -> Hour {
    let mut r = Vec::new();
    for source in 0..2 {
        for strategy in 0..3 {
            r.push((1, source, strategy, 900));
            r.push((2, source, strategy, 300));
        }
        r.push((3, source, 0, 2));
        r.push((4, source, 0, 120));
    }
    for tracker in 0..8 {
        for outcome in 0..3 {
            r.push((5, tracker, outcome, 40));
        }
    }
    r.extend((0..17).map(|k| (9, 0, k, 3)));
    for t in 0..12 {
        r.push((10, t, 0, 10));
        r.push((10, t, 1, 2));
    }
    r.extend((80..110).map(|b| (11, 0, b, 4)));
    r.extend((0..2).map(|d| (12, d, 0, 3600)));
    for m in 6..=8 {
        r.extend((0..12).map(|k| (m, 0, k, 5)));
    }
    r.push((13, 0, 0, 1_000_000));
    r.push((14, 3, 0, 300));
    r.extend((1..=10).map(|reason| (14, 6, reason, 20)));
    r.extend((0..12).map(|t| (15, t, 0, 1)));
    let mut c = Vec::new();
    for source in 0..2 {
        for (stage, reason) in [(1, 0), (2, 0), (3, 0), (4, 0), (5, 0)] {
            c.push((source, stage, reason, 100));
        }
        c.extend((1..=10).map(|reason| (source, 6, reason, 10)));
    }
    (r, c)
}

/// A year of `hour`'s rows up to now, with an index watermark row an hour
/// (never pruned), then compacted as the recorder does.
fn year_of(db: &Path, hour: &Hour) -> Result<(), Error> {
    let now = unix_now();
    let now_h = now / 3600;
    {
        let mut c = Connection::open(db)?;
        let tx = c.transaction()?;
        let mut r = tx.prepare(
            "INSERT OR REPLACE INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut k = tx.prepare(
            "INSERT OR REPLACE INTO cohort_hour (cohort, source, stage, reason, count) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for h in now_h - 365 * 24..now_h {
            for &(m, d, key, v) in &hour.0 {
                r.execute(params![h, m, d, key, v])?;
            }
            for &(s, st, re, n) in &hour.1 {
                k.execute(params![h, s, st, re, n])?;
            }
        }
        let mut w = tx.prepare(
            "INSERT OR REPLACE INTO index_watermark_hour (hour, last_id) VALUES (?1, ?2)",
        )?;
        for h in now_h - 365 * 24..now_h {
            w.execute(params![h, h * 100])?;
        }
        drop((r, k, w));
        tx.commit()?;
    }
    let mut store = Store::open(db, StoreConfig::default())?;
    let r = store.compact_rollups(CompactOptions {
        now,
        keep_days: 30,
        hold_from_day: None,
        budget: Duration::from_secs(3600),
        kind: &metric::aggregation,
    })?;
    assert!(r.done, "{r:?}");
    Ok(())
}

/// Bytes the rollups take: a vacuumed store with a year of them, less an
/// empty one.
fn storage(hour: &Hour) -> Result<u64, Error> {
    let dir = tempfile::tempdir()?;
    let size = |name: &str, fill: bool| -> Result<u64, Error> {
        let db = dir.path().join(name);
        drop(Store::open(&db, StoreConfig::default())?);
        if fill {
            year_of(&db, hour)?;
        }
        let out = dir.path().join(format!("{name}.vacuumed"));
        Connection::open(&db)?.execute("VACUUM INTO ?1", [out.to_string_lossy()])?;
        Ok(std::fs::metadata(out)?.len())
    };
    Ok(size("year.db", true)?.saturating_sub(size("empty.db", false)?))
}

struct App {
    router: Router,
    cookie: String,
}

impl App {
    async fn get(&self, uri: &str) -> Result<(), Error> {
        let mut req = Request::get(uri)
            .header(header::COOKIE, &self.cookie)
            .body(Body::empty())?;
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
        let resp = self.router.clone().oneshot(req).await?;
        let status = resp.status();
        axum::body::to_bytes(resp.into_body(), 64 << 20).await?;
        if !status.is_success() {
            return Err(format!("{uri}: {status}").into());
        }
        Ok(())
    }
}

/// Insights open in a tab, and the digest: one round per `every` until
/// `stop`. Returns the rounds run.
async fn load(
    app: Arc<App>,
    source: Arc<Source>,
    every: Duration,
    stop: Arc<AtomicBool>,
) -> Result<u64, Error> {
    let mut rounds = 0;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        source.flush.fetch_add(1, Ordering::Relaxed);
        for uri in ROUND {
            app.get(uri).await?;
        }
        rounds += 1;
        tokio::time::sleep(every.saturating_sub(started.elapsed())).await;
    }
    Ok(rounds)
}

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 95).div_ceil(100) - 1]
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// `loaded` against `base`: the median of the paired per-run deltas, and
/// their spread.
fn summary(base: &[f64], loaded: &[f64]) -> (f64, f64, f64) {
    let deltas: Vec<f64> = base.iter().zip(loaded).map(|(b, l)| l / b - 1.0).collect();
    let lo = deltas.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = deltas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (median(deltas), lo, hi)
}

async fn run(a: &Args) -> Result<bool, Error> {
    let started = Instant::now();
    let hour = fixture_hour()?;
    let wide = wide_hour();
    let (bytes, wide_bytes) = (storage(&hour)?, storage(&wide)?);
    for (name, h, b) in [
        ("the fixture's busiest", &hour, bytes),
        ("a wide", &wide, wide_bytes),
    ] {
        eprintln!(
            "{name} hour: {} rollup + {} cohort rows; a year takes {:.2} MB",
            h.0.len(),
            h.1.len(),
            b as f64 / 1e6
        );
    }

    let dir = tempfile::tempdir()?;
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default())?;
    let index = SearchIndex::open(&index_dir(&dir.path().join("index")))?;
    let mut g = Generator::new(0x5eed);
    ingest(&mut store, &index, &mut g, a.torrents, false)?;
    // The overview reads the wide year: the heavier load.
    year_of(&db, &wide)?;
    eprintln!(
        "corpus of {} built in {:.0?}",
        a.torrents,
        started.elapsed()
    );

    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false)?);
    let source = Arc::new(Source::default());
    let state = ApiState {
        auth: auth.clone(),
        index: Some(index.clone().into()),
        reader: Arc::new(Mutex::new(Store::open_reader(&db, StoreConfig::default())?)),
        filters: None,
        settings: None,
        adult: Default::default(),
        ops: Some(Arc::new(Ops) as Arc<dyn OpsApi>),
        insights: Some(source.clone() as Arc<dyn InsightsSource>),
        user_data: None,
        hot: None,
        events: None,
    };
    let router = Router::new().nest("/api/v1", router(state));
    let token = auth.setup_token().ok_or("no setup token")?;
    let mut req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"token":"{token}","password":"correct horse battery"}}"#
        )))?;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let resp = router.clone().oneshot(req).await?;
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()?
        .split(';')
        .next()
        .ok_or("cookie")?
        .to_string();
    let app = Arc::new(App { router, cookie });

    // Keyword queries from the corpus's own names.
    let words: Vec<String> = {
        let c = Connection::open(&db)?;
        let names: Vec<String> = c
            .prepare("SELECT name FROM torrents ORDER BY id LIMIT 5000")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut w: Vec<String> = names
            .iter()
            .flat_map(|n| n.split(|c: char| !c.is_alphanumeric()))
            .filter(|w| w.len() >= 4 && w.chars().all(|c| c.is_ascii_alphabetic()))
            .map(str::to_lowercase)
            .collect();
        w.sort();
        w.dedup();
        w
    };
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    let queries: Vec<String> = (0..SEARCHES)
        .map(|_| words.choose(&mut rng).cloned().unwrap_or_default())
        .collect();
    // Warm the caches and the sealed days' sums.
    for q in &queries {
        app.get(&format!("/api/v1/search?q={q}")).await?;
    }
    app.get("/api/v1/insights/overview?range=7d").await?;

    // A round alone, for the report: what one refetch costs.
    let t = Instant::now();
    for _ in 0..3 {
        source.flush.fetch_add(1, Ordering::Relaxed);
        for uri in ROUND {
            app.get(uri).await?;
        }
    }
    let round = t.elapsed() / 3;

    let (mut s_base, mut s_load, mut i_base, mut i_load) = (vec![], vec![], vec![], vec![]);
    let mut rounds = 0;
    // Searches on a fixed corpus first, then ingest; base and loaded
    // alternate which goes first.
    for ingesting in [false, true] {
        for r in 0..a.runs {
            for loaded in [r % 2 == 1, r % 2 == 0] {
                let stop = Arc::new(AtomicBool::new(false));
                let bg = loaded.then(|| {
                    tokio::spawn(load(app.clone(), source.clone(), a.every, stop.clone()))
                });
                let label = if loaded { "loaded" } else { "base  " };
                if ingesting {
                    let t = Instant::now();
                    ingest(&mut store, &index, &mut g, a.ingest, true)?;
                    let rate = a.ingest as f64 / t.elapsed().as_secs_f64();
                    eprintln!("run {r} {label}: ingest {rate:.0}/s");
                    if loaded { &mut i_load } else { &mut i_base }.push(rate);
                } else {
                    let mut times = Vec::new();
                    let t = Instant::now();
                    for q in queries.iter().cycle() {
                        if t.elapsed() >= a.phase {
                            break;
                        }
                        let one = Instant::now();
                        app.get(&format!("/api/v1/search?q={q}")).await?;
                        times.push(one.elapsed());
                    }
                    let n = times.len();
                    let ms = p95(times).as_secs_f64() * 1e3;
                    eprintln!("run {r} {label}: search p95 {ms:.3} ms over {n}");
                    if loaded { &mut s_load } else { &mut s_base }.push(ms);
                }
                stop.store(true, Ordering::Relaxed);
                if let Some(bg) = bg {
                    rounds += bg.await??;
                }
            }
        }
    }
    let (sd, slo, shi) = summary(&s_base, &s_load);
    let (id, ilo, ihi) = summary(&i_base, &i_load);
    let pct = |x: f64| format!("{:+.1}%", x * 100.0);
    println!(
        "| Measure | Base (median) | Loaded (median) | Δ (median pair) | Δ per pair | Budget | Gate |"
    );
    println!("|---|---|---|---|---|---|---|");
    println!(
        "| Search p95 | {:.2} ms | {:.2} ms | {} | {} … {} | +5% | ≤ {} |",
        median(s_base.clone()),
        median(s_load.clone()),
        pct(sd),
        pct(slo),
        pct(shi),
        pct(SEARCH_BUDGET + NOISE_ALLOWANCE)
    );
    println!(
        "| Ingest | {:.0}/s | {:.0}/s | {} | {} … {} | −2% | ≥ {} |",
        median(i_base.clone()),
        median(i_load.clone()),
        pct(id),
        pct(ilo),
        pct(ihi),
        pct(-(INGEST_BUDGET + NOISE_ALLOWANCE))
    );
    println!(
        "| Rollup storage, fixture rate | {:.2} MB/year | | | | 50 MB/year | ≤ 50 MB |",
        bytes as f64 / 1e6
    );
    println!(
        "| Rollup storage, wide hour | {:.2} MB/year | | | | 50 MB/year | ≤ 50 MB |",
        wide_bytes as f64 / 1e6
    );
    let duty = round.as_secs_f64() / a.every.as_secs_f64();
    println!(
        "| Insights duty cycle | {:.0?} per {:?} | {:.2}% | | | | ≤ {:.0}% |",
        round,
        a.every,
        duty * 100.0,
        DUTY_BUDGET * 100.0
    );
    println!(
        "\n{} runs, {rounds} Insights rounds (one per {:?}, {:.0?} each), {:.0?} in all",
        a.runs,
        a.every,
        round,
        started.elapsed()
    );
    let ok = duty <= DUTY_BUDGET
        && sd <= SEARCH_BUDGET + NOISE_ALLOWANCE
        && id >= -(INGEST_BUDGET + NOISE_ALLOWANCE)
        && bytes.max(wide_bytes) <= STORAGE_BUDGET;
    Ok(ok)
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    match rt.block_on(run(&a)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) if a.gate => {
            eprintln!("a reporting-overhead budget was missed");
            ExitCode::FAILURE
        }
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reporting-overhead: {e}");
            ExitCode::FAILURE
        }
    }
}
