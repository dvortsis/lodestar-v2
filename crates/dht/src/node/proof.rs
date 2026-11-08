//! CR-0d's runtime half: drives the crash-safe restart proof around the
//! actor (`docs/decisions/2026-10-30-CR-0d.md` §2; the on-disk half is
//! [`crate::restart`]).
//!
//! The actor owns one [`Proof`]. It decides when sampling may start
//! ([`Proof::may_sample`]), seeds restored restrictions into the sampler
//! once the start work lands, and commits the snapshot at shutdown. Nothing
//! here sends a packet: routing, passive service and lookups never wait.
//!
//! Modes:
//! - **Legacy:** no data directory (`state_path: None`). Today's full
//!   suppression from the actor's epoch; no lock and no files.
//! - **Off:** the switch is off, or a fixture overrides the start delay
//!   (I5). The lock is held for the run's whole life (N2); a leftover
//!   marker is overwritten as `off` before the first sample, and the
//!   window counts from when the lock was taken (C1).
//! - **On:** S0–S4 at start, seeding on resume, S8–S11 at shutdown.
//!
//! The lock is retried every maintenance tick with backoff (N3). The file
//! work runs on tokio's blocking pool, so a slow disk can't stall routing,
//! and reports back over a channel the actor polls.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use tokio::time::Instant;

use super::sampler::Sampler;
use super::{PersistStep, RestartConfig, Stats};
use crate::NodeId;
use crate::restart::{
    self, Clock, Decision, DirId, Found, Here, Loaded, LockAttempt, Marker, MarkerState, Reason,
    RestartLock, RunHeader, Witness, WitnessFound,
};

/// The longest wait between lock or invalidation retries (I1, N3).
const MAX_RETRY: Duration = Duration::from_secs(60);
/// At shutdown, how long to wait for a start or retry job still running
/// on the blocking pool, so a resume it earned isn't lost.
const DRAIN: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum Mode {
    Legacy,
    Off,
    On,
}

/// Where the run stands with the lock and the start work.
#[derive(Debug)]
enum Phase {
    /// Waiting for the lock: retried at `next`, backing off to `MAX_RETRY`.
    Locking { next: Instant, backoff: Duration },
    /// The start work is running off the actor's thread.
    Starting,
    /// The start work landed (or there's nothing to do).
    Ready,
    /// The filesystem has no locks: this run never resumes, and samples
    /// as today, from the actor's epoch.
    Lockless,
}

/// What the start work reports.
#[derive(Debug)]
struct Started {
    /// This run's S0 instant: the suppression and the gate count from it.
    start: Instant,
    decision: Decision,
    reason: Reason,
    /// The marker (dirty or off) or, when on, the witness, made durable.
    /// False until one lands; a run that had no clean marker needs none.
    invalidated: bool,
    /// Whether the witness bump landed (retried once more before S11).
    witness_ok: bool,
    /// Whether only the marker write can invalidate the earlier proof:
    /// the marker couldn't be read (it may be a clean one), or this run
    /// took a new `dir_id`, so its witness bump is another file (§0.1(4)).
    need_marker: bool,
    /// For the retries and the commit.
    pending: Option<Invalidation>,
    run: Option<RunHeader>,
    dir_path: restart::Digest32,
    path: String,
    errors: Vec<(PersistStep, io::ErrorKind)>,
    /// For `dht.restart_check`: the marker the last run left.
    previous: &'static str,
}

/// The writes that invalidate an earlier proof, kept for retrying (I1).
#[derive(Debug, Clone)]
struct Invalidation {
    marker: Marker,
    witness: Option<(PathBuf, Witness)>,
}

