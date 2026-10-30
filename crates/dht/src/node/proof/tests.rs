//! The restart proof's runtime half on a paused clock, with the file work
//! inline. Each "run" is a fresh `Proof` and `Sampler` on the same data and
//! witness directories, as a restart would make; a crash is a run dropped
//! without `commit`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, advance};

use super::*;
use crate::compact::NodeId;
use crate::restart::fail;
use crate::state::State;

const QUERY_TIMEOUT: Duration = Duration::from_secs(4);
const TICK: Duration = Duration::from_secs(1);

fn horizon() -> Duration {
    super::super::sampler::MAX_INTERVAL + QUERY_TIMEOUT
}

/// Wall time that moves with tokio's paused clock, from a fixed base.
#[derive(Debug)]
struct SimClock {
    base: Duration,
    start: Instant,
}

impl Clock for SimClock {
    fn wall(&self) -> Duration {
        self.base + Instant::now().duration_since(self.start)
    }
    fn mono(&self) -> Instant {
        Instant::now()
    }
}

/// A scratch root with a data dir and a witness dir beside it.
struct Dirs {
    root: PathBuf,
    data: PathBuf,
    witness: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Dirs {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lodestar-proof-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        let witness = root.join("witness");
        std::fs::create_dir_all(&data).unwrap();
        Dirs {
            root,
            data,
            witness,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.data.join("dht-state.bin")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn ids() -> Vec<NodeId> {
    vec![NodeId([7; 20]), NodeId([8; 20])]
}

fn state() -> State {
    State {
        ids: ids(),
        external_ip: None,
        nodes: Vec::new(),
    }
}

fn clock() -> Arc<SimClock> {
    Arc::new(SimClock {
        base: Duration::from_secs(1_700_000_000),
        start: Instant::now(),
    })
}

/// One run of the node, as far as the proof is concerned.
struct Run {
    proof: Proof,
    sampler: Sampler,
    stats: Stats,
}

impl Run {
    fn boot(d: &Dirs, clock: &Arc<SimClock>, on: bool) -> Run {
        Run::boot_with(d, clock, on, None)
    }

    fn boot_with(d: &Dirs, clock: &Arc<SimClock>, on: bool, delay: Option<Duration>) -> Run {
        // dht-state.bin exists, as after any earlier run.
        if !d.state_path().exists() {
            state().save(&d.state_path()).unwrap();
        }
        let cfg = on.then(|| RestartConfig {
            witness_dir: Some(d.witness.clone()),
            clock: clock.clone(),
            inline_io: true,
        });
        let off_cfg = (!on).then(|| RestartConfig {
            witness_dir: None,
            clock: clock.clone(),
            inline_io: true,
        });
        let path = d.state_path();
        let mut run = Run {
            proof: Proof::new(
                Some(&path),
                cfg.as_ref(),
                delay,
                horizon(),
                TICK,
                Instant::now(),
            ),
            sampler: Sampler::default(),
            stats: Stats::default(),
        };
        // The off-run still needs a clock and inline work in the tests.
        if let Some(c) = off_cfg {
            run.proof.cfg = Some(c);
        }
        run.tick();
        run
    }

    fn tick(&mut self) {
        self.proof
            .tick(Instant::now(), &ids(), &self.stats, &mut self.sampler);
    }

    /// A sample sent to `addr` and answered with `interval`.
    fn sampled(&mut self, addr: SocketAddr, token: u64, interval: Duration) {
        let now = Instant::now();
        assert!(
            self.sampler.reserve(addr, token, now),
            "{addr} must be free"
        );
        assert!(self.sampler.complete(addr, token, interval, now));
    }

    fn commit(mut self, d: &Dirs) {
        let r = self
            .proof
            .commit(&mut self.sampler, &state(), &d.state_path(), &self.stats);
        assert_eq!(r, Some(Ok(())), "the commit must land");
    }
}

fn addr(i: u8) -> SocketAddr {
    SocketAddr::from(([198, 51, 100, i], 6881))
}

#[tokio::test(start_paused = true)]
async fn first_start_suppresses_for_the_horizon() {
    let d = Dirs::new("first");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::FirstStart);
    assert!(!r.proof.may_sample(Instant::now()));
    advance(horizon() - Duration::from_millis(1)).await;
    r.tick();
    assert!(!r.proof.may_sample(Instant::now()));
    assert!(r.proof.suppression_seconds(Instant::now()) >= 1);
    advance(Duration::from_millis(1)).await;
    assert!(r.proof.may_sample(Instant::now()));
    assert_eq!(r.proof.suppression_seconds(Instant::now()), 0);
}

#[tokio::test(start_paused = true)]
async fn a_clean_restart_resumes_and_honours_each_restored_restriction() {
    let d = Dirs::new("resume");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    assert!(r.proof.may_sample(Instant::now()));
    r.sampled(addr(1), 1, Duration::from_secs(600));
    r.sampled(addr(2), 2, Duration::from_secs(3 * 3600));
    r.commit(&d);

    advance(Duration::from_secs(60)).await;
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(
        r.proof.may_sample(Instant::now()),
        "no blanket gate after a full window"
    );
    assert_eq!(
        r.stats.restart_obligations_restored.load(Ordering::Relaxed),
        2
    );
    // Each restored endpoint stays refused until its own deadline.
    let now = Instant::now();
    assert!(!r.sampler.reserve(addr(1), 10, now));
    assert!(!r.sampler.reserve(addr(2), 11, now));
    assert!(
        r.sampler.reserve(addr(3), 12, now),
        "an unrestricted endpoint is free"
    );
    advance(Duration::from_secs(540) - Duration::from_millis(1)).await;
    assert!(!r.sampler.reserve(addr(1), 13, Instant::now()));
    advance(Duration::from_secs(1)).await;
    assert!(
        r.sampler.reserve(addr(1), 14, Instant::now()),
        "free at its deadline"
    );
    assert!(!r.sampler.reserve(addr(2), 15, Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn a_short_suppressed_run_passes_its_window_on_through_the_gate() {
    // N1: A resumes and samples E, then is killed. B suppresses (`dirty`)
    // and stops cleanly 5 minutes later. C, starting 1 minute after that,
    // resumes but sends nothing to anyone before B's start + H.
    let d = Dirs::new("gate");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    let mut a = Run::boot(&d, &c, true);
    assert_eq!(a.proof.reason(), Reason::Resumed);
    a.sampled(addr(9), 1, Duration::from_secs(3600));
    drop(a); // kill -9: no commit

    advance(Duration::from_secs(60)).await;
    let b = Run::boot(&d, &c, true);
    assert_eq!(b.proof.reason(), Reason::Dirty);
    let b_start = Instant::now();
    advance(Duration::from_secs(300)).await;
    b.commit(&d);

    advance(Duration::from_secs(60)).await;
    let mut c_run = Run::boot(&d, &c, true);
    assert_eq!(c_run.proof.reason(), Reason::Resumed);
    assert!(!c_run.proof.may_sample(Instant::now()), "the gate holds");
    let until = b_start + horizon();
    advance(until - Instant::now() - Duration::from_millis(1)).await;
    c_run.tick();
    assert!(!c_run.proof.may_sample(Instant::now()));
    advance(Duration::from_secs(1)).await;
    assert!(c_run.proof.may_sample(Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn the_gate_carries_through_first_start_off_run_generation_and_torn_witness() {
    for case in ["first_start", "off_run", "generation", "torn_witness"] {
        let d = Dirs::new("gatecases");
        let c = clock();
        // Get to the state each case starts from.
        match case {
            "first_start" => {}
            _ => {
                let mut r = Run::boot(&d, &c, true);
                advance(horizon()).await;
                r.tick();
                r.commit(&d);
            }
        }
        match case {
            "off_run" => drop(Run::boot(&d, &c, false)),
            "generation" => {
                // A restore: the witness is ahead of the marker.
                let Found::Ok(m) = restart::read_marker(&d.data) else {
                    panic!()
                };
                let w = restart::witness_path(&d.witness, &m.run.dir_id);
                let mut wit = Witness::decode(&std::fs::read(&w).unwrap()).unwrap();
                wit.generation += 5;
                std::fs::write(&w, wit.encode()).unwrap();
            }
            "torn_witness" => {
                let Found::Ok(m) = restart::read_marker(&d.data) else {
                    panic!()
                };
                let w = restart::witness_path(&d.witness, &m.run.dir_id);
                let mut bytes = std::fs::read(&w).unwrap();
                bytes.truncate(bytes.len() / 2);
                std::fs::write(&w, bytes).unwrap();
            }
            _ => {}
        }
        // A short suppressed run that stops cleanly.
        let b = Run::boot(&d, &c, true);
        let expect = match case {
            "torn_witness" => Reason::NoWitness,
            "first_start" => Reason::FirstStart,
            "off_run" => Reason::OffRun,
            _ => Reason::Generation,
        };
        assert_eq!(b.proof.reason(), expect, "{case}");
        let b_start = Instant::now();
        advance(Duration::from_secs(120)).await;
        b.commit(&d);
        advance(Duration::from_secs(30)).await;
        let mut next = Run::boot(&d, &c, true);
        assert_eq!(next.proof.reason(), Reason::Resumed, "{case}");
        let until = b_start + horizon();
        advance(until - Instant::now() - Duration::from_millis(1)).await;
        next.tick();
        assert!(!next.proof.may_sample(Instant::now()), "{case}: early");
        advance(Duration::from_millis(1)).await;
        assert!(next.proof.may_sample(Instant::now()), "{case}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_busy_lock_is_retried_and_nothing_is_touched_meanwhile() {
    let d = Dirs::new("busy");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    let before = std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap();
    let LockAttempt::Held(other) = restart::try_lock(&d.data).unwrap() else {
        eprintln!("SKIPPED: no file locks in the temp dir");
        return;
    };
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Locked);
    assert!(!r.proof.may_sample(Instant::now()));
    assert!(r.proof.suppression_seconds(Instant::now()) >= 1);
    for _ in 0..10 {
        advance(TICK).await;
        r.tick();
    }
    assert_eq!(
        std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap(),
        before,
        "a locked-out run writes nothing"
    );
    drop(other);
    // The retry backs off to at most a minute.
    advance(MAX_RETRY).await;
    r.tick();
    assert_eq!(
        r.proof.reason(),
        Reason::Resumed,
        "nothing was sent: it may resume"
    );
}

#[tokio::test(start_paused = true)]
async fn an_off_run_holds_the_lock_and_its_window_starts_when_it_gets_it() {
    // C1: an off-run locked out for longer than H sends nothing until H
    // after it takes the lock. N2: an on-run beside it is locked out.
    let d = Dirs::new("c1");
    let c = clock();
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        eprintln!("SKIPPED: no file locks in the temp dir");
        return;
    };
    let mut off = Run::boot(&d, &c, false);
    advance(horizon() + Duration::from_secs(3600)).await;
    off.tick();
    assert!(!off.proof.may_sample(Instant::now()), "locked out: nothing");
    drop(holder);
    advance(MAX_RETRY).await;
    off.tick();
    let took = Instant::now();
    assert!(
        !off.proof.may_sample(took),
        "the window starts now, not at the epoch"
    );
    let on = Run::boot(&d, &c, true);
    assert_eq!(
        on.proof.reason(),
        Reason::Locked,
        "an on-run beside it waits"
    );
    assert!(!on.proof.may_sample(Instant::now()));
    advance(horizon()).await;
    assert!(off.proof.may_sample(Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn failed_invalidation_blocks_sampling_until_a_retry_lands() {
    // I1: a clean marker exists and neither the marker nor the witness can
    // be written. Nothing is sampled; once a retry lands, the run samples
    // after its window.
    let d = Dirs::new("i1");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    fail::arm_sticky(Some(restart::Point::AfterWrite));
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::InvalidateFailed);
    advance(horizon() + Duration::from_secs(60)).await;
    r.tick();
    assert!(!r.proof.may_sample(Instant::now()));
    assert!(
        r.proof.suppression_seconds(Instant::now()) >= 1,
        "never reads 0 while blocked"
    );
    assert!(
        r.stats.restart_persist_errors[step(PersistStep::Invalidate)].load(Ordering::Relaxed) >= 2
    );
    fail::arm_sticky(None);
    advance(MAX_RETRY).await;
    r.tick(); // starts the retry
    r.tick(); // reads its result
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(r.proof.may_sample(Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn an_off_run_between_two_on_runs_reads_off_run() {
    let d = Dirs::new("offrun");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    drop(Run::boot(&d, &c, false));
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::OffRun);
    drop(r); // releases the lock
    // A fixture that overrides the delay is an off-run too (I5).
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    drop(Run::boot_with(&d, &c, true, Some(Duration::ZERO)));
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::OffRun);
}

#[tokio::test(start_paused = true)]
async fn in_flight_samples_are_sealed_for_the_full_horizon() {
    // P6: a sample in flight at shutdown is restored for now + H, and a
    // late reply under the old token can't shorten it.
    let d = Dirs::new("p6");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    assert!(r.sampler.reserve(addr(4), 77, Instant::now()));
    let sealed_at = Instant::now();
    let Run {
        mut proof,
        mut sampler,
        stats,
    } = r;
    proof
        .commit(&mut sampler, &state(), &d.state_path(), &stats)
        .unwrap()
        .unwrap();
    assert!(!sampler.complete(addr(4), 77, Duration::ZERO, Instant::now()));
    let mut r = Run::boot(&d, &c, true);
    advance(sealed_at + horizon() - Instant::now() - Duration::from_secs(1)).await;
    r.tick();
    assert!(!r.sampler.reserve(addr(4), 1, Instant::now()));
    advance(Duration::from_secs(2)).await;
    assert!(r.sampler.reserve(addr(4), 2, Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn the_marker_carries_the_ids_saved_at_shutdown() {
    // P2: BEP 42 rotated the ids mid-run; dht-state.bin is saved with the
    // new ones, and the next start (which loads them) resumes.
    let d = Dirs::new("p2");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    let rotated = State {
        ids: vec![NodeId([9; 20])],
        ..state()
    };
    let Run {
        mut proof,
        mut sampler,
        stats,
    } = r;
    proof
        .commit(&mut sampler, &rotated, &d.state_path(), &stats)
        .unwrap()
        .unwrap();
    let mut next = Run::boot(&d, &c, true);
    // `boot` presents the old ids: those no longer match.
    assert_eq!(next.proof.reason(), Reason::NodeIdentity);
    drop(next);
    // With the rotated ids, as the actor would load them, it resumes.
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    let Run {
        mut proof,
        mut sampler,
        stats,
    } = r;
    proof
        .commit(&mut sampler, &rotated, &d.state_path(), &stats)
        .unwrap()
        .unwrap();
    next = Run {
        proof: Proof::new(
            Some(&d.state_path()),
            Some(&RestartConfig {
                witness_dir: Some(d.witness.clone()),
                clock: c.clone(),
                inline_io: true,
            }),
            None,
            horizon(),
            TICK,
            Instant::now(),
        ),
        sampler: Sampler::default(),
        stats: Stats::default(),
    };
    next.proof.tick(
        Instant::now(),
        &[NodeId([9; 20])],
        &next.stats,
        &mut next.sampler,
    );
    assert_eq!(next.proof.reason(), Reason::Resumed);
}

#[tokio::test(start_paused = true)]
async fn legacy_runs_without_a_data_dir_keep_todays_suppression() {
    let p = Proof::new(None, None, None, horizon(), TICK, Instant::now());
    assert!(!p.may_sample(Instant::now()));
    advance(horizon()).await;
    assert!(p.may_sample(Instant::now()));
    let p = Proof::new(
        None,
        None,
        Some(Duration::ZERO),
        horizon(),
        TICK,
        Instant::now(),
    );
    assert!(p.may_sample(Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn no_state_save_before_s1_hashes_it() {
    let d = Dirs::new("save");
    let c = clock();
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        eprintln!("SKIPPED: no file locks in the temp dir");
        return;
    };
    let mut r = Run::boot(&d, &c, true);
    assert!(
        !r.proof.may_save_state(),
        "locked out: dht-state.bin is the holder's"
    );
    drop(holder);
    advance(MAX_RETRY).await;
    r.tick();
    assert!(r.proof.may_save_state());
}

#[tokio::test(start_paused = true)]
async fn a_full_table_commits_within_the_budget() {
    // P4: 200,000 restrictions, the most a table holds. The ceiling is
    // loose so a slow runner can't flake it; the time is printed.
    let d = Dirs::new("budget");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    let now = Instant::now();
    for i in 0..restart::MAX_ENTRIES as u32 {
        let a = SocketAddr::from((i.to_be_bytes(), 6881));
        assert!(r.sampler.reserve(a, u64::from(i) + 1, now));
        assert!(
            r.sampler
                .complete(a, u64::from(i) + 1, Duration::from_secs(3600), now)
        );
    }
    let wall = std::time::Instant::now();
    r.commit(&d);
    let took = wall.elapsed();
    eprintln!("commit of {} restrictions: {took:?}", restart::MAX_ENTRIES);
    assert!(took < Duration::from_secs(8), "{took:?}");
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert_eq!(
        r.stats.restart_obligations_restored.load(Ordering::Relaxed),
        restart::MAX_ENTRIES as u64
    );
}
