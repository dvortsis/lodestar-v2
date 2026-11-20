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
mod restart;
mod rng;
mod run;
mod scenario;

use std::path::PathBuf;

use scenario::{Scenario, Switches};

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

/// Runs `s` on every [`SCREEN_SEEDS`] seed, one thread each (each run
/// has its own runtime, so the threads don't change any run's result).
fn over_seeds(s: &Scenario) -> Vec<run::Report> {
    std::thread::scope(|scope| {
        let runs: Vec<_> = SCREEN_SEEDS
            .map(|seed| {
                let mut s = s.clone();
                s.seed = seed;
                scope.spawn(move || simulate(&s))
            })
            .collect();
        runs.into_iter()
            .map(|r| r.join().expect("sim thread"))
            .collect()
    })
}

/// The seeds every screening uses. Fixed, so a candidate can't pick the
/// seeds it wins on.
pub const SCREEN_SEEDS: std::ops::Range<u64> = 1..9;

/// A screening result: the sign of every paired difference agrees, or not.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub outcome: Outcome,
    /// Steady-state stored per minute, "on" minus "off", per seed in
    /// [`SCREEN_SEEDS`] order.
    pub diffs: Vec<f64>,
    /// Each seed's capacity ledger, `[off, on]`: `W`, `q` and `y` for a
    /// lookup-cost change (README, "The calibrated scenario").
    pub ledgers: Vec<[run::Ledger; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// "On" stored more on every seed.
    Better,
    /// "On" stored less on every seed.
    Worse,
    /// Mixed signs, or a tie on any seed.
    Inconclusive,
}

/// Screens a candidate: runs both arms on each of [`SCREEN_SEEDS`] (the
/// arms of a seed share their network luck, since the network keys it per
/// node and query) and applies a sign test to the post-warmup stored rate.
/// A tie on any seed counts against. This is the only comparison
/// candidates use (README, "A/B a candidate").
#[allow(dead_code)] // Called by candidates' A/B tests.
pub fn screen(off: &Scenario, on: &Scenario) -> Verdict {
    let (a, b) = (over_seeds(off), over_seeds(on));
    let pairs = || a.iter().zip(&b);
    let diffs: Vec<f64> = pairs()
        .map(|(a, b)| b.steady_stored - a.steady_stored)
        .collect();
    Verdict {
        outcome: sign_test(&diffs),
        diffs,
        ledgers: pairs().map(|(a, b)| [a.ledger, b.ledger]).collect(),
    }
}

fn sign_test(diffs: &[f64]) -> Outcome {
    if !diffs.is_empty() && diffs.iter().all(|d| *d > 0.0) {
        Outcome::Better
    } else if !diffs.is_empty() && diffs.iter().all(|d| *d < 0.0) {
        Outcome::Worse
    } else {
        Outcome::Inconclusive
    }
}

/// One scenario's steady-state stored rate over [`SCREEN_SEEDS`]:
/// (mean, min, max), for the noise floor.
fn steady_over_seeds(s: &Scenario) -> (f64, f64, f64) {
    let stored: Vec<f64> = SCREEN_SEEDS
        .map(|seed| {
            let mut s = s.clone();
            s.seed = seed;
            simulate(&s).steady_stored
        })
        .collect();
    let mean = stored.iter().sum::<f64>() / stored.len().max(1) as f64;
    let min = stored.iter().copied().fold(f64::INFINITY, f64::min);
    let max = stored.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, min, max)
}

#[test]
fn sign_test_needs_every_seed_and_fails_ties() {
    assert_eq!(sign_test(&[1.0, 0.5, 2.0]), Outcome::Better);
    assert_eq!(sign_test(&[-1.0, -0.5]), Outcome::Worse);
    assert_eq!(sign_test(&[1.0, -0.5, 2.0]), Outcome::Inconclusive);
    assert_eq!(sign_test(&[1.0, 0.0, 2.0]), Outcome::Inconclusive);
    assert_eq!(sign_test(&[]), Outcome::Inconclusive);
}