#[derive(Debug)]
pub(crate) struct Proof {
    mode: Mode,
    data_dir: PathBuf,
    state_path: PathBuf,
    cfg: Option<RestartConfig>,
    /// `H`: `MAX_INTERVAL + query_timeout`, and `H_s`.
    horizon: Duration,
    horizon_s: u32,
    /// The suppression for off and legacy runs: `H` unless a fixture set it.
    delay: Duration,
    epoch: Instant,
    phase: Phase,
    /// Shared with every job on the blocking pool, so the lock outlives
    /// any of this run's file work (§2.2): a later run can't start, or
    /// have its temp files deleted, while one is still running.
    lock: Option<Arc<RestartLock>>,
    job: Option<mpsc::Receiver<Started>>,
    /// A start job whose channel closed without a result: rerun it then.
    rerun_at: Option<Instant>,
    started: Option<Started>,
    /// Invalidation retries: when, and the current backoff.
    retry: (Instant, Duration),
    /// An invalidation retry in progress: (marker landed, witness landed).
    retrying: Option<mpsc::Receiver<Retried>>,
    reason: Reason,
    /// No sample to anyone before this (N1, and the suppression).
    gate_until: Option<Instant>,
    /// Seeded restrictions waiting for the actor to apply them.
    seed: Option<Vec<(std::net::SocketAddr, Instant)>>,
    /// The restored count, for the gauge.
    restored: u64,
}

impl Proof {
    pub(crate) fn new(
        state_path: Option<&Path>,
        restart: Option<&RestartConfig>,
        start_delay: Option<Duration>,
        horizon: Duration,
        tick: Duration,
        epoch: Instant,
    ) -> Proof {
        let data_dir = state_path
            .and_then(Path::parent)
            .map(|d| {
                if d.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    d.to_path_buf()
                }
            })
            .unwrap_or_default();
        let mode = match (state_path, restart, start_delay) {
            (None, _, _) => Mode::Legacy,
            (Some(_), Some(_), None) => Mode::On,
            _ => Mode::Off,
        };
        let phase = match mode {
            Mode::Legacy => Phase::Ready,
            _ => Phase::Locking {
                next: epoch,
                backoff: tick,
            },
        };
        Proof {
            reason: match mode {
                Mode::On => Reason::Locked,
                _ => Reason::Off,
            },
            mode,
            data_dir,
            state_path: state_path.map(Path::to_path_buf).unwrap_or_default(),
            cfg: restart.cloned(),
            horizon,
            horizon_s: restart::horizon_s(horizon),
            delay: start_delay.unwrap_or(horizon),
            epoch,
            phase,
            lock: None,
            job: None,
            rerun_at: None,
            started: None,
            retry: (epoch, tick),
            retrying: None,
            gate_until: None,
            seed: None,
            restored: 0,
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.cfg.as_ref().map_or_else(
            || Arc::new(restart::SystemClock) as Arc<dyn Clock>,
            |c| c.clock.clone(),
        )
    }

    fn inline_io(&self) -> bool {
        self.cfg.as_ref().is_some_and(|c| c.inline_io)
    }

    /// Whether the run may send a sample now.
    pub(crate) fn may_sample(&self, now: Instant) -> bool {
        match (&self.mode, &self.phase) {
            (Mode::Legacy, _) | (_, Phase::Lockless) => now >= self.epoch + self.delay,
            (_, Phase::Ready) => {
                self.started.as_ref().is_some_and(|s| s.invalidated)
                    && self.gate_until.is_none_or(|g| now >= g)
            }
            _ => false,
        }
    }

    /// Seconds until sampling may start, never 0 while it can't (I1).
    pub(crate) fn suppression_seconds(&self, now: Instant) -> u64 {
        if self.may_sample(now) {
            return 0;
        }
        let end = match (&self.mode, &self.phase) {
            (Mode::Legacy, _) | (_, Phase::Lockless) => Some(self.epoch + self.delay),
            (_, Phase::Ready) => self.gate_until,
            _ => None,
        };
        end.map_or(0, |e| e.saturating_duration_since(now).as_secs())
            .max(1)
    }

    #[cfg(test)]
    pub(crate) fn reason(&self) -> Reason {
        self.reason
    }

