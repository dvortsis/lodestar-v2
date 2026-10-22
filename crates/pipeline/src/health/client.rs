//! One task per tracker: it owns the tracker's client and [`Pacer`], and
//! answers scrape jobs for rounds.
//!
//! A job is a round's hashes. The task splits them into requests the
//! tracker accepts, paces every request, and reports the answers, or that
//! the tracker failed or was skipped.
//!
//! Each request must start by the job's limit: within `max_wait`, and
//! early enough to finish before the round closes. A slow tracker (a
//! raised `min_request_interval`, a Low intensity) sends the requests that
//! fit, a partial job, and the rest of the round's hashes go unasked by
//! it. One that can't start any (backing off, down, banned) skips the job
//! at once, so a bad tracker never holds a round up.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::{Counts, Error, Tracker};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::trackers::{Failure, Health, Pacer};

/// A round's hashes for one tracker.
#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub round: u64,
    pub hashes: Arc<[InfoHash]>,
    /// The last moment ([`Clock`] milliseconds) a request may start and
    /// still be answered before the round closes.
    pub deadline_ms: u64,
}

/// How a job went, for rollups and metrics. Codes are `rollup_hour.key`
/// values: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok = 0,
    Timeout = 1,
    /// Refused with words meaning a rate limit or ban.
    Banned = 2,
    /// Any other failure: network, protocol, HTTP status, refusal.
    Error = 3,
    /// Not asked: the tracker was backing off, down or banned.
    Skipped = 4,
    /// Answered after its round closed: the answers were dropped. Counted
    /// by the scheduler, which knows the round is gone.
    Late = 5,
}

impl Outcome {
    pub const ALL: [Outcome; 6] = [
        Outcome::Ok,
        Outcome::Timeout,
        Outcome::Banned,
        Outcome::Error,
        Outcome::Skipped,
        Outcome::Late,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Timeout => "timeout",
            Outcome::Banned => "banned",
            Outcome::Error => "error",
            Outcome::Skipped => "skipped",
            Outcome::Late => "late",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Done {
    pub round: u64,
    pub tracker: u16,
    /// One entry per hash of the job's leading hashes that were answered
    /// (all of them, for a whole job that succeeded).
    pub answers: Vec<Option<Counts>>,
    pub outcome: Outcome,
    /// How many of the job's leading hashes were asked about, a failed
    /// request's included: 0 for a skip, fewer than all for a partial job.
    pub asked: usize,
}

/// What the scheduler can see of a tracker task without asking it.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// Working on a job: a new one would only queue behind it.
    pub busy: AtomicBool,
    /// When its pacer allows the next request ([`Clock`] milliseconds).
    pub ready_at: AtomicU64,
    /// Its pacer's current spacing between requests, in milliseconds.
    pub interval_ms: AtomicU64,
}

impl Gate {
    /// Whether a job sent now could start at least one request by `limit`
    /// rather than be skipped: the check [`ClientTask`] makes when it
    /// receives it.
    pub fn can_start(&self, limit: u64) -> bool {
        requests_by(
            self.ready_at.load(Ordering::Relaxed),
            self.interval_ms.load(Ordering::Relaxed),
            limit,
        ) > 0
    }
}

/// How many requests, paced `interval_ms` apart from `ready_at`, can start
/// by `limit`.
pub(crate) fn requests_by(ready_at: u64, interval_ms: u64, limit: u64) -> u64 {
    match limit.checked_sub(ready_at) {
        None => 0,
        Some(_) if interval_ms == 0 => u64::MAX,
        Some(room) => 1 + room / interval_ms,
    }
}

/// A tracker as the status page and metrics see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerStatus {
    pub id: u16,
    pub url: String,
    pub health: Health,
    pub failures: u32,
    /// Rounds by [`Outcome`] code.
    pub rounds: [u64; 6],
    /// Unix seconds of the last answer.
    pub last_ok_at: Option<i64>,
}

/// A monotonic millisecond clock shared by a scheduler's pacers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock(Instant);

impl Clock {
    pub fn new() -> Clock {
        Clock(Instant::now())
    }

    pub fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    pub fn at(&self, ms: u64) -> Instant {
        self.0 + Duration::from_millis(ms)
    }
}

pub(crate) struct ClientTask {
    pub id: u16,
    pub tracker: Tracker,
    pub pacer: Pacer,
    pub clock: Clock,
    /// Longest a job may wait for this tracker's next slot.
    pub max_wait: Duration,
    pub status: Arc<Mutex<TrackerStatus>>,
    pub gate: Arc<Gate>,
}

