//! One task per tracker: it owns the tracker's client and [`Pacer`], and
//! answers scrape jobs for rounds.
//!
//! A job is a round's hashes. The task splits them into requests the
//! tracker accepts, paces every request, and reports the answers, or that
//! the tracker failed or was skipped. A tracker that can't start within
//! `max_wait` (backing off, down, banned) skips the job at once, so a bad
//! tracker never holds a round up; it just isn't counted as checked.

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
}

#[derive(Debug)]
pub(crate) struct Done {
    pub round: u64,
    pub tracker: u16,
    /// One entry per hash on success.
    pub answers: Option<Vec<Option<Counts>>>,
    pub outcome: Outcome,
    /// Whether any request went out: a skip asks nothing.
    pub asked: bool,
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
    /// Whether a job of `hashes` sent now would be worked on rather than
    /// skipped: the same check [`ClientTask`] makes when it receives it.
    pub fn fits(&self, now_ms: u64, hashes: usize, per_request: usize, max_wait_ms: u64) -> bool {
        fits(
            self.ready_at.load(Ordering::Relaxed),
            self.interval_ms.load(Ordering::Relaxed),
            hashes.div_ceil(per_request.max(1)) as u64,
            now_ms + max_wait_ms,
        )
    }
}

/// Every request of a job must start by `limit`: an HTTP tracker taking
/// 32 hashes a request needs three slots for a full round, and giving up
/// after two would throw the first answers away.
fn fits(ready_at: u64, interval_ms: u64, requests: u64, limit: u64) -> bool {
    ready_at + requests.saturating_sub(1) * interval_ms <= limit
}

/// A tracker as the status page and metrics see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerStatus {
    pub id: u16,
    pub url: String,
    pub health: Health,
    pub failures: u32,
    /// Rounds by [`Outcome`] code.
    pub rounds: [u64; 5],
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
            let (answers, outcome, asked) = self.scrape(&job.hashes).await;
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

    /// The answers (on success), the outcome, and whether anything was
    /// asked.
    async fn scrape(
        &mut self,
        hashes: &[InfoHash],
    ) -> (Option<Vec<Option<Counts>>>, Outcome, bool) {
        let limit = self.clock.now() + self.max_wait.as_millis() as u64;
        let per_request = self.tracker.max_hashes().max(1);
        let requests = hashes.len().div_ceil(per_request) as u64;
        let interval = self.pacer.interval().as_millis() as u64;
        if !fits(self.pacer.ready_at(), interval, requests, limit) {
            return (None, Outcome::Skipped, false);
        }
        let mut answers = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(per_request) {
            let ready = self.pacer.ready_at();
            tokio::time::sleep_until(self.clock.at(ready)).await;
            let now = self.clock.now();
            self.pacer.start(now);
            match self.tracker.scrape(chunk).await {
                Ok(reply) => {
                    self.pacer.succeeded(self.clock.now(), reply.retry_after);
                    answers.extend(reply.counts);
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
                    return (None, outcome, true);
                }
            }
        }
        (Some(answers), Outcome::Ok, true)
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
    fn a_job_fits_when_its_last_request_starts_in_time() {
        // Three HTTP requests 6 s apart, ready now: the last starts at 12 s.
        assert!(fits(1_000, 6_000, 3, 13_000));
        assert!(!fits(1_000, 6_000, 3, 12_999));
        // One request only needs the next slot.
        assert!(fits(5_000, 6_000, 1, 5_000));
        assert!(fits(5_000, 6_000, 0, 5_000));
        let gate = Gate::default();
        gate.ready_at.store(10_000, Ordering::Relaxed);
        gate.interval_ms.store(2_000, Ordering::Relaxed);
        // 74 hashes at 32 a request: three requests, the last at 14 s.
        assert!(gate.fits(6_000, 74, 32, 8_000));
        assert!(!gate.fits(6_000, 74, 32, 7_999));
        assert!(gate.fits(6_000, 74, 74, 4_000), "one UDP packet");
    }
}