    /// Whether the actor may rewrite `dht-state.bin` now. An on-run must
    /// not before S1 has hashed it, or the next start would see another
    /// writer (`foreign_run`) when there was none.
    pub(crate) fn may_save_state(&self) -> bool {
        !matches!(self.mode, Mode::On)
            || self.started.is_some()
            || matches!(self.phase, Phase::Lockless)
    }

    /// One maintenance tick: the lock, the start work and its retries.
    /// Returns restored restrictions for the sampler, once.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        ids: &[NodeId],
        stats: &Stats,
        sampler: &mut Sampler,
    ) {
        if let Phase::Locking { next, backoff } = self.phase
            && now >= next
        {
            self.try_lock(now, backoff, ids, stats);
        }
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(started) => {
                    self.job = None;
                    self.land(started, stats);
                }
                // The job died (a panic on the pool): try again later (I6).
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    fail(stats, PersistStep::Invalidate, io::ErrorKind::Other);
                    self.rerun_at = Some(now + self.retry.1);
                    self.retry.1 = (self.retry.1 * 2).min(MAX_RETRY);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.rerun_at.is_some_and(|t| now >= t) {
            self.rerun_at = None;
            self.start(ids);
        }
        if let Some(seed) = self.seed.take() {
            for (addr, until) in seed {
                sampler.restore(addr, until);
            }
        }
        self.retry_invalidation(now, stats);
        let (live, refused, honoured) = sampler.restored_counts();
        stats
            .restart_obligations_pending
            .store(live as u64, Ordering::Relaxed);
        stats
            .restart_obligations_refused
            .store(refused, Ordering::Relaxed);
        stats
            .restart_obligations_honoured
            .store(honoured, Ordering::Relaxed);
        stats
            .restart_obligations_restored
            .store(self.restored, Ordering::Relaxed);
        stats.sample_suppression_reason.store(
            Reason::ALL
                .iter()
                .position(|r| *r == self.reason)
                .unwrap_or(0) as u64,
            Ordering::Relaxed,
        );
    }

    /// Opening the lock file and a non-blocking `flock`/`LockFileEx` run on
    /// the actor's thread: neither waits on the disk's contents.
    fn try_lock(&mut self, now: Instant, backoff: Duration, ids: &[NodeId], stats: &Stats) {
        let attempt = restart::try_lock(&self.data_dir);
        match attempt {
            Ok(LockAttempt::Held(lock)) => {
                self.lock = Some(Arc::new(lock));
                self.phase = Phase::Starting;
                self.start(ids);
            }
            Ok(LockAttempt::Unsupported) => {
                self.phase = Phase::Lockless;
                self.reason = match self.mode {
                    Mode::On => Reason::Locked,
                    _ => Reason::Off,
                };
                lodestar_log::log_event!(
                    crate::log_events::RESTART_CHECK,
                    mode = "lockless",
                    reason = self.reason.as_str(),
                    previous = "unknown",
                    generation = 0usize,
                    restored = 0usize,
                    dropped_expired = 0usize
                );
                // Best effort, off the actor's thread: a marker carried over
                // from a filesystem that had locks must not stay clean (§2.2).
                let data_dir = self.data_dir.clone();
                run_blocking(self.inline_io(), move || {
                    if let Found::Ok(m) = restart::read_marker(&data_dir)
                        && !matches!(m.state, MarkerState::Off)
                    {
                        let off = Marker {
                            state: MarkerState::Off,
                            ..m
                        };
                        let _ = restart::overwrite_in_place(
                            &data_dir.join(restart::MARKER_FILE),
                            &off.encode(),
                        );
                    }
                });
            }
            Ok(LockAttempt::Busy) | Err(_) => {
                if let Err(e) = &attempt {
                    fail(stats, PersistStep::Lock, e.kind());
                }
                self.reason = Reason::Locked;
                self.phase = Phase::Locking {
                    next: now + backoff,
                    backoff: (backoff * 2).min(MAX_RETRY),
                };
            }
        }
    }

    /// S0–S4 (or the off-run's invalidation), off the actor's thread.
    fn start(&mut self, ids: &[NodeId]) {
        let job = StartJob {
            on: matches!(self.mode, Mode::On),
            data_dir: self.data_dir.clone(),
            state_path: self.state_path.clone(),
            witness_dir: self.cfg.as_ref().and_then(|c| c.witness_dir.clone()),
            clock: self.clock(),
            ids: restart::ids_digest(ids),
            horizon_s: self.horizon_s,
        };
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        let lock = self.lock.clone();
        run_blocking(self.inline_io(), move || {
            let _ = tx.send(job.run());
            drop(lock);
        });
    }

    fn land(&mut self, started: Started, stats: &Stats) {
        for (e, kind) in &started.errors {
            fail(stats, *e, *kind);
        }
        let (restored, dropped) = match &started.decision {
            Decision::Resume(s) => (s.entries.len(), s.dropped_expired),
            Decision::Suppress(_) => (0, 0),
        };
        lodestar_log::log_event!(
            crate::log_events::RESTART_CHECK,
            mode = match (&self.mode, &started.decision) {
                (Mode::On, Decision::Resume(_)) => "resume",
                (Mode::On, Decision::Suppress(_)) => "suppress",
                _ => "off",
            },
            reason = started.reason.as_str(),
            previous = started.previous,
            generation = started.run.map_or(0, |r| r.generation),
            restored = restored,
            dropped_expired = dropped
        );
        self.phase = Phase::Ready;
        self.reason = if started.invalidated {
            started.reason
        } else {
            Reason::InvalidateFailed
        };
        self.gate_until = match (&self.mode, &started.decision) {
            (Mode::On, Decision::Resume(seeded)) => {
                self.restored = seeded.entries.len() as u64;
                self.seed = Some(seeded.entries.clone());
                seeded.gate_until
            }
            (Mode::On, Decision::Suppress(_)) => Some(started.start + self.horizon),
            _ => Some(started.start + self.delay),
        };
        if let (Some(cfg), Some(run)) = (&self.cfg, &started.run)
            && matches!(self.mode, Mode::On)
        {
            // Orphan cleanup reads paths named in witness files, which
            // can hang: after S4, and never in the way (§1.4).
            if let Some(wdir) = cfg.witness_dir.clone() {
                let (keep, path, now_ms) = (run.dir_id, started.path.clone(), {
                    restart::floor_ms(cfg.clock.wall())
                });
                let lock = self.lock.clone();
                run_blocking(self.inline_io(), move || {
                    let _ = restart::prune_witnesses(&wdir, &keep, &path, now_ms);
                    drop(lock);
                });
            }
        }
        self.started = Some(started);
    }

    /// I1: until the marker or the witness lands, retry both with backoff,
    /// off the actor's thread like the start work.
    fn retry_invalidation(&mut self, now: Instant, stats: &Stats) {
        let Some(started) = &self.started else {
            return;
        };
        if started.invalidated {
            return;
        }
        if let Some(rx) = &self.retrying {
            match rx.try_recv() {
                Ok(r) => {
                    self.retrying = None;
                    self.retried(r, now, stats);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.retrying = None;
                    self.retried(Retried::default(), now, stats);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            return;
        }
        if now < self.retry.0 {
            return;
        }
        let Some(inv) = started.pending.clone() else {
            return;
        };
        let marker_path = self.data_dir.join(restart::MARKER_FILE);
        let (tx, rx) = mpsc::channel();
        self.retrying = Some(rx);
        let lock = self.lock.clone();
        run_blocking(self.inline_io(), move || {
            let marker = restart::overwrite_in_place(&marker_path, &inv.marker.encode()).err();
            let witness = inv
                .witness
                .as_ref()
                .map(|(path, w)| restart::overwrite_in_place(path, &w.encode()).err());
            let _ = tx.send(Retried {
                marker: Some(marker.map(|e| e.kind())),
                witness: witness.map(|w| w.map(|e| e.kind())),
            });
            drop(lock);
        });
    }

    fn retried(&mut self, r: Retried, now: Instant, stats: &Stats) {
        let Some(started) = &mut self.started else {
            return;
        };
        let marker_ok = matches!(r.marker, Some(None));
        let witness_ok = matches!(r.witness, Some(None));
        let landed = marker_ok || (witness_ok && !started.need_marker);
        if !marker_ok {
            let kind = r.marker.flatten().unwrap_or(io::ErrorKind::Other);
            fail(stats, PersistStep::Invalidate, kind);
        }
        if let Some(Some(kind)) = r.witness {
            fail(stats, PersistStep::Witness, kind);
        }
        if !landed {
            let backoff = (self.retry.1 * 2).min(MAX_RETRY);
            self.retry = (now + self.retry.1, backoff);
            return;
        }
        started.invalidated = true;
        started.witness_ok |= witness_ok;
        // A late landing is suppress mode (§2 I1): sampling waits for this
        // run's start + H, even on a resume, and the commit passes that on.
        let own_end = started.start + self.horizon;
        match &started.decision {
            Decision::Resume(_) if matches!(self.mode, Mode::On) => {
                self.gate_until = Some(self.gate_until.map_or(own_end, |g| g.max(own_end)));
                self.reason = Reason::InvalidateFailed;
            }
            _ => self.reason = started.reason,
        }
    }

    /// At shutdown, waits a bounded time for a job still on the blocking
    /// pool, and lands its result, so a resume or invalidation that
    /// finished just now isn't lost.
    fn drain(&mut self, stats: &Stats) {
        let now = Instant::now();
        if let Some(rx) = self.job.take()
            && let Ok(started) = rx.recv_timeout(DRAIN)
        {
            self.land(started, stats);
        }
        if let Some(rx) = self.retrying.take()
            && let Ok(r) = rx.recv_timeout(DRAIN)
        {
            self.retried(r, now, stats);
        }
    }

    /// S8–S11 at shutdown, after the event loop has stopped sending:
    /// seal what's in flight, save `dht-state.bin`, write the snapshot,
    /// then the clean marker. Returns `None` when this run doesn't commit
    /// (off, legacy, never invalidated): the caller saves `dht-state.bin`
    /// as before. Any failure leaves the marker dirty: the next start
    /// suppresses.
    pub(crate) fn commit(
        &mut self,
        sampler: &mut Sampler,
        state: &crate::state::State,
        state_path: &Path,
        stats: &Stats,
    ) -> Option<Result<(), PersistStep>> {
        self.drain(stats);
        // A start job that landed in the drain may have restrictions to
        // seed: they must be in the table before it's captured.
        if let Some(seed) = self.seed.take() {
            for (addr, until) in seed {
                sampler.restore(addr, until);
            }
        }
        let started = self.started.as_ref()?;
        if !matches!(self.mode, Mode::On) || self.lock.is_none() || !started.invalidated {
            return None;
        }
        let run = started.run?;
        let clock = self.clock();
        let began = clock.mono();
        // S8: in flight → `now + H`, under fresh tokens (P6).
        sampler.seal_in_flight(clock.mono() + self.horizon);
        // A witness bump that failed while the marker landed: once more, or
        // the next start reads `generation` (it would still be safe).
        if !started.witness_ok
            && let Some((path, w)) = started.pending.as_ref().and_then(|p| p.witness.as_ref())
            && let Err(e) = restart::overwrite_in_place(path, &w.encode())
        {
            fail(stats, PersistStep::Witness, e.kind());
        }
        let at = |step: PersistStep| move |e: io::Error| (step, e.kind());
        let result = (|| {
            // S9: the ids saved here are the marker's (P2).
            let state_bytes = state.encode();
            restart::write_atomic(state_path, &state_bytes).map_err(at(PersistStep::StateBin))?;
            let run = RunHeader {
                ids: restart::ids_digest(&state.ids),
                ..run
            };
            // The gate: a suppressed run passes on its own window; a
            // resumed one carries forward what it inherited (N1).
            let gate = self.gate_until;
            let now = clock.mono();
            let snapshot = restart::capture_now(
                clock.as_ref(),
                run,
                sampler.restrictions(now).collect::<Vec<_>>(),
                gate,
                self.horizon_s,
            )
            .map_err(|_| (PersistStep::Snapshot, io::ErrorKind::InvalidData))?;
            let bytes = snapshot.encode();
            let snap: restart::Digest32 = bytes[bytes.len() - 32..].try_into().expect("32 bytes");
            restart::write_atomic(&self.data_dir.join(restart::SNAPSHOT_FILE), &bytes)
                .map_err(at(PersistStep::Snapshot))?;
            // S11: the commit point.
            let marker = Marker {
                state: MarkerState::Clean {
                    snap,
                    state_bin: restart::digest(&state_bytes),
                },
                run,
                dir_path: started.dir_path,
                at_ms: restart::stamp_after_capture(clock.as_ref()),
            };
            restart::write_atomic(&self.data_dir.join(restart::MARKER_FILE), &marker.encode())
                .map_err(at(PersistStep::Marker))?;
            Ok(snapshot.entries.len())
        })();
        let ok = result.is_ok();
        lodestar_log::log_event!(
            crate::log_events::RESTART_COMMIT,
            ok = ok,
            duration_ms = clock.mono().saturating_duration_since(began).as_millis() as u64,
            entries = result.as_ref().map_or(0, |n| *n as u64)
        );
        if let Err((step_failed, kind)) = &result {
            fail(stats, *step_failed, *kind);
        }
        self.lock = None;
        Some(result.map(|_| ()).map_err(|(s, _)| s))
    }
}

/// What an invalidation retry got: per write, `Some(None)` landed,
/// `Some(Some(kind))` failed, `None` not attempted (no witness). The
/// default (a job that died) counts as both failing.
#[derive(Debug, Default)]
struct Retried {
    marker: Option<Option<io::ErrorKind>>,
    witness: Option<Option<io::ErrorKind>>,
}

/// Counts a failed write and logs it with its kind (§5.3).
fn fail(stats: &Stats, s: PersistStep, kind: io::ErrorKind) {
    Stats::inc(&stats.restart_persist_errors[step(s)]);
    lodestar_log::log_event!(
        crate::log_events::RESTART_PERSIST_FAILED,
        step = s.as_str(),
        kind = kind_label(kind)
    );
}

fn kind_label(kind: io::ErrorKind) -> &'static str {
    use io::ErrorKind as K;
    match kind {
        K::NotFound => "not_found",
        K::PermissionDenied => "permission_denied",
        K::ReadOnlyFilesystem => "read_only",
        K::StorageFull | K::QuotaExceeded => "storage_full",
        K::TimedOut => "timed_out",
        K::Unsupported => "unsupported",
        K::InvalidData => "invalid_data",
        K::Interrupted => "interrupted",
        K::WouldBlock => "would_block",
        _ => "other",
    }
}