/// A switch that changes nothing ties on every seed, which fails the
/// screen. One seed shows it; all of `SCREEN_SEEDS` would cost the ignored
/// `noise_floor`'s run time.
#[test]
fn an_inert_switch_screens_inconclusive() {
    let mut s = scenario::healthy();
    s.minutes = 4;
    s.seed = SCREEN_SEEDS.start;
    let (off, on) = (simulate(&s), simulate(&s));
    assert_eq!(
        sign_test(&[on.steady_stored - off.steady_stored]),
        Outcome::Inconclusive
    );
}

/// X-crawler-sim-calibrate: the calibrated live-lookup scenario, seed 1.
#[test]
fn live_lookup_matches_golden() {
    check_golden(&scenario::live_lookup());
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

/// CR-SIM-restart: a clean restart with `dht.safe_restart` on. The run
/// itself asserts that no node is sampled before its restored obligation
/// expires, that sampling resumes at once, and that the counters add up.
#[test]
fn restart_honoured_matches_golden() {
    check_golden(&scenario::restart_honoured());
}

/// The negative control: the same restart with `dht.safe_restart` off in
/// leg 2 suppresses in full, and no node is sampled in leg 2.
#[test]
fn restart_without_safe_restart_suppresses_in_full() {
    let mut s = scenario::restart_honoured();
    s.minutes = 10;
    if let Some(r) = &mut s.restart {
        r.safe_restart = false;
    }
    let r = simulate(&s);
    assert_eq!(
        r.dht.sample_suppression_reason,
        lodestar_dht::restart::Reason::Off,
        "{}",
        r.text
    );
    assert_eq!(r.dht.samples_sent, 0, "{}", r.text);
    // The full window: 6 h plus the query timeout from leg 2's start, less
    // the leg's 10 min.
    assert!(
        r.dht.sample_suppression_seconds >= 6 * 3600 + 4 - 600,
        "{}",
        r.text
    );
    assert_eq!(r.dht.restart_obligations_restored, 0, "{}", r.text);
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
/// `cargo test -p lodestar-crawler-sim --test sim -- --ignored`
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
/// count over seeds 1..=8. `cargo test -p lodestar-crawler-sim --test sim noise_floor -- --ignored --nocapture`
#[test]
#[ignore = "measurement: about 15 s of wall time"]
fn noise_floor() {
    for s in [
        scenario::healthy(),
        scenario::high_latency(),
        scenario::admission_pressure(),
    ] {
        let (mean, min, max) = steady_over_seeds(&s);
        println!(
            "noise {}: steady stored/min mean={mean:.1} min={min:.1} max={max:.1} spread={:.1}%",
            s.name,
            100.0 * (max - min) / mean.max(1.0)
        );
    }
}

/// CR-ADM's switch on the H2 baseline, one short seed: the actor stops
/// refusing for capacity, and the run still conserves and stores.
#[test]
fn coordinated_admission_stops_capacity_refusals() {
    let mut off = scenario::admission_pressure();
    off.minutes = 4;
    let on = Scenario {
        switches: Switches {
            coordinated_admission: true,
            ..Switches::default()
        },
        ..off.clone()
    };
    let (a, b) = (simulate(&off), simulate(&on));
    assert!(a.dht.peer_rejected_capacity > 1_000, "{}", a.text);
    assert_eq!(b.dht.peer_rejected_capacity, 0, "{}", b.text);
    assert!(b.text.contains("coordinated_admission=true"));
    assert!(b.pool.admission[1] > 0, "grants:\n{}", b.text);
    assert!(b.stored > 0, "{}", b.text);
    assert!(b.dht.peer_active + b.dht.peer_queued <= 32, "{}", b.text);
}

/// The CR-ADM screen: both arms on every `SCREEN_SEEDS` seed, per scenario,
/// with what the brief asks the PR to report.
/// `cargo test -p lodestar-crawler-sim --test sim coordinated_admission_screen -- --ignored --nocapture`
#[test]
#[ignore = "measurement: a few minutes of wall time"]
fn coordinated_admission_screen() {
    let mut production = scenario::admission_pressure();
    production.name = "admission-pressure@defaults";
    production.pool.max_pending = lodestar_sources::PoolConfig::default().max_pending;
    production.pool.retry_backoff = lodestar_sources::PoolConfig::default().retry_backoff;
    for off in [
        scenario::admission_pressure(),
        production,
        scenario::healthy(),
        scenario::high_latency(),
    ] {
        let on = Scenario {
            switches: Switches {
                coordinated_admission: true,
                ..Switches::default()
            },
            ..off.clone()
        };
        println!("\n## {}", off.name);
        println!(
            "seed | arm | actor rejected | pool unaccepted | admitted | completed | evicted (unserved/lookup/peers/fetch) | stored | steady/min"
        );
        let mut diffs = Vec::new();
        let mut steady = [Vec::new(), Vec::new()];
        for seed in SCREEN_SEEDS {
            let mut row = [0.0; 2];
            for (arm, s) in [(0, &off), (1, &on)] {
                let mut s = s.clone();
                s.seed = seed;
                let r = simulate(&s);
                let e = r.pool.evicted_service;
                println!(
                    "{seed} | {} | {} | {} | {} | {} | {}/{}/{}/{} | {} | {:.1}",
                    ["off", "on"][arm],
                    r.dht.peer_rejected,
                    r.pool.lookup_unaccepted,
                    r.dht.peer_admitted,
                    r.dht.lookups_completed,
                    e[0],
                    e[1],
                    e[2],
                    e[3],
                    r.stored,
                    r.steady_stored
                );
                row[arm] = r.steady_stored;
                steady[arm].push(r.steady_stored);
            }
            diffs.push(row[1] - row[0]);
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let spread = |v: &[f64]| {
            let (lo, hi) = v
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                    (a.min(*x), b.max(*x))
                });
            100.0 * (hi - lo) / mean(v).max(1.0)
        };
        println!(
            "steady stored/min: off mean {:.1} (spread {:.1}%), on mean {:.1} (spread {:.1}%), change {:+.1}%; sign test {:?}; diffs {:?}",
            mean(&steady[0]),
            spread(&steady[0]),
            mean(&steady[1]),
            spread(&steady[1]),
            100.0 * (mean(&steady[1]) - mean(&steady[0])) / mean(&steady[0]).max(1.0),
            sign_test(&diffs),
            diffs
                .iter()
                .map(|d| (d * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
}

/// CR-1a-H's switch on a short healthy run whose samplers hold what they
/// list: hinted queries go out and find peers, the guard stays bounded,
/// and the same seed is still byte-identical.
#[test]
fn responder_hints_send_hinted_queries_deterministically() {
    let mut off = scenario::healthy();
    off.minutes = 4;
    off.net.samplers_hold = true;
    let on = Scenario {
        switches: Switches {
            responder_hints: true,
            ..Switches::default()
        },
        ..off.clone()
    };
    let (a, b) = (simulate(&off), simulate(&on));
    assert_eq!(a.dht.hint_queries + a.dht.hints_captured, 0, "{}", a.text);
    assert!(b.text.contains("responder_hints=true"));
    assert!(b.dht.hints_captured > 0, "{}", b.text);
    assert!(b.dht.hint_queries > 0, "{}", b.text);
    assert!(b.dht.hint_useful > 0, "{}", b.text);
    assert!(b.dht.hint_useful <= b.dht.hint_queries);
    assert!(b.dht.hint_guard_entries <= 20_000);
    assert!(b.stored > 0, "{}", b.text);
    assert!(b.dht.peer_active + b.dht.peer_queued <= 32, "{}", b.text);
    assert_eq!(simulate(&on).text, b.text);
}

/// The CR-1a-H screen: hints off against on over `SCREEN_SEEDS`, both arms
/// on a network whose samplers answer `get_peers` for what they list.
/// `cargo test -p lodestar-crawler-sim --test sim responder_hints_screen -- --ignored --nocapture`
#[test]
#[ignore = "measurement: a few minutes of wall time"]
fn responder_hints_screen() {
    for mut off in [
        scenario::high_latency(),
        scenario::healthy(),
        scenario::admission_pressure(),
    ] {
        off.net.samplers_hold = true;
        let on = Scenario {
            switches: Switches {
                responder_hints: true,
                ..Switches::default()
            },
            ..off.clone()
        };
        println!("\n## {}", off.name);
        println!(
            "seed | arm | admitted | completed | lookup ms | captured | expired unused | queries | useful | fallbacks (expired/throttled/guard_full/other) | guard skips | stored | steady/min"
        );
        let mut diffs = Vec::new();
        let mut steady = [Vec::new(), Vec::new()];
        let mut ms = [Vec::new(), Vec::new()];
        for seed in SCREEN_SEEDS {
            let mut row = [0.0; 2];
            for (arm, s) in [(0, &off), (1, &on)] {
                let mut s = s.clone();
                s.seed = seed;
                let r = simulate(&s);
                let d = &r.dht;
                let f = d.hint_fallbacks;
                let lookup_ms = d.peer_lookup_millis as f64 / d.peer_lookups_answered.max(1) as f64;
                println!(
                    "{seed} | {} | {} | {} | {:.0} | {} | {} | {} | {} | {}/{}/{}/{} | {} | {} | {:.1}",
                    ["off", "on"][arm],
                    d.peer_admitted,
                    d.lookups_completed,
                    lookup_ms,
                    d.hints_captured,
                    d.hints_expired,
                    d.hint_queries,
                    d.hint_useful,
                    f[0],
                    f[4],
                    f[5],
                    f[1] + f[2] + f[3] + f[6],
                    d.hint_guard_skips,
                    r.stored,
                    r.steady_stored
                );
                row[arm] = r.steady_stored;
                steady[arm].push(r.steady_stored);
                ms[arm].push(lookup_ms);
            }
            diffs.push(row[1] - row[0]);
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let spread = |v: &[f64]| {
            let (lo, hi) = v
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                    (a.min(*x), b.max(*x))
                });
            100.0 * (hi - lo) / mean(v).max(1.0)
        };
        println!(
            "steady stored/min: off mean {:.1} (spread {:.1}%), on mean {:.1}, change {:+.1}%; lookup ms: off {:.0}, on {:.0}; sign test {:?}; diffs {:?}",
            mean(&steady[0]),
            spread(&steady[0]),
            mean(&steady[1]),
            100.0 * (mean(&steady[1]) - mean(&steady[0])) / mean(&steady[0]).max(1.0),
            mean(&ms[0]),
            mean(&ms[1]),
            sign_test(&diffs),
            diffs
                .iter()
                .map(|d| (d * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
}

/// CR-1a-S's switch on a short healthy run: sessions really stream (the
/// glue forwards them; without it the "on" arm would quietly run the old
/// path), the off arm counts nothing, and the same seed is byte-identical.
#[test]
fn streaming_peers_stream_batches_deterministically() {
    let mut off = scenario::healthy();
    off.minutes = 4;
    let on = Scenario {
        switches: Switches {
            streaming_peers: true,
            ..Switches::default()
        },
        ..off.clone()
    };
    let (a, b) = (simulate(&off), simulate(&on));
    assert_eq!(a.dht.peer_stream_batches, 0, "{}", a.text);
    assert_eq!(a.pool.stream_batches, 0, "{}", a.text);
    assert!(b.text.contains("streaming_peers=true"));
    assert!(b.dht.peer_stream_batches > 0, "{}", b.text);
    assert!(b.pool.stream_batches > 0, "{}", b.text);
    assert!(
        b.pool.stream_terminals.iter().sum::<u64>() > 0,
        "{}",
        b.text
    );
    assert!(b.pool.stream_first_fetches > 0, "{}", b.text);
    assert!(b.stored > 0, "{}", b.text);
    assert!(b.dht.peer_active + b.dht.peer_queued <= 32, "{}", b.text);
    assert_eq!(simulate(&on).text, b.text);
}

/// The CR-1a-S screen: streaming off against on over `SCREEN_SEEDS`, on
/// each baseline scenario. Conversion is stored per admitted lookup. Time
/// to first fetch is, off, the lookup's run time (peers arrive only at its
/// end) and, on, the time to the first batch plus the pool's wait from
/// that batch to the fetch.
/// `cargo test -p lodestar-crawler-sim --test sim streaming_peers_screen -- --ignored --nocapture`
#[test]
#[ignore = "measurement: a few minutes of wall time"]
fn streaming_peers_screen() {
    for off in [
        scenario::healthy(),
        scenario::high_latency(),
        scenario::admission_pressure(),
    ] {
        let on = Scenario {
            switches: Switches {
                streaming_peers: true,
                ..Switches::default()
            },
            ..off.clone()
        };
        println!("\n## {}", off.name);
        println!(
            "seed | arm | admitted | completed | stored | conversion | first fetch ms | batches | backpressure | capped | source failed | steady/min"
        );
        let mut diffs = Vec::new();
        let [mut steady, mut conv, mut first] = <[[Vec<f64>; 2]; 3]>::default();
        for seed in SCREEN_SEEDS {
            let mut row = [0.0; 2];
            for (arm, s) in [(0, &off), (1, &on)] {
                let mut s = s.clone();
                s.seed = seed;
                let r = simulate(&s);
                let d = &r.dht;
                let per = |sum: u64, n: u64| sum as f64 / n.max(1) as f64;
                let first_ms = if arm == 0 {
                    per(d.peer_lookup_millis, d.peer_lookups_answered)
                } else {
                    per(
                        d.peer_stream_first_batch_millis,
                        d.peer_stream_first_batches,
                    ) + per(
                        r.pool.stream_first_fetch_millis,
                        r.pool.stream_first_fetches,
                    )
                };
                let conversion = r.stored as f64 / d.peer_admitted.max(1) as f64;
                println!(
                    "{seed} | {} | {} | {} | {} | {:.3} | {:.0} | {} | {} | {} | {} | {:.1}",
                    ["off", "on"][arm],
                    d.peer_admitted,
                    d.lookups_completed,
                    r.stored,
                    conversion,
                    first_ms,
                    d.peer_stream_batches,
                    d.peer_stream_backpressure,
                    r.text
                        .lines()
                        .find_map(|l| l.strip_prefix("stream_peers_capped="))
                        .unwrap_or("0"),
                    r.pool.stream_terminals[3],
                    r.steady_stored
                );
                row[arm] = r.steady_stored;
                steady[arm].push(r.steady_stored);
                conv[arm].push(conversion);
                first[arm].push(first_ms);
            }
            diffs.push(row[1] - row[0]);
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let spread = |v: &[f64]| {
            let (lo, hi) = v
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                    (a.min(*x), b.max(*x))
                });
            100.0 * (hi - lo) / mean(v).max(1.0)
        };
        println!(
            "steady stored/min: off mean {:.1} (spread {:.1}%), on mean {:.1}, change {:+.1}%; conversion: off {:.3}, on {:.3}; first fetch ms: off {:.0}, on {:.0}; sign test {:?}; diffs {:?}",
            mean(&steady[0]),
            spread(&steady[0]),
            mean(&steady[1]),
            100.0 * (mean(&steady[1]) - mean(&steady[0])) / mean(&steady[0]).max(1.0),
            mean(&conv[0]),
            mean(&conv[1]),
            mean(&first[0]),
            mean(&first[1]),
            sign_test(&diffs),
            diffs
                .iter()
                .map(|d| (d * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
}

/// CR-1b's switch on a short healthy run: sample and exploration targets
/// really take the cursors' prefixes, the off arm counts none, the query
/// rate and BEP 51 intervals hold, and the same seed is byte-identical.
#[test]
fn target_prefixes_sweep_deterministically() {
    let mut off = scenario::healthy();
    off.minutes = 4;
    let on = Scenario {
        switches: Switches {
            target_prefixes: true,
            ..Switches::default()
        },
        ..off.clone()
    };
    let (a, b) = (simulate(&off), simulate(&on));
    assert_eq!(a.dht.target_prefix_queries, [0, 0], "{}", a.text);
    assert_eq!(a.dht.target_prefix_cycles, 0);
    assert!(b.text.contains("target_prefixes=true"));
    let [explore, sample] = b.dht.target_prefix_queries;
    assert!(explore > 0, "{}", b.text);
    assert_eq!(sample, b.dht.samples_sent, "every sample took a prefix");
    assert!(b.dht.target_prefix_cycles > 0, "{}", b.text);
    // Both arms count novelty, so the A/B compares like with like.
    assert!(a.dht.target_novel_nodes[1] > 0 && b.dht.target_novel_nodes[1] > 0);
    // Politeness: the query rate's bucket and the BEP 51 intervals.
    let secs = 60 * u64::from(on.minutes);
    let qps = u64::from(on.intensity.queries_per_sec());
    assert!(b.dht.queries_out <= qps * (secs + 1), "{}", b.text);
    assert_eq!(b.net.sample_early, 0, "{}", b.text);
    assert!(b.net.rate_limited <= a.net.rate_limited, "{}", b.text);
    assert_eq!(simulate(&on).text, b.text);
}

/// The CR-1b screen: random targets against the 256-prefix sweep over
/// `SCREEN_SEEDS`. Stored per minute is the primary metric; new sample
/// hashes, new frontier contacts and the per-call yield are discovery's.
/// `cargo test -p lodestar-crawler-sim --test sim target_prefixes_screen -- --ignored --nocapture`
#[test]
#[ignore = "measurement: a few minutes of wall time"]
fn target_prefixes_screen() {
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let spread = |v: &[f64]| {
        let (lo, hi) = v
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                (a.min(*x), b.max(*x))
            });
        100.0 * (hi - lo) / mean(v).max(1.0)
    };
    let change = |v: &[Vec<f64>; 2]| 100.0 * (mean(&v[1]) - mean(&v[0])) / mean(&v[0]).max(1.0);
    for off in [
        scenario::healthy(),
        scenario::high_latency(),
        scenario::admission_pressure(),
    ] {
        let on = Scenario {
            switches: Switches {
                target_prefixes: true,
                ..Switches::default()
            },
            ..off.clone()
        };
        println!("\n## {}", off.name);
        println!(
            "seed | arm | samples | new hashes | hashes/sample | novel sample/explore | cycles | early | rate limited | stored | steady/min"
        );
        let mut steady: [Vec<f64>; 2] = Default::default();
        let mut hashes: [Vec<f64>; 2] = Default::default();
        let mut novel: [Vec<f64>; 2] = Default::default();
        let (mut d_stored, mut d_hashes) = (Vec::new(), Vec::new());
        for seed in SCREEN_SEEDS {
            let mut row = [(0.0, 0.0); 2];
            for (arm, s) in [(0, &off), (1, &on)] {
                let mut s = s.clone();
                s.seed = seed;
                let r = simulate(&s);
                let d = &r.dht;
                println!(
                    "{seed} | {} | {} | {} | {:.2} | {}/{} | {} | {} | {} | {} | {:.1}",
                    ["off", "on"][arm],
                    d.samples_sent,
                    d.harvested_sample,
                    d.harvested_sample as f64 / d.samples_sent.max(1) as f64,
                    d.target_novel_nodes[1],
                    d.target_novel_nodes[0],
                    d.target_prefix_cycles,
                    r.net.sample_early,
                    r.net.rate_limited,
                    r.stored,
                    r.steady_stored
                );
                let h = d.harvested_sample as f64;
                row[arm] = (r.steady_stored, h);
                steady[arm].push(r.steady_stored);
                hashes[arm].push(h);
                novel[arm].push(d.target_novel_nodes.iter().sum::<u64>() as f64);
            }
            d_stored.push(row[1].0 - row[0].0);
            d_hashes.push(row[1].1 - row[0].1);
        }
        println!(
            "steady stored/min: off {:.1} (spread {:.1}%), on {:.1}, change {:+.1}%, sign {:?}; new hashes: off {:.0} (spread {:.1}%), on {:.0}, change {:+.1}%, sign {:?}; novel contacts: off {:.0}, on {:.0}, change {:+.1}%",
            mean(&steady[0]),
            spread(&steady[0]),
            mean(&steady[1]),
            change(&steady),
            sign_test(&d_stored),
            mean(&hashes[0]),
            spread(&hashes[0]),
            mean(&hashes[1]),
            change(&hashes),
            sign_test(&d_hashes),
            mean(&novel[0]),
            mean(&novel[1]),
            change(&novel),
        );
    }
}

/// CR-0's 24 h figures the `live-lookup` scenario is calibrated to, each
/// with the tolerance every seed and the 8-seed mean must meet: (name,
/// target, tolerance, value). README, "The calibrated scenario".
fn calibration(l: &run::Ledger) -> [(&'static str, f64, f64, f64); 10] {
    [
        ("unanswered", 0.382, 0.03, l.unanswered),
        ("L slots", 30.68, 1.0, l.slots),
        ("W s", 23.86, 1.5, l.w),
        ("admitted/s", 1.286, 0.1, l.admitted_per_s),
        ("y", 0.160, 0.04, l.y),
        ("fetches", 3.66, 0.6, l.fetches),
        ("stored/min", 12.34, 3.1, l.stored_per_min),
        ("queries/s", 76.1, 7.6, l.queries_per_s),
        ("q_all", 59.2, 6.0, l.q_all),
        ("outstanding", 160.0, 24.0, l.outstanding),
    ]
}

/// X-crawler-sim-calibrate: `live-lookup` lands within tolerance of CR-0
/// on every `SCREEN_SEEDS` seed, and on their mean. Prints the table the
/// README quotes:
/// `cargo test -p lodestar-crawler-sim --test sim live_lookup_is_calibrated -- --nocapture`
#[test]
fn live_lookup_is_calibrated() {
    let reports = over_seeds(&scenario::live_lookup());
    let ledgers: Vec<run::Ledger> = reports.iter().map(|r| r.ledger).collect();
    println!(
        "seed | L | admitted/s | W s | W_residence s | q | q_all | y | fetches | stored/min | queries/s | outstanding | unanswered | deadlines/completed"
    );
    let row = |name: String, l: &run::Ledger| {
        println!(
            "{name} | {:.2} | {:.3} | {:.2} | {:.2} | {:.1} | {:.1} | {:.3} | {:.2} | {:.2} | {:.1} | {:.1} | {:.3} | {:.2}",
            l.slots,
            l.admitted_per_s,
            l.w,
            l.w_residence,
            l.q,
            l.q_all,
            l.y,
            l.fetches,
            l.stored_per_min,
            l.queries_per_s,
            l.outstanding,
            l.unanswered,
            l.deadlines_per_completed
        );
    };
    for (seed, l) in SCREEN_SEEDS.zip(&ledgers) {
        row(seed.to_string(), l);
    }
    let n = ledgers.len() as f64;
    let mean = |f: fn(&run::Ledger) -> f64| ledgers.iter().map(f).sum::<f64>() / n;
    let m = run::Ledger {
        slots: mean(|l| l.slots),
        admitted_per_s: mean(|l| l.admitted_per_s),
        w: mean(|l| l.w),
        w_residence: mean(|l| l.w_residence),
        q: mean(|l| l.q),
        q_all: mean(|l| l.q_all),
        y: mean(|l| l.y),
        fetches: mean(|l| l.fetches),
        found: mean(|l| l.found),
        stored_per_min: mean(|l| l.stored_per_min),
        queries_per_s: mean(|l| l.queries_per_s),
        outstanding: mean(|l| l.outstanding),
        unanswered: mean(|l| l.unanswered),
        deadlines_per_completed: mean(|l| l.deadlines_per_completed),
        ended_share: std::array::from_fn(|i| {
            ledgers.iter().map(|l| l.ended_share[i]).sum::<f64>() / n
        }),
    };
    row("mean".into(), &m);
    let mut misses = Vec::new();
    for (who, l) in SCREEN_SEEDS
        .map(|s| format!("seed {s}"))
        .chain(["mean".to_string()])
        .zip(ledgers.iter().chain([&m]))
    {
        for (name, target, tol, v) in calibration(l) {
            if (v - target).abs() > tol {
                misses.push(format!("{who}: {name}={v:.3}, target {target}±{tol}"));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "outside tolerance:\n{}",
        misses.join("\n")
    );
}

/// CR-LOOKUP-END runs deterministically on the calibrated scenario, and
/// with the switch on some lookups end on enough values.
#[test]
fn lookup_end_runs_deterministically() {
    let on = Scenario {
        switches: Switches {
            lookup_end: true,
            ..Switches::default()
        },
        ..scenario::live_lookup()
    };
    let (a, b) = (simulate(&on), simulate(&on));
    assert_eq!(a.text, b.text, "same seed, same run");
    assert!(a.text.contains("lookup_end=true"));
    assert!(a.ledger.ended_share[lodestar_dht::node::LedgerEnd::EnoughValues as usize] > 0.0);
}

/// The CR-LOOKUP-END screen: the calibrated `live-lookup` scenario, the
/// switch off against on, over `SCREEN_SEEDS`. Reports `W`, `q` and `y`
/// beside stored per minute, and the two figures the change is kept on:
/// stored per slot-second (`stored/s ÷ L`) and stored per DHT query
/// (`stored ÷ queries_out`). Kept only if both rise.
/// `cargo test -p lodestar-crawler-sim --test sim lookup_end_screen -- --ignored --nocapture`
#[test]
#[ignore = "measurement: a few minutes of wall time"]
fn lookup_end_screen() {
    use lodestar_dht::node::LedgerEnd;
    let off = scenario::live_lookup();
    let on = Scenario {
        switches: Switches {
            lookup_end: true,
            ..Switches::default()
        },
        ..off.clone()
    };
    let v = screen(&off, &on);
    let per_slot_s = |l: &run::Ledger| l.stored_per_min / 60.0 / l.slots.max(1e-9);
    let per_query = |l: &run::Ledger| l.stored_per_min / 60.0 / l.queries_per_s.max(1e-9);
    let share = |l: &run::Ledger, why: LedgerEnd| l.ended_share[why as usize];
    println!(
        "seed | arm | L | admitted/s | W_little s | W s | q | q_all | y | stored/min | stored/slot-s | stored/query | deadline | enough_values | values"
    );
    let mut sums = [[0.0f64; 12]; 2];
    for (seed, pair) in SCREEN_SEEDS.zip(&v.ledgers) {
        for (arm, l) in pair.iter().enumerate() {
            let row = [
                l.slots,
                l.admitted_per_s,
                l.w,
                l.w_residence,
                l.q,
                l.q_all,
                l.y,
                l.stored_per_min,
                per_slot_s(l),
                per_query(l),
                share(l, LedgerEnd::Deadline),
                share(l, LedgerEnd::EnoughValues),
            ];
            for (s, x) in sums[arm].iter_mut().zip(row) {
                *s += x;
            }
            println!(
                "{seed} | {} | {:.2} | {:.3} | {:.2} | {:.2} | {:.1} | {:.1} | {:.3} | {:.2} | {:.5} | {:.6} | {:.3} | {:.3} | {:.3}",
                ["off", "on"][arm],
                row[0],
                row[1],
                row[2],
                row[3],
                row[4],
                row[5],
                row[6],
                row[7],
                row[8],
                row[9],
                row[10],
                row[11],
                share(l, LedgerEnd::Values),
            );
        }
    }
    let n = v.ledgers.len() as f64;
    for (arm, s) in sums.iter().enumerate() {
        println!(
            "mean | {} | {}",
            ["off", "on"][arm],
            s.iter()
                .map(|x| format!("{:.4}", x / n))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    let rises =
        |f: &dyn Fn(&run::Ledger) -> f64| v.ledgers.iter().filter(|p| f(&p[1]) > f(&p[0])).count();
    println!(
        "stored/min diffs {:?}: {:?}; stored/slot-s rose on {}/8 seeds, stored/query on {}/8",
        v.diffs,
        v.outcome,
        rises(&per_slot_s),
        rises(&per_query),
    );
}
