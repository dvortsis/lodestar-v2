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
    if golden != report.text {
        let first = golden
            .lines()
            .zip(report.text.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| golden.lines().count().min(report.text.lines().count()));
        panic!(
            "`{}` differs from {} at line {}:\n  golden: {:?}\n  now:    {:?}\n\nfull report:\n{}\n\
             If the change is intended, regenerate with CRAWLER_SIM_BLESS=1 and explain it in the PR.",
            s.name,
            path.display(),
            first + 1,
            golden.lines().nth(first).unwrap_or("<end>"),
            report.text.lines().nth(first).unwrap_or("<end>"),
            report.text
        );
    }
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

/// An hour of the admission-pressure network: too slow for every CI run.
/// `cargo test -p lodestar-sources --test sim -- --ignored`
#[test]
#[ignore = "stress: about a minute of wall time"]
fn admission_pressure_hour() {
    let mut s = scenario::admission_pressure();
    s.minutes = 60;
    let r = simulate(&s);
    println!("{}", r.text);
}
