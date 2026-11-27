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
    /// A run whose lock nobody holds on purpose: see [`Run::take_lock`].
    fn boot(d: &Dirs, clock: &Arc<SimClock>, on: bool) -> Run {
        let mut run = Run::boot_with(d, clock, on, None);
        run.take_lock();
        run
    }

    /// A run that may find the lock held, and must say so.
    fn boot_locked(d: &Dirs, clock: &Arc<SimClock>, on: bool) -> Run {
        Run::boot_with(d, clock, on, None)
    }

    /// Retries the lock at the same simulated instant until it's free, with
    /// a 10 s real-time deadline. On Unix a child process another test in
    /// this binary forks (the abort test) shares the lock file for a moment
    /// before its `exec` closes it; in production that costs one tick.
    fn take_lock(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while matches!(self.proof.phase, Phase::Locking { .. })
            && std::time::Instant::now() < deadline
        {
            self.proof.phase = Phase::Locking {
                next: Instant::now(),
                backoff: TICK,
            };
            self.tick();
            std::thread::yield_now();
        }
    }

    fn boot_with(d: &Dirs, clock: &Arc<SimClock>, on: bool, delay: Option<Duration>) -> Run {
        Run::boot_into(d, clock, on, delay, Sampler::default())
    }

    /// An on-run whose sampler is `sampler` from the start, so a resume
    /// seeds into it.
    fn boot_sampler(d: &Dirs, clock: &Arc<SimClock>, sampler: Sampler) -> Run {
        let mut run = Run::boot_into(d, clock, true, None, sampler);
        run.take_lock();
        run
    }

    fn boot_into(
        d: &Dirs,
        clock: &Arc<SimClock>,
        on: bool,
        delay: Option<Duration>,
        sampler: Sampler,
    ) -> Run {
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
            sampler,
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
        // As the actor does on every sample it reserves.
        self.proof.note_sampled();
    }

    fn commit(mut self, d: &Dirs) {
        let r = self
            .proof
            .commit(&mut self.sampler, &state(), &d.state_path(), &self.stats);
        assert_eq!(r, Some(Ok(())), "the commit must land");
    }
}

/// Enough retry rounds, after a fault is disarmed, for one to land: a
/// failure already queued is read first, then the next retry runs.
async fn settle(r: &mut Run) {
    for _ in 0..3 {
        advance(MAX_RETRY).await;
        r.tick();
        r.tick();
    }
}

fn addr(i: u8) -> SocketAddr {
    SocketAddr::from(([198, 51, 100, i], 6881))
}

