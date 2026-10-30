//! CR-SIM: a deterministic replay harness for crawler candidates.
//!
//! Drives the real DHT node actor and the real fetch pool in tokio's
//! paused clock against a scripted network (`net.rs`) and scripted
//! metadata peers (`fetch.rs`). The same seed gives a byte-identical
//! report; the baselines' reports are checked in under `golden/`.
//! See `README.md` for regenerating them and for A/B-ing a candidate.

mod fetch;
mod log;
mod net;
mod rng;
mod run;
mod scenario;

use std::path::PathBuf;

use scenario::Scenario;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sim/golden")
        .join(format!("{name}.txt"))
}

/// A fresh current-thread runtime with the clock paused: the only
/// scheduler shape whose task order is reproducible.
fn simulate(s: &Scenario) -> run::Report {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("runtime");
    let wall = std::time::Instant::now();
    let report = rt.block_on(run::run(s));
    eprintln!(
        "crawler-sim `{}`: {} simulated min in {:.2?} wall, stored {}",
        s.name,
        s.minutes,
        wall.elapsed(),
        report.stored
    );
    if let Some(dir) = std::env::var_os("CRAWLER_SIM_LOG") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("log dir");
        let path = dir.join(format!("{}-seed{}.log", s.name, s.seed));
        std::fs::write(&path, report.log.as_deref().unwrap_or_default()).expect("write log");
        eprintln!("crawler-sim: outcome log at {}", path.display());
    }
    report
}

/// Compares a report with its golden, or rewrites the golden when
/// `CRAWLER_SIM_BLESS=1`.
fn check_golden(s: &Scenario) {
    let report = simulate(s);
    let path = golden_path(s.name);
    if std::env::var("CRAWLER_SIM_BLESS").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &report.text).expect("write golden");
        eprintln!("crawler-sim: blessed {}", path.display());
        return;
    }
    let golden = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "no golden at {} ({e}); run with CRAWLER_SIM_BLESS=1",
                path.display()
            )
        })
        // A Windows checkout may have turned LF into CRLF.
        .replace("\r\n", "\n");
    let now = comparable(&golden, &report.text);
    if golden != now {
        let first = golden
            .lines()
            .zip(now.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| golden.lines().count().min(now.lines().count()));
        panic!(
            "`{}` differs from {} at line {}:\n  golden: {:?}\n  now:    {:?}\n\nfull report:\n{}\n\
             If the change is intended, regenerate with CRAWLER_SIM_BLESS=1 and explain it in the PR.",
            s.name,
            path.display(),
            first + 1,
            golden.lines().nth(first).unwrap_or("<end>"),
            now.lines().nth(first).unwrap_or("<end>"),
            report.text
        );
    }
}