fn step(s: PersistStep) -> usize {
    PersistStep::ALL
        .iter()
        .position(|p| *p == s)
        .expect("listed")
}

/// Runs file work on tokio's blocking pool, or inline when asked (tests)
/// or when there's no runtime.
fn run_blocking(inline: bool, f: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if !inline => {
            h.spawn_blocking(f);
        }
        _ => f(),
    }
}

#[derive(Debug)]
struct StartJob {
    on: bool,
    data_dir: PathBuf,
    state_path: PathBuf,
    witness_dir: Option<PathBuf>,
    clock: Arc<dyn Clock>,
    ids: restart::Digest32,
    horizon_s: u32,
}

/// Test-only: a start job for this data directory waits for a release
/// before it runs, so a test can hold it on the pool (the drain test).
/// Keyed by directory, so no other test's jobs are touched.
#[cfg(test)]
pub(crate) static HOLD_START: std::sync::Mutex<Option<(PathBuf, mpsc::Receiver<()>)>> =
    std::sync::Mutex::new(None);

impl StartJob {
    fn run(self) -> Started {
        #[cfg(test)]
        {
            let held = {
                let mut hold = HOLD_START.lock().expect("hold lock");
                match hold.take() {
                    Some((dir, rx)) if dir == self.data_dir => Some(rx),
                    other => {
                        *hold = other;
                        None
                    }
                }
            };
            if let Some(rx) = held {
                let _ = rx.recv_timeout(Duration::from_secs(10));
            }
        }
        if self.on {
            self.run_on()
        } else {
            self.run_off()
        }
    }