impl ClientTask {
    /// Runs until the job channel closes.
    pub async fn run(mut self, mut jobs: mpsc::Receiver<Job>, done: mpsc::Sender<Done>) {
        self.publish();
        while let Some(job) = jobs.recv().await {
            self.gate.busy.store(true, Ordering::Relaxed);
            let (answers, outcome, asked) = self.scrape(&job).await;
            self.record(outcome);
            self.publish();
            self.gate.busy.store(false, Ordering::Relaxed);
            let d = Done {
                round: job.round,
                tracker: self.id,
                answers,
                outcome,
                asked,
            };
            if done.send(d).await.is_err() {
                return;
            }
        }
    }

    fn publish(&self) {
        self.gate
            .ready_at
            .store(self.pacer.ready_at(), Ordering::Relaxed);
        self.gate
            .interval_ms
            .store(self.pacer.interval().as_millis() as u64, Ordering::Relaxed);
    }

    /// The answers for the leading hashes answered, the outcome, and how
    /// many leading hashes were asked about.
    async fn scrape(&mut self, job: &Job) -> (Vec<Option<Counts>>, Outcome, usize) {
        let now = self.clock.now();
        let limit = (now + self.max_wait.as_millis() as u64).min(job.deadline_ms);
        let per_request = self.tracker.max_hashes().max(1);
        let interval = self.pacer.interval().as_millis() as u64;
        // A partial job when the tracker is slower than the round: the
        // requests that can start in time, the rest unasked. They go out
        // an interval apart from the first, however long it sat idle.
        let fit = requests_by(self.pacer.ready_at().max(now), interval, limit);
        let take = usize::try_from(fit)
            .unwrap_or(usize::MAX)
            .saturating_mul(per_request)
            .min(job.hashes.len());
        if take == 0 {
            return (Vec::new(), Outcome::Skipped, 0);
        }
        let hashes = &job.hashes[..take];
        let mut answers = Vec::with_capacity(take);
        for chunk in hashes.chunks(per_request) {
            let ready = self.pacer.ready_at();
            tokio::time::sleep_until(self.clock.at(ready)).await;
            let now = self.clock.now();
            self.pacer.start(now);
            match self.tracker.scrape(chunk).await {
                Ok(reply) => {
                    self.pacer.succeeded(self.clock.now(), reply.retry_after);
                    // One entry per hash asked, `None` for one left out.
                    answers.extend(
                        reply
                            .counts
                            .into_iter()
                            .chain(std::iter::repeat(None))
                            .take(chunk.len()),
                    );
                }
                Err(e) => {
                    let failure = Failure::of(&e);
                    self.pacer.failed(self.clock.now(), failure);
                    let outcome = match (failure, &e) {
                        (Failure::Banned, _) => Outcome::Banned,
                        (_, Error::Timeout) => Outcome::Timeout,
                        _ => Outcome::Error,
                    };
                    tracing::debug!(tracker = self.id, "scrape failed: {e}");
                    // Earlier requests' answers stand.
                    let asked = answers.len() + chunk.len();
                    return (answers, outcome, asked);
                }
            }
        }
        (answers, Outcome::Ok, take)
    }

    fn record(&self, outcome: Outcome) {
        let mut s = self.status.lock().expect("tracker status lock");
        s.health = self.pacer.health();
        s.failures = self.pacer.failures();
        s.rounds[outcome as usize] += 1;
        if outcome == Outcome::Ok {
            s.last_ok_at = Some(crate::unix_now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_that_start_by_the_limit() {
        // Ready now, 6 s apart: by 12 s three can start, by 11.9 s two.
        assert_eq!(requests_by(1_000, 6_000, 13_000), 3);
        assert_eq!(requests_by(1_000, 6_000, 12_999), 2);
        // Ready after the limit: none, a skip.
        assert_eq!(requests_by(5_001, 6_000, 5_000), 0);
        assert_eq!(requests_by(5_000, 6_000, 5_000), 1);
        assert_eq!(requests_by(0, 0, 0), u64::MAX, "unpaced");
        let gate = Gate::default();
        gate.ready_at.store(10_000, Ordering::Relaxed);
        gate.interval_ms.store(2_000, Ordering::Relaxed);
        assert!(gate.can_start(10_000));
        assert!(!gate.can_start(9_999));
    }
}