/// The report minus counters the golden doesn't know that read 0: a new
/// counter added by a candidate stays 0 with its switch off, so the
/// golden still proves the switch inert.
fn comparable(golden: &str, report: &str) -> String {
    let known: std::collections::HashSet<&str> = golden
        .lines()
        .filter_map(|l| l.split_once('=').map(|(name, _)| name))
        .collect();
    let mut out = String::new();
    for line in report.lines() {
        let new_zero = line
            .split_once('=')
            .is_some_and(|(name, v)| v == "0" && !name.contains(' ') && !known.contains(name));
        if !new_zero {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Metadata stored over `seeds` seeds of one scenario: (mean, min, max).
/// One seed is inside the simulator's noise; compare arms over several.
pub fn stored_over_seeds(s: &Scenario, seeds: std::ops::Range<u64>) -> (f64, u64, u64) {
    let stored: Vec<u64> = seeds
        .map(|seed| {
            let mut s = s.clone();
            s.seed = seed;
            simulate(&s).stored
        })
        .collect();
    let mean = stored.iter().sum::<u64>() as f64 / stored.len().max(1) as f64;
    let min = stored.iter().copied().min().unwrap_or(0);
    let max = stored.iter().copied().max().unwrap_or(0);
    (mean, min, max)
}

/// Stored metadata, "on" arm minus "off" arm, per seed. The network keys
/// its luck per node and query, so the arms of one seed share their random
/// numbers and the difference isolates the switch better than unpaired
/// means do.
#[allow(dead_code)] // For candidates' A/B tests (README, "A/B a candidate").
pub fn paired_stored_difference(
    off: &Scenario,
    on: &Scenario,
    seeds: std::ops::Range<u64>,
) -> Vec<i64> {
    seeds
        .map(|seed| {
            let (mut a, mut b) = (off.clone(), on.clone());
            a.seed = seed;
            b.seed = seed;
            simulate(&b).stored as i64 - simulate(&a).stored as i64
        })
        .collect()
}

#[test]
fn healthy_matches_golden() {
    check_golden(&scenario::healthy());
}

#[test]
fn high_latency_matches_golden() {
    check_golden(&scenario::high_latency());
}

#[test]
fn admission_pressure_matches_golden() {
    check_golden(&scenario::admission_pressure());
}

/// Two runs of one seed in one process are byte-identical, and a different
/// seed changes the outcome (so the seed really drives the run).
#[test]
fn same_seed_is_byte_identical() {
    let mut s = scenario::healthy();
    s.minutes = 3;
    let a = simulate(&s);
    let b = simulate(&s);
    assert_eq!(a.text, b.text);
    s.seed = 2;
    let c = simulate(&s);
    assert_ne!(a.text, c.text);
}

/// The harness measures what it claims: the healthy network stores
/// metadata, admits owned lookups and respects the actor's 32-request cap.
#[test]
fn healthy_run_is_plausible() {
    let mut s = scenario::healthy();
    s.minutes = 3;
    let r = simulate(&s);
    assert!(r.stored > 0, "nothing stored:\n{}", r.text);
    assert!(r.dht.peer_admitted > 0, "no lookups admitted:\n{}", r.text);
    assert!(r.dht.samples_sent > 0, "no BEP 51 samples:\n{}", r.text);
    assert!(r.dht.peer_active + r.dht.peer_queued <= 32, "{}", r.text);
}

/// A golden check ignores new zero counters but nothing else.
#[test]
fn comparison_tolerates_only_new_zero_counters() {
    let golden = "a=1\nb=2\n";
    assert_eq!(comparable(golden, "a=1\nnew=0\nb=2\n"), golden);
    assert_ne!(comparable(golden, "a=1\nnew=3\nb=2\n"), golden);
    assert_ne!(comparable(golden, "a=0\nb=2\n"), golden);
}

/// A remote rate limit drops our queries, and the node sees them time out.
#[test]
fn rate_limits_drop_queries() {
    let mut s = scenario::healthy();
    s.minutes = 2;
    s.net.rate_limit_qps = None;
    let open = simulate(&s);
    s.net.rate_limit_qps = Some(1);
    let limited = simulate(&s);
    assert_eq!(open.net.rate_limited, 0);
    assert!(limited.net.rate_limited > 0, "{}", limited.text);
    assert!(limited.dht.timeouts > open.dht.timeouts, "{}", limited.text);
}

/// An hour of the admission-pressure network: too slow for every CI run.
/// `cargo test -p lodestar-sources --test sim -- --ignored`
#[test]
#[ignore = "stress: about a minute of wall time"]
fn admission_pressure_hour() {
    let mut s = scenario::admission_pressure();
    s.minutes = 60;
    let r = simulate(&s);
    println!("{}", r.text);
    assert!(r.stored > 0, "{}", r.text);
    assert!(r.dht.peer_active + r.dht.peer_queued <= 32, "{}", r.text);
}

/// The noise floor A/B comparisons have to clear: each baseline's stored
/// count over seeds 1..=8. `cargo test -p lodestar-sources --test sim noise_floor -- --ignored --nocapture`
#[test]
#[ignore = "measurement: about 15 s of wall time"]
fn noise_floor() {
    for s in [
        scenario::healthy(),
        scenario::high_latency(),
        scenario::admission_pressure(),
    ] {
        let (mean, min, max) = stored_over_seeds(&s, 1..9);
        println!(
            "noise {}: stored mean={mean:.1} min={min} max={max} spread={:.1}%",
            s.name,
            100.0 * (max - min) as f64 / mean.max(1.0)
        );
    }
}