    /// The off-run's invalidation (§5.2): a leftover marker becomes `off`,
    /// keeping its generation and `dir_id`. The window counts from now (C1).
    fn run_off(self) -> Started {
        let start = self.clock.mono();
        let marker = restart::read_marker(&self.data_dir);
        let previous = previous(&marker);
        let mut errors = Vec::new();
        let (invalidated, pending) = match marker {
            Found::Missing => (true, None),
            Found::Ok(m) if matches!(m.state, MarkerState::Off) => (true, None),
            Found::Ok(m) => {
                let off = Marker {
                    state: MarkerState::Off,
                    ..m
                };
                let ok = write_marker(&self.data_dir, &off, &mut errors);
                (
                    ok,
                    Some(Invalidation {
                        marker: off,
                        witness: None,
                    }),
                )
            }
            // An unreadable marker may be a clean one we can't read yet (an
            // I/O error, a permission change), so it must be overwritten
            // before any sample, and retried until it is (§0.1(4)). Its
            // real `dir_id` and path are unknown: a fresh id and a zero
            // path digest make the next on-run read `off_run`, then start
            // a new chain (§1.4).
            Found::Bad(_) => {
                let off = Marker {
                    state: MarkerState::Off,
                    run: RunHeader {
                        generation: 0,
                        dir_id: rand::random(),
                        ids: self.ids,
                        run_id: rand::random(),
                    },
                    dir_path: [0; 32],
                    at_ms: restart::ceil_ms(self.clock.wall()),
                };
                let ok = write_marker(&self.data_dir, &off, &mut errors);
                (
                    ok,
                    Some(Invalidation {
                        marker: off,
                        witness: None,
                    }),
                )
            }
        };
        Started {
            start,
            decision: Decision::Suppress(Reason::Off),
            reason: Reason::Off,
            invalidated,
            witness_ok: true,
            need_marker: true,
            pending,
            run: None,
            dir_path: [0; 32],
            path: String::new(),
            errors,
            previous,
        }
    }

