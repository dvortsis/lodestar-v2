//! Scrape soak (M4 exit, `docs/decisions/2026-10-22-M4.md`): the scrape
//! scheduler working flat out on a stored corpus, against in-process fake
//! trackers, while the process's resident memory is sampled. Memory must
//! stay flat: lanes, rounds and per-tracker state are all bounded, so a
//! long run may not grow.
//!
//! Point it at a `search-bench --dir` corpus (its `bench.db`); the run
//! writes swarm rows into it.
//!
//! ```text
//! scrape-soak --dir PATH [--minutes 10] [--interval-ms 10]
//! ```
//!
//! Exits non-zero if resident memory grows more than 5% over the second
//! half of the run (Linux; elsewhere it reports but doesn't judge).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::health::HealthConfig;
use lodestar_pipeline::{HealthSetup, Pipeline, PipelineConfig, open_hot};
use lodestar_store::{Store, StoreConfig};
use lodestar_tracker_scrape::testing::{Fake, http_tracker, udp_tracker};
use lodestar_tracker_scrape::{Counts, TrackerUrl, UdpConfig};

const USAGE: &str = "usage: scrape-soak --dir PATH [--minutes N] [--interval-ms N]";
/// Memory growth allowed over the second half of the run.
const MAX_DRIFT: f64 = 0.05;

/// A swarm per hash, from its bytes: about 40% dead, the rest small and
/// heavy-tailed, like the bench's synthetic distribution.
fn answer(h: &InfoHash) -> Counts {
    if h.0[0] < 100 {
        return Counts::default();
    }
    let s = u32::from(h.0[1]) * u32::from(h.0[1]) / 512;
    Counts {
        seeders: s,
        leechers: u32::from(h.0[2]) / 16,
        completed: s * 10 + u32::from(h.0[3]),
    }
}

/// Resident memory in KiB, where the platform says.
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> std::process::ExitCode {
    let mut dir = None;
    let mut minutes = 10u64;
    let mut interval_ms = 10u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().unwrap_or_default();
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(value())),
            "--minutes" => minutes = value().parse().unwrap_or(minutes),
            "--interval-ms" => interval_ms = value().parse().unwrap_or(interval_ms),
            _ => {
                eprintln!("{USAGE}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("{USAGE}");
        return std::process::ExitCode::FAILURE;
    };
    match run(dir, minutes, interval_ms).await {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(e) => {
            eprintln!("scrape-soak: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(
    dir: PathBuf,
    minutes: u64,
    interval_ms: u64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let db = dir.join("bench.db");
    let mut store = Store::open(&db, StoreConfig::default())?;
    let started = Instant::now();
    let (hot, _) = open_hot(&mut store, &dir.join("hot"))?;
    let hot = Arc::new(hot);
    eprintln!(
        "hot columns for {} torrents in {:.1}s",
        hot.len(),
        started.elapsed().as_secs_f64()
    );
    let mut fakes: Vec<(Fake, bool)> = Vec::new();
    for _ in 0..3 {
        fakes.push((udp_tracker().await, true));
    }
    fakes.push((http_tracker().await, false));
    for (f, _) in &fakes {
        *f.state.answer.lock().expect("fake lock") = Some(answer);
    }
    let interval = Duration::from_millis(interval_ms.max(1));
    let health = HealthSetup {
        config: HealthConfig {
            interval,
            http_interval: interval * 3,
            round_deadline: Duration::from_secs(5),
            refill_every: Duration::from_millis(500),
            dht: false,
            udp: UdpConfig {
                timeout: Duration::from_secs(1),
                ..UdpConfig::default()
            },
            ..HealthConfig::default()
        },
        trackers: fakes
            .iter()
            .map(|(f, udp)| TrackerUrl::parse(&f.url(*udp)))
            .collect::<Result<_, _>>()?,
        db: db.clone(),
        store: StoreConfig::default(),
        dht: None,
    };
    let config = PipelineConfig {
        hot: Some(hot.clone()),
        health: Some(health),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::start(config, store, Vec::new()).await?;
    let monitor = pipeline.monitor();
    let h = monitor.health().ok_or("no scheduler")?.clone();
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let total = Duration::from_secs(minutes.max(1) * 60);
    let every = Duration::from_secs(30).min(total / 4);
    let began = Instant::now();
    let mut samples: Vec<(Duration, u64)> = Vec::new();
    println!(
        "{:>7} {:>10} {:>10} {:>12} {:>10}",
        "elapsed", "rss_mib", "rounds", "torrents", "per_s"
    );
    let mut last_scraped = 0;
    while began.elapsed() < total {
        tokio::time::sleep(every).await;
        let s = h.stats();
        let scraped = get(&s.alive) + get(&s.dead) + get(&s.unreached);
        let rss = rss_kib().unwrap_or(0);
        samples.push((began.elapsed(), rss));
        println!(
            "{:>6.0}s {:>10.1} {:>10} {:>12} {:>10.0}",
            began.elapsed().as_secs_f64(),
            rss as f64 / 1024.0,
            get(&s.rounds),
            scraped,
            (scraped - last_scraped) as f64 / every.as_secs_f64()
        );
        last_scraped = scraped;
    }
    pipeline.shutdown().await?;
    let second: Vec<u64> = samples
        .iter()
        .filter(|(t, _)| *t >= total / 2)
        .map(|(_, r)| *r)
        .collect();
    let (Some(&first), Some(&max)) = (second.first(), second.iter().max()) else {
        return Ok(true);
    };
    if first == 0 {
        println!("resident memory not available here: not judged");
        return Ok(true);
    }
    let drift = (max as f64 - first as f64) / first as f64;
    println!(
        "second half: {:.1} MiB → max {:.1} MiB ({:+.1}%, limit {:.0}%)",
        first as f64 / 1024.0,
        max as f64 / 1024.0,
        drift * 100.0,
        MAX_DRIFT * 100.0
    );
    Ok(drift <= MAX_DRIFT)
}