#[tokio::test(start_paused = true)]
async fn a_first_start_samples_at_once() {
    // Owner §2.77: no start waits. The reason stays as a diagnostic.
    let d = Dirs::new("first");
    let c = clock();
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::FirstStart);
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
async fn no_gate_carries_over_an_unproven_run() {
    // CR-0w (it was N1's gate): A resumes and samples E, then is killed.
    // B can't resume (`dirty`): it restores nothing, samples at once and
    // stops cleanly 5 minutes later. C resumes B's commit and samples at
    // once too: no gate is written or restored, only B's own restrictions.
    let d = Dirs::new("gate");
    let c = clock();
    let r = Run::boot(&d, &c, true);
    r.commit(&d);
    let mut a = Run::boot(&d, &c, true);
    assert_eq!(a.proof.reason(), Reason::Resumed);
    a.sampled(addr(9), 1, Duration::from_secs(3600));
    drop(a); // kill -9: no commit

    advance(Duration::from_secs(60)).await;
    let mut b = Run::boot(&d, &c, true);
    assert_eq!(b.proof.reason(), Reason::Dirty);
    assert!(b.proof.may_sample(Instant::now()), "samples at once");
    assert_eq!(
        b.stats.restart_obligations_restored.load(Ordering::Relaxed),
        0,
        "restores nothing"
    );
    b.sampled(addr(9), 2, Duration::from_secs(600));
    advance(Duration::from_secs(300)).await;
    b.commit(&d);
    let (s, _) =
        restart::Snapshot::decode(&std::fs::read(d.data.join(restart::SNAPSHOT_FILE)).unwrap())
            .unwrap();
    assert_eq!(s.gate_s, 0, "no gate is written");

    advance(Duration::from_secs(60)).await;
    let c_run = Run::boot(&d, &c, true);
    assert_eq!(c_run.proof.reason(), Reason::Resumed);
    assert!(c_run.proof.may_sample(Instant::now()), "no gate");
    assert_eq!(c_run.proof.suppression_seconds(Instant::now()), 0);
    let mut c_run = c_run;
    assert!(
        !c_run.sampler.reserve(addr(9), 3, Instant::now()),
        "B's own restriction holds"
    );
    assert!(c_run.sampler.reserve(addr(1), 4, Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn no_gate_after_a_first_start_off_run_generation_or_torn_witness() {
    for case in ["first_start", "off_run", "generation", "torn_witness"] {
        let d = Dirs::new("gatecases");
        let c = clock();
        // Get to the state each case starts from.
        match case {
            "first_start" => {}
            _ => Run::boot(&d, &c, true).commit(&d),
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
        // A short unproven run that samples at once and stops cleanly.
        let mut b = Run::boot(&d, &c, true);
        let expect = match case {
            "torn_witness" => Reason::NoWitness,
            "first_start" => Reason::FirstStart,
            "off_run" => Reason::OffRun,
            _ => Reason::Generation,
        };
        assert_eq!(b.proof.reason(), expect, "{case}");
        assert!(b.proof.may_sample(Instant::now()), "{case}: at once");
        b.sampled(addr(3), 1, Duration::from_secs(1800));
        advance(Duration::from_secs(120)).await;
        b.commit(&d);
        advance(Duration::from_secs(30)).await;
        let mut next = Run::boot(&d, &c, true);
        assert_eq!(next.proof.reason(), Reason::Resumed, "{case}");
        assert!(next.proof.may_sample(Instant::now()), "{case}: no gate");
        assert!(
            !next.sampler.reserve(addr(3), 2, Instant::now()),
            "{case}: the unproven run's own restriction carries over"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_busy_lock_is_retried_and_nothing_is_touched_meanwhile() {
    // CR-0w: a locked-out on-run holds samples for at most START_HOLD,
    // then samples, still locked out and still writing nothing.
    let d = Dirs::new("busy");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    let before = std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap();
    let LockAttempt::Held(other) = restart::try_lock(&d.data).unwrap() else {
        panic!("this test needs file locks in the temp dir");
    };
    let start = Instant::now();
    let mut r = Run::boot_locked(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Locked);
    assert!(!r.proof.may_sample(Instant::now()));
    let left = r.proof.suppression_seconds(Instant::now());
    assert!((1..=START_HOLD.as_secs()).contains(&left), "{left}");
    advance(START_HOLD - Duration::from_millis(1)).await;
    r.tick();
    assert!(!r.proof.may_sample(Instant::now()));
    advance(Duration::from_millis(1)).await;
    r.tick();
    assert_eq!(Instant::now(), start + START_HOLD);
    assert!(r.proof.may_sample(Instant::now()), "the hold is bounded");
    assert_eq!(r.proof.suppression_seconds(Instant::now()), 0);
    assert_eq!(r.proof.reason(), Reason::Locked);
    for _ in 0..10 {
        advance(TICK).await;
        r.tick();
    }
    assert_eq!(
        std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap(),
        before,
        "a locked-out run writes nothing"
    );
    // It has been sampling: a node in flight, another cooling.
    let now = Instant::now();
    assert!(r.sampler.reserve(addr(1), 1, now));
    r.proof.note_sampled();
    r.sampled(addr(2), 2, Duration::from_secs(60));
    drop(other);
    // The retry backs off to at most a minute.
    advance(MAX_RETRY).await;
    r.tick();
    r.take_lock();
    // The proof would resume, but the run already sampled: it restores
    // nothing (seeding is only ever into a sampler that hasn't sampled),
    // and reports `locked`.
    assert_eq!(r.proof.reason(), Reason::Locked, "landed past the hold");
    assert_eq!(
        r.stats.restart_obligations_restored.load(Ordering::Relaxed),
        0
    );
    assert!(r.proof.may_sample(Instant::now()));
    // Its own records are untouched: the in-flight sample completes.
    assert!(
        r.sampler
            .complete(addr(1), 1, Duration::from_secs(600), Instant::now())
    );
    // It invalidated, so it commits its own table.
    r.commit(&d);
    let next = Run::boot(&d, &c, true);
    assert_eq!(next.proof.reason(), Reason::Resumed);
    let mut next = next;
    assert!(!next.sampler.reserve(addr(1), 3, Instant::now()), "carried");
}

#[tokio::test(start_paused = true)]
async fn an_off_run_samples_at_once_and_holds_the_lock_once_it_gets_it() {
    // CR-0w (it was C1's window): an off-run samples at once, locked out
    // or not. N2: an on-run beside it is locked out.
    let d = Dirs::new("c1");
    let c = clock();
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        panic!("this test needs file locks in the temp dir");
    };
    let mut off = Run::boot_locked(&d, &c, false);
    assert!(off.proof.may_sample(Instant::now()), "locked out, at once");
    drop(holder);
    advance(MAX_RETRY).await;
    off.tick();
    off.take_lock();
    assert!(off.proof.may_sample(Instant::now()));
    let on = Run::boot_locked(&d, &c, true);
    assert_eq!(
        on.proof.reason(),
        Reason::Locked,
        "an on-run beside it is locked out"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_invalidation_samples_at_once_and_commits_once_a_retry_lands() {
    // CR-0w (it was I1's wait): a clean marker exists and neither the
    // marker nor the witness can be written. The run samples at once with
    // what it restored; it can't commit until a retry lands. A late
    // landing adds no wait, and the next start resumes with no gate.
    let d = Dirs::new("i1");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampled(addr(1), 1, Duration::from_secs(3600));
    r.commit(&d);
    fail::arm_sticky(Some(restart::Point::AfterWrite));
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::InvalidateFailed);
    assert!(r.proof.may_sample(Instant::now()), "at once");
    assert_eq!(r.proof.suppression_seconds(Instant::now()), 0);
    assert!(
        !r.sampler.reserve(addr(1), 2, Instant::now()),
        "what it restored holds"
    );
    for _ in 0..4 {
        advance(Duration::from_secs(30)).await;
        r.tick();
    }
    assert!(
        r.stats.restart_persist_errors[step(PersistStep::Invalidate)].load(Ordering::Relaxed) >= 2
    );
    r.sampled(addr(2), 3, Duration::from_secs(1800));
    fail::arm_sticky(None);
    settle(&mut r).await;
    assert_eq!(r.proof.reason(), Reason::Resumed, "landed late: no wait");
    assert!(r.proof.may_sample(Instant::now()));
    advance(Duration::from_secs(60)).await;
    r.commit(&d);
    let mut next = Run::boot(&d, &c, true);
    assert_eq!(next.proof.reason(), Reason::Resumed);
    assert!(next.proof.may_sample(Instant::now()), "no gate");
    assert!(!next.sampler.reserve(addr(2), 4, Instant::now()), "carried");
}

#[tokio::test(start_paused = true)]
async fn a_run_that_never_invalidates_samples_but_never_commits() {
    // The marker can't be written all run: the run samples, and at
    // shutdown it doesn't commit. A later start may then resume from the
    // older snapshot without this run's restrictions: re-asks, never a
    // wait (`docs/decisions/2026-11-27-CR-0w.md`).
    let d = Dirs::new("i1never");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    fail::arm_sticky(Some(restart::Point::AfterWrite));
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::InvalidateFailed);
    assert!(r.proof.may_sample(Instant::now()));
    r.sampled(addr(4), 1, Duration::from_secs(1800));
    let got = r
        .proof
        .commit(&mut r.sampler, &state(), &d.state_path(), &r.stats);
    fail::arm_sticky(None);
    assert_eq!(got, None, "never invalidated: no commit");
}

#[tokio::test(start_paused = true)]
async fn a_failed_commit_step_leaves_the_next_start_dirty() {
    // S9, S10, S11: whichever write fails, the marker stays dirty, the
    // step is counted, and the next start suppresses.
    for (nth, failed) in [
        (1, PersistStep::StateBin),
        (2, PersistStep::Snapshot),
        (3, PersistStep::Marker),
    ] {
        let d = Dirs::new("commitfail");
        let c = clock();
        let mut r = Run::boot(&d, &c, true);
        advance(horizon()).await;
        r.tick();
        r.sampled(addr(5), 1, Duration::from_secs(600));
        fail::arm_nth(restart::Point::AfterSync, nth);
        let Run {
            mut proof,
            mut sampler,
            stats,
        } = r;
        let got = proof.commit(&mut sampler, &state(), &d.state_path(), &stats);
        fail::arm(None);
        assert_eq!(got, Some(Err(failed)), "{failed:?}");
        assert_eq!(
            stats.restart_persist_errors[step(failed)].load(Ordering::Relaxed),
            1,
            "{failed:?}"
        );
        drop(proof);
        let next = Run::boot(&d, &c, true);
        assert_eq!(next.proof.reason(), Reason::Dirty, "{failed:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_witness_with_a_landed_marker_resumes_and_is_retried_at_commit() {
    // §6.8: S3 fails, S4 lands: invalidated, so the run resumes (not
    // `invalidate_failed`); the commit bumps the witness again, so the
    // next start resumes too instead of reading `generation`.
    let d = Dirs::new("s3");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    fail::arm(Some(restart::Point::AfterWrite)); // the witness, written first
    let r = Run::boot(&d, &c, true);
    fail::arm(None);
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(r.proof.may_sample(Instant::now()));
    assert_eq!(
        r.stats.restart_persist_errors[step(PersistStep::Witness)].load(Ordering::Relaxed),
        1
    );
    r.commit(&d);
    let next = Run::boot(&d, &c, true);
    assert_eq!(next.proof.reason(), Reason::Resumed);
}

#[tokio::test(start_paused = true)]
async fn an_off_run_that_cant_overwrite_the_marker_samples_and_retries() {
    // §6.13, CR-0w: an off-run with a clean marker it can't overwrite
    // samples at once and retries the overwrite until it lands.
    let d = Dirs::new("offfail");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    fail::arm_sticky(Some(restart::Point::AfterWrite));
    let mut off = Run::boot(&d, &c, false);
    assert_eq!(off.proof.reason(), Reason::InvalidateFailed);
    assert!(off.proof.may_sample(Instant::now()));
    fail::arm_sticky(None);
    settle(&mut off).await;
    assert_eq!(off.proof.reason(), Reason::Off);
    let Found::Ok(m) = restart::read_marker(&d.data) else {
        panic!()
    };
    assert_eq!(m.state, MarkerState::Off);
}

#[tokio::test(start_paused = true)]
async fn an_on_run_holding_the_lock_keeps_an_off_run_out() {
    // §6.7 / N2 with a marker present: the off-run beside a live on-run
    // reports `locked` and writes nothing. It samples (CR-0w).
    let d = Dirs::new("onoff");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    let on = Run::boot(&d, &c, true);
    assert_eq!(on.proof.reason(), Reason::Resumed);
    let marker = std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap();
    let mut off = Run::boot_locked(&d, &c, false);
    assert_eq!(off.proof.reason(), Reason::Locked);
    assert!(off.proof.may_sample(Instant::now()));
    advance(horizon() * 2).await;
    off.tick();
    assert_eq!(
        std::fs::read(d.data.join(restart::MARKER_FILE)).unwrap(),
        marker
    );
    drop(on);
}

#[tokio::test(start_paused = true)]
async fn overlapping_runs_never_both_resume() {
    // §6.7: a new run starting while the old one is still up (an
    // in-process restart whose old actor hasn't finished) waits for the
    // lock, holding samples for at most START_HOLD, then resumes from the
    // old run's commit.
    let d = Dirs::new("overlap");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    let mut old = Run::boot(&d, &c, true);
    assert_eq!(old.proof.reason(), Reason::Resumed);
    old.sampled(addr(6), 1, Duration::from_secs(1800));
    let mut new = Run::boot_locked(&d, &c, true);
    assert_eq!(
        new.proof.reason(),
        Reason::Locked,
        "the old run still holds it"
    );
    assert!(!new.proof.may_sample(Instant::now()));
    advance(Duration::from_secs(5)).await;
    old.commit(&d);
    // Within START_HOLD: nothing sampled yet, so it resumes.
    new.take_lock();
    assert!(Instant::now() < new.proof.epoch + START_HOLD);
    assert_eq!(new.proof.reason(), Reason::Resumed);
    assert!(
        !new.sampler.reserve(addr(6), 2, Instant::now()),
        "the old run's restriction holds"
    );
}

#[tokio::test(start_paused = true)]
async fn reload_send_crash_reload_never_resumes_past_a_crash() {
    // §6.2: 100 restarts, each stopping cleanly or crashing at a random
    // point. A crash is always followed by a start that restores nothing;
    // a clean stop by a resume that still holds the last run's
    // restrictions. Every start samples at once (CR-0w).
    use rand::{RngExt, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xC0DE);
    let d = Dirs::new("loop");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    let mut last_clean = true;
    let mut held: Vec<(SocketAddr, Instant)> = Vec::new();
    for i in 0..100u32 {
        let mut r = Run::boot(&d, &c, true);
        if last_clean {
            assert_eq!(r.proof.reason(), Reason::Resumed, "iteration {i}");
            let now = Instant::now();
            for (a, until) in &held {
                if *until > now {
                    assert!(
                        !r.sampler.reserve(*a, 1 << 40, now),
                        "iteration {i}: {a} free early"
                    );
                }
            }
        } else {
            assert_eq!(r.proof.reason(), Reason::Dirty, "iteration {i}");
        }
        // No window or gate; only the crash-loop guard, after a run that
        // lived under a minute, and never longer than that minute.
        let wait = r.proof.suppression_seconds(Instant::now());
        assert!(
            wait == 0 || (!last_clean && wait <= CRASH_LOOP.as_secs()),
            "iteration {i}: {wait} s"
        );
        advance(Duration::from_secs(wait)).await;
        r.tick();
        assert!(r.proof.may_sample(Instant::now()));
        let mut sampled = Vec::new();
        for k in 0..2u32 {
            let a = SocketAddr::from(([10, (i % 250) as u8, k as u8, 1], 6881));
            let wait = Duration::from_secs(rng.random_range(60..7200));
            r.sampled(a, u64::from(i * 10 + k + 1), wait);
            sampled.push((a, Instant::now() + wait));
        }
        advance(Duration::from_secs(rng.random_range(1..600))).await;
        last_clean = rng.random_bool(0.5);
        if last_clean {
            r.commit(&d);
            held = sampled;
        } else {
            drop(r); // kill -9 at a random point in the run
        }
        advance(Duration::from_secs(rng.random_range(1..120))).await;
    }
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
    // A fixture's start delay no longer makes an off-run (CR-0w drops
    // I5's coupling): the run is on, resumes, and holds for its delay.
    Run::boot(&d, &c, true).commit(&d);
    let mut delayed = Run::boot_with(&d, &c, true, Some(Duration::from_secs(30)));
    delayed.take_lock();
    assert_eq!(delayed.proof.reason(), Reason::Resumed);
    assert!(!delayed.proof.may_sample(Instant::now()), "its own delay");
    advance(Duration::from_secs(30)).await;
    assert!(delayed.proof.may_sample(Instant::now()));
    delayed.commit(&d);
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Resumed);
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
async fn a_scaled_cooldown_is_saved_as_the_horizon() {
    // WK-10e: a 48 h cooldown (scale 8) doesn't fail the snapshot. It's
    // saved and restored as `MAX_INTERVAL` from the save, so every node's
    // own interval (at most that) still holds after the restart.
    let d = Dirs::new("wk10e");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    let max = super::super::sampler::MAX_INTERVAL;
    assert!(r.sampler.reserve(addr(5), 9, Instant::now()));
    assert!(
        r.sampler
            .complete_scaled(addr(5), 9, max, 8, Instant::now())
    );
    let saved_at = Instant::now();
    let Run {
        mut proof,
        mut sampler,
        stats,
    } = r;
    proof
        .commit(&mut sampler, &state(), &d.state_path(), &stats)
        .unwrap()
        .unwrap();
    let mut r = Run::boot(&d, &c, true);
    advance(saved_at + max - Instant::now() - Duration::from_secs(1)).await;
    r.tick();
    assert_eq!(r.proof.reason(), Reason::Resumed, "the snapshot loaded");
    assert!(!r.sampler.reserve(addr(5), 1, Instant::now()));
    advance(Duration::from_secs(2)).await;
    assert!(r.sampler.reserve(addr(5), 2, Instant::now()));
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
async fn legacy_runs_sample_at_once_unless_a_fixture_delays_them() {
    let p = Proof::new(None, None, None, horizon(), TICK, Instant::now());
    assert!(p.may_sample(Instant::now()));
    assert_eq!(p.suppression_seconds(Instant::now()), 0);
    let delay = Duration::from_secs(90);
    let p = Proof::new(None, None, Some(delay), horizon(), TICK, Instant::now());
    assert!(!p.may_sample(Instant::now()));
    assert_eq!(p.suppression_seconds(Instant::now()), 90);
    advance(delay).await;
    assert!(p.may_sample(Instant::now()));
}

#[tokio::test(start_paused = true)]
async fn no_state_save_before_s1_hashes_it() {
    let d = Dirs::new("save");
    let c = clock();
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        panic!("this test needs file locks in the temp dir");
    };
    let mut r = Run::boot_locked(&d, &c, true);
    assert!(
        !r.proof.may_save_state(),
        "locked out: dht-state.bin is the holder's"
    );
    drop(holder);
    advance(MAX_RETRY).await;
    r.tick();
    r.take_lock();
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
        // One address per /24, inside every block's share.
        let a = SocketAddr::from(((i << 8).to_be_bytes(), 6881));
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

/// A marker that exists but can't be read or overwritten: a directory in
/// its place (EISDIR on both, even for root, where `chmod 000` wouldn't
/// stop a read). The code can't tell it from a clean marker it can't read.
fn make_marker_unreadable(d: &Dirs) {
    let m = d.data.join(restart::MARKER_FILE);
    let _ = std::fs::remove_file(&m);
    std::fs::create_dir(&m).unwrap();
}

fn make_marker_readable(d: &Dirs) {
    std::fs::remove_dir(d.data.join(restart::MARKER_FILE)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_marker_must_be_overwritten_before_a_commit() {
    // Runner review B1: an unreadable marker may be a clean one, so an
    // on-run doesn't count as invalidated, even with its own (new id's)
    // witness written, until the marker itself is overwritten. It samples
    // meanwhile (CR-0w), restoring nothing.
    let d = Dirs::new("unreadable");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    make_marker_unreadable(&d);
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::InvalidateFailed);
    assert!(r.proof.may_sample(Instant::now()));
    advance(Duration::from_secs(60)).await;
    r.tick();
    assert_eq!(
        r.proof.reason(),
        Reason::InvalidateFailed,
        "the witness bump doesn't count"
    );
    assert!(!r.proof.started.as_ref().unwrap().invalidated);
    make_marker_readable(&d);
    settle(&mut r).await;
    assert_eq!(
        r.proof.reason(),
        Reason::Corrupt,
        "landed: the start's own reason"
    );
    let Found::Ok(m) = restart::read_marker(&d.data) else {
        panic!()
    };
    assert_eq!(m.state, MarkerState::Dirty);
    r.commit(&d);
}

#[tokio::test(start_paused = true)]
async fn an_off_run_overwrites_an_unreadable_marker_too() {
    let d = Dirs::new("unreadableoff");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    make_marker_unreadable(&d);
    let mut off = Run::boot(&d, &c, false);
    assert_eq!(off.proof.reason(), Reason::InvalidateFailed);
    assert!(off.proof.may_sample(Instant::now()));
    make_marker_readable(&d);
    settle(&mut off).await;
    assert_eq!(off.proof.reason(), Reason::Off);
    let Found::Ok(m) = restart::read_marker(&d.data) else {
        panic!()
    };
    assert_eq!(m.state, MarkerState::Off);
}

#[tokio::test(start_paused = true)]
async fn a_copied_directory_needs_its_marker_overwritten() {
    // N7: the witness for this id names another path, so the run takes a
    // new `dir_id` and its witness bump goes to another file. Only the
    // marker write invalidates the old proof.
    let d = Dirs::new("copied");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    advance(horizon()).await;
    r.tick();
    r.commit(&d);
    let Found::Ok(m) = restart::read_marker(&d.data) else {
        panic!()
    };
    let w = restart::witness_path(&d.witness, &m.run.dir_id);
    let mut wit = Witness::decode(&std::fs::read(&w).unwrap()).unwrap();
    wit.path = "/somewhere/else".into();
    std::fs::write(&w, wit.encode()).unwrap();
    // The witness file (written first) lands; the marker can't be written.
    fail::arm_nth(restart::Point::AfterWrite, 2);
    let r = Run::boot(&d, &c, true);
    fail::arm(None);
    assert_eq!(r.proof.reason(), Reason::InvalidateFailed);
    assert!(!r.proof.started.as_ref().unwrap().invalidated);
    assert!(r.proof.may_sample(Instant::now()), "it samples meanwhile");
}

/// On a real clock, waits (deadline 10 s) until the wall clock is past the
/// marker's time. S11 rounds that up to the millisecond and S2 rounds its
/// own reading down, so a restart within the commit's millisecond reads
/// `clock_backward` and suppresses, which is safe and no real restart is
/// that fast; a test that restarts at once must step past it.
async fn past_the_marker(d: &Dirs, c: &Arc<SimClock>) {
    let Found::Ok(m) = restart::read_marker(&d.data) else {
        panic!("a marker")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while restart::floor_ms(c.wall()) <= m.at_ms && std::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert!(restart::floor_ms(c.wall()) > m.at_ms);
}

/// Real time, real blocking pool: the paths `inline_io` skips.
fn pool_config(d: &Dirs, c: &Arc<SimClock>) -> RestartConfig {
    RestartConfig {
        witness_dir: Some(d.witness.clone()),
        clock: c.clone(),
        inline_io: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_job_landing_in_the_shutdown_drain_is_seeded_before_the_capture() {
    // Runner review B2: the run stops right after its first tick, while
    // S0–S4 run on the pool. The commit drains the job, seeds what it
    // restored, and only then captures the table.
    // CR-0w: also for a run older than START_HOLD that never sampled: the
    // drain still seeds what it restored.
    for age in [Duration::ZERO, Duration::from_secs(30)] {
        let d = Dirs::new("drain");
        let c = clock();
        let mut r = Run::boot(&d, &c, true);
        r.sampled(addr(7), 1, Duration::from_secs(3600));
        r.commit(&d);
        past_the_marker(&d, &c).await;
        let cfg = pool_config(&d, &c);
        let path = d.state_path();
        let mut run = Run {
            proof: Proof::new(
                Some(&path),
                Some(&cfg),
                None,
                horizon(),
                TICK,
                Instant::now()
                    .checked_sub(age)
                    .expect("the clock is past 30 s"),
            ),
            sampler: Sampler::default(),
            stats: Stats::default(),
        };
        // Hold the start job on the pool until the shutdown is under way.
        let (release, held) = std::sync::mpsc::channel();
        *HOLD_START.lock().unwrap() = Some((d.data.clone(), held));
        // Takes the lock and spawns S0–S4 on the pool, retrying only if a
        // forked child briefly shares the lock file (see `take_lock`).
        run.tick();
        run.take_lock();
        let Run {
            mut proof,
            mut sampler,
            stats,
        } = run;
        assert!(
            proof.job.is_some(),
            "the start job is still out: the drain is what lands it"
        );
        assert!(proof.started.is_none());
        release.send(()).unwrap();
        assert_eq!(
            proof.commit(&mut sampler, &state(), &path, &stats),
            Some(Ok(())),
            "the drained start job resumed, so this run commits"
        );
        let Found::Ok((snap, _)) = restart::read_snapshot(&d.data) else {
            panic!("a snapshot was written")
        };
        assert!(
            snap.entries.iter().any(|e| e.addr == addr(7)),
            "the restored restriction survives into the new snapshot"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_blocking_pool_path_reaches_a_resume() {
    let d = Dirs::new("pool");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampled(addr(8), 1, Duration::from_secs(3600));
    r.commit(&d);
    past_the_marker(&d, &c).await;
    let cfg = pool_config(&d, &c);
    let path = d.state_path();
    let mut run = Run {
        proof: Proof::new(
            Some(&path),
            Some(&cfg),
            None,
            horizon(),
            TICK,
            Instant::now(),
        ),
        sampler: Sampler::default(),
        stats: Stats::default(),
    };
    // Wait for the job, polling with a deadline (no fixed sleep).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !matches!(run.proof.phase, Phase::Ready) && std::time::Instant::now() < deadline {
        run.tick();
        tokio::task::yield_now().await;
    }
    assert!(
        matches!(run.proof.phase, Phase::Ready),
        "the pool job landed"
    );
    assert_eq!(run.proof.reason(), Reason::Resumed);
    assert!(!run.sampler.reserve(addr(8), 2, Instant::now()), "restored");
    run.commit(&d);
}

/// X-split-resume-gate: a cooldown in the spill filter goes in the snapshot
/// with the filter, so the next run samples everyone else at once and
/// still refuses that node until its own end (never sooner, and within a
/// couple of minutes after). Whether the next run has the split on or off,
/// and whether anything spilled, no gate is added.
#[tokio::test(start_paused = true)]
async fn a_spilled_cooldown_is_restored_and_the_next_run_samples_at_once() {
    for (spill, split_next) in [(true, true), (true, false), (false, true)] {
        let d = Dirs::new("spill");
        let c = clock();
        let mut r = Run::boot(&d, &c, true);
        advance(horizon()).await;
        r.tick();
        r.sampler = Sampler::split_with_capacity(if spill { 1 } else { 2 });
        let t0 = Instant::now();
        r.sampled(addr(1), 1, Duration::from_secs(3600));
        // With one place, this moves addr(1)'s cooldown to the filter.
        r.sampled(addr(2), 2, Duration::from_secs(600));
        assert_eq!(r.sampler.spill_counts().0, usize::from(spill));
        r.commit(&d);
        let bytes = std::fs::read(d.data.join(restart::SNAPSHOT_FILE)).unwrap();
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(
            version,
            if spill {
                restart::SNAPSHOT_SPILL_VERSION
            } else {
                restart::VERSION
            },
            "version 1 unless something spilled"
        );

        advance(Duration::from_secs(60)).await;
        let mut r = Run::boot_sampler(&d, &c, Sampler::new(split_next));
        assert_eq!(r.proof.reason(), Reason::Resumed);
        assert!(r.proof.may_sample(Instant::now()), "no gate");
        assert_eq!(r.proof.suppression_seconds(Instant::now()), 0);
        // The listed restriction, as before.
        assert!(!r.sampler.reserve(addr(2), 10, Instant::now()), "restored");
        assert!(
            r.sampler.reserve(addr(3), 11, Instant::now()),
            "others are free"
        );
        assert_eq!(r.sampler.spill_counts().0, usize::from(spill));
        if !spill {
            continue;
        }
        // The spilled one: refused until its own end, then free within
        // two minutes (the filter's minute and the snapshot's).
        let end = t0 + Duration::from_secs(3600);
        advance(end - Instant::now() - Duration::from_millis(1)).await;
        r.tick();
        assert!(
            !r.sampler.reserve(addr(1), 12, Instant::now()),
            "still cooling"
        );
        advance(Duration::from_secs(120)).await;
        r.sampler.prune(Instant::now());
        assert!(r.sampler.reserve(addr(1), 13, Instant::now()), "ended");
    }
}

/// X-split-resume-gate's fallback, under CR-0w: if the snapshot's filter
/// can't be rebuilt (here the new sampler's filter is already in use), its
/// cooldowns are dropped and nothing waits for them: the run samples at
/// once, and the next commit writes no gate.
#[tokio::test(start_paused = true)]
async fn a_filter_that_cant_be_rebuilt_is_dropped_without_a_gate() {
    let d = Dirs::new("spill-fallback");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampler = Sampler::split_with_capacity(1);
    r.sampled(addr(1), 1, Duration::from_secs(3600));
    r.sampled(addr(2), 2, Duration::from_secs(600));
    assert_eq!(r.sampler.spill_counts().0, 1);
    r.commit(&d);

    advance(Duration::from_secs(60)).await;
    let now = Instant::now();
    let mut busy = Sampler::split_with_capacity(1);
    for (i, a) in [addr(5), addr(6)].into_iter().enumerate() {
        assert!(busy.reserve(a, 20 + i as u64, now));
        assert!(busy.complete(
            a,
            20 + i as u64,
            Duration::from_secs(7200 - 600 * i as u64),
            now
        ));
    }
    assert_eq!(busy.spill_counts().0, 1, "in use before seeding");
    let mut r = Run::boot_sampler(&d, &c, busy);
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(r.proof.may_sample(Instant::now()), "no gate");
    assert!(
        !r.sampler.reserve(addr(2), 30, Instant::now()),
        "the listed restriction still holds"
    );
    r.commit(&d);
    let bytes = std::fs::read(d.data.join(restart::SNAPSHOT_FILE)).unwrap();
    let (s, _) = restart::Snapshot::decode(&bytes).unwrap();
    assert_eq!(s.gate_s, 0, "the next commit writes no gate");
}

/// CR-0w: a version-1 snapshot an older binary wrote with a gate (#1118's
/// upgrade path) resumes its per-node restrictions and drops the gate:
/// the first run on this binary samples at once and commits none.
#[tokio::test(start_paused = true)]
async fn an_old_snapshots_gate_is_dropped() {
    let d = Dirs::new("v1gate");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampled(addr(1), 1, Duration::from_secs(3600));
    r.commit(&d);
    // Rewrite the snapshot as an older run would have: the same entries
    // behind a 5 h gate, and the clean marker naming it.
    let path = d.data.join(restart::SNAPSHOT_FILE);
    let (mut snap, _) = restart::Snapshot::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(snap.gate_s, 0);
    snap.gate_s = 5 * 3600;
    let bytes = snap.encode();
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        restart::VERSION
    );
    std::fs::write(&path, &bytes).unwrap();
    let Found::Ok(mut m) = restart::read_marker(&d.data) else {
        panic!()
    };
    let MarkerState::Clean { state_bin, .. } = m.state else {
        panic!("clean")
    };
    m.state = MarkerState::Clean {
        snap: bytes[bytes.len() - 32..].try_into().unwrap(),
        state_bin,
    };
    std::fs::write(d.data.join(restart::MARKER_FILE), m.encode()).unwrap();

    advance(Duration::from_secs(60)).await;
    let mut r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(r.proof.may_sample(Instant::now()), "the gate is dropped");
    assert_eq!(r.proof.suppression_seconds(Instant::now()), 0);
    assert!(
        !r.sampler.reserve(addr(1), 2, Instant::now()),
        "its restriction holds"
    );
    r.commit(&d);
    let (s, _) = restart::Snapshot::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(s.gate_s, 0, "and none is passed on");
}

/// CR-0w, item 4 of the runner's review: inside START_HOLD a busy lock is
/// retried every tick, so a predecessor that lets go at about 8 s still
/// hands over a resume before the hold ends.
#[tokio::test(start_paused = true)]
async fn a_lock_freed_late_in_the_hold_still_resumes() {
    let d = Dirs::new("late-lock");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampled(addr(1), 1, Duration::from_secs(3600));
    r.commit(&d);
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        panic!("this test needs file locks in the temp dir");
    };
    let start = Instant::now();
    let mut r = Run::boot_locked(&d, &c, true);
    for _ in 0..8 {
        advance(TICK).await;
        r.tick();
    }
    assert_eq!(r.proof.reason(), Reason::Locked);
    drop(holder);
    // The next tick's retry takes it; allow the forked-child case a few
    // more ticks, still inside the hold.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while matches!(r.proof.phase, Phase::Locking { .. })
        && Instant::now() < start + START_HOLD - TICK
        && std::time::Instant::now() < deadline
    {
        advance(TICK).await;
        r.tick();
    }
    assert!(Instant::now() < start + START_HOLD, "inside the hold");
    assert_eq!(r.proof.reason(), Reason::Resumed);
    assert!(!r.sampler.reserve(addr(1), 2, Instant::now()), "restored");
}

/// CR-0w, item 3: a run that hasn't sampled (paused, or sampling off)
/// still resumes when its lock frees long after START_HOLD.
#[tokio::test(start_paused = true)]
async fn a_run_that_hasnt_sampled_resumes_after_the_hold() {
    let d = Dirs::new("paused-late");
    let c = clock();
    let mut r = Run::boot(&d, &c, true);
    r.sampled(addr(1), 1, Duration::from_secs(3600));
    r.commit(&d);
    let LockAttempt::Held(holder) = restart::try_lock(&d.data).unwrap() else {
        panic!("this test needs file locks in the temp dir");
    };
    let mut r = Run::boot_locked(&d, &c, true);
    advance(Duration::from_secs(120)).await;
    r.tick();
    assert!(r.proof.may_sample(Instant::now()), "past the hold");
    drop(holder);
    advance(MAX_RETRY).await;
    r.tick();
    r.take_lock();
    assert_eq!(r.proof.reason(), Reason::Resumed, "nothing was sampled");
    assert!(!r.sampler.reserve(addr(1), 2, Instant::now()), "restored");
}

/// CR-0w, item 2: the crash-loop guard. A run that crashed under a minute
/// after it started holds the next start until a minute after that start;
/// one that crashed after longer doesn't hold it at all.
#[tokio::test(start_paused = true)]
async fn a_crash_loop_samples_at_most_once_a_minute() {
    for lived in [20u64, 90] {
        let d = Dirs::new("crashloop");
        let c = clock();
        Run::boot(&d, &c, true).commit(&d);
        let prev_start = Instant::now();
        let crashed = Run::boot(&d, &c, true);
        assert_eq!(crashed.proof.reason(), Reason::Resumed);
        advance(Duration::from_secs(lived)).await;
        drop(crashed); // kill -9 after `lived` seconds
        let r = Run::boot(&d, &c, true);
        assert_eq!(r.proof.reason(), Reason::Dirty, "lived {lived}");
        let now = Instant::now();
        if lived >= 60 {
            assert!(r.proof.may_sample(now), "lived {lived}: no hold");
            assert_eq!(r.proof.suppression_seconds(now), 0);
            continue;
        }
        assert!(!r.proof.may_sample(now), "lived {lived}: held");
        let left = r.proof.suppression_seconds(now);
        assert!((1..=CRASH_LOOP.as_secs()).contains(&left), "{left}");
        let end = prev_start + CRASH_LOOP;
        assert!(!r.proof.may_sample(end - Duration::from_secs(1)));
        assert!(r.proof.may_sample(end + Duration::from_secs(1)));
    }
}

/// CR-0w: the crash-loop guard is capped at a minute. A dirty marker
/// stamped an hour ahead (the clock stepped back an hour, as after an NTP
/// correction) still holds for at most CRASH_LOOP.
#[tokio::test(start_paused = true)]
async fn a_stepped_back_clock_cant_stretch_the_crash_loop_guard() {
    let d = Dirs::new("crashloop-step");
    let c = clock();
    Run::boot(&d, &c, true).commit(&d);
    drop(Run::boot(&d, &c, true)); // kill -9: a dirty marker
    let Found::Ok(mut m) = restart::read_marker(&d.data) else {
        panic!("a marker")
    };
    assert_eq!(m.state, MarkerState::Dirty);
    m.at_ms = restart::floor_ms(c.wall()) + 3_600_000;
    std::fs::write(d.data.join(restart::MARKER_FILE), m.encode()).unwrap();
    let start = Instant::now();
    let r = Run::boot(&d, &c, true);
    assert_eq!(r.proof.reason(), Reason::Dirty);
    let left = r.proof.suppression_seconds(start);
    assert!(
        (1..=CRASH_LOOP.as_secs()).contains(&left),
        "held {left} s, more than the guard's minute"
    );
    assert!(!r.proof.may_sample(start));
    assert!(r.proof.may_sample(start + CRASH_LOOP));
}