    /// S0–S4.
    fn run_on(self) -> Started {
        let mut errors = Vec::new();
        // S0: canonical path, then wall, then monotonic (I4).
        let path = restart::canonical(&self.data_dir);
        let usable_path = path.clone().unwrap_or_default();
        let here = Here::read(
            self.clock.as_ref(),
            usable_path.clone(),
            self.ids,
            self.horizon_s,
        );
        // S1.
        let _ = restart::remove_stale_tmp(&self.data_dir);
        let _ = std::fs::remove_file(restart::tmp_path(&self.state_path));
        let marker = restart::read_marker(&self.data_dir);
        let snapshot = restart::read_snapshot(&self.data_dir);
        let witness = match (&marker, &self.witness_dir, &path) {
            (Found::Ok(m), Some(w), Some(p)) => restart::read_witness(w, p, &m.run.dir_id),
            (_, Some(_), Some(_)) => WitnessFound::Missing,
            _ => WitnessFound::Unusable,
        };
        let state_bin = std::fs::read(&self.state_path)
            .ok()
            .map(|b| restart::digest(&b));
        // What must land before the first sample (§0.1(4)): nothing if
        // there's no marker, or one that can't resume; the marker or the
        // witness for a readable clean one; only the marker for one we
        // can't read, which may be clean.
        let (had_clean, unreadable) = match &marker {
            Found::Ok(m) => (matches!(m.state, MarkerState::Clean { .. }), false),
            Found::Bad(_) => (false, true),
            Found::Missing => (false, false),
        };
        let previous = previous(&marker);
        let loaded = Loaded {
            marker,
            snapshot,
            witness,
            state_bin,
        };
        // S2.
        let plan = restart::plan(&loaded, &here);
        let mut decision = plan.decision;
        // A data directory with no UTF-8 canonical path has no witness.
        if path.is_none() {
            decision = Decision::Suppress(Reason::NoWitness);
        }
        let reason = match &decision {
            Decision::Resume(_) => Reason::Resumed,
            Decision::Suppress(r) => *r,
        };
        let run = RunHeader {
            generation: plan.generation,
            dir_id: match plan.dir_id {
                DirId::Keep(id) => id,
                DirId::New => rand::random(),
            },
            ids: self.ids,
            run_id: rand::random(),
        };
        let wall = restart::ceil_ms(self.clock.wall());
        let dirty = Marker {
            state: MarkerState::Dirty,
            run,
            dir_path: here.dir_path,
            at_ms: wall,
        };
        // S3: the witness, if this run has a usable one.
        let witness = match (&self.witness_dir, &path) {
            (Some(w), Some(p)) => prepare_witness_dir(w, p).map(|()| {
                (
                    restart::witness_path(w, &run.dir_id),
                    Witness {
                        generation: run.generation,
                        dir_id: run.dir_id,
                        at_ms: wall,
                        path: p.clone(),
                    },
                )
            }),
            _ => None,
        };
        let witness_ok = match &witness {
            Some((file, w)) => match restart::overwrite_in_place(file, &w.encode()) {
                Ok(()) => true,
                Err(e) => {
                    errors.push((PersistStep::Witness, e.kind()));
                    false
                }
            },
            None => false,
        };
        // S4.
        let marker_ok = write_marker(&self.data_dir, &dirty, &mut errors);
        // The witness bump only invalidates the old proof when it's the
        // old marker's own witness: a kept `dir_id`.
        let need_marker = unreadable || (had_clean && matches!(plan.dir_id, DirId::New));
        let invalidated = if need_marker {
            marker_ok
        } else {
            !had_clean || witness_ok || marker_ok
        };
        Started {
            start: here.mono0,
            decision,
            reason,
            invalidated,
            witness_ok,
            need_marker,
            pending: Some(Invalidation {
                marker: dirty,
                witness,
            }),
            run: Some(run),
            dir_path: here.dir_path,
            path: usable_path,
            errors,
            previous,
        }
    }
}

fn previous(marker: &Found<Marker>) -> &'static str {
    match marker {
        Found::Missing => "none",
        Found::Bad(_) => "bad",
        Found::Ok(m) => match m.state {
            MarkerState::Clean { .. } => "clean",
            MarkerState::Dirty => "dirty",
            MarkerState::Off => "off",
        },
    }
}

fn write_marker(
    data_dir: &Path,
    m: &Marker,
    errors: &mut Vec<(PersistStep, io::ErrorKind)>,
) -> bool {
    match restart::overwrite_in_place(&data_dir.join(restart::MARKER_FILE), &m.encode()) {
        Ok(()) => true,
        Err(e) => {
            errors.push((PersistStep::Invalidate, e.kind()));
            false
        }
    }
}

/// Creates the witness dir if needed, then checks it resolves outside the
/// data dir (§1.5). `None` means no witness.
fn prepare_witness_dir(dir: &Path, data_dir_canonical: &str) -> Option<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    // Only this account: the witness is the proof's memory (§1.5).
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    restart::witness_usable(dir, data_dir_canonical).then_some(())
}

#[cfg(test)]
mod tests;
