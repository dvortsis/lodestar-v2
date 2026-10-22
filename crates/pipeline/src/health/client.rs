//! One task per tracker: it owns the tracker's client and [`Pacer`], and
//! answers scrape jobs for rounds.
//!
//! A job is a round's hashes. The task splits them into requests the
//! tracker accepts, paces every request, and reports the answers, or that
//! the tracker failed or was skipped. A tracker that can't start within
//! `max_wait` (backing off, down, banned) skips the job at once, so a bad
//! tracker never holds a round up; it just isn't counted as checked.

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
}

impl ClientTask {
    /// Runs until the job channel closes.
    pub async fn run(mut self, mut jobs: mpsc::Receiver<Job>, done: mpsc::Sender<Done>) {
        while let Some(job) = jobs.recv().await {
            let (answers, outcome) = self.scrape(&job.hashes).await;
            self.record(outcome);
            let d = Done {
                round: job.round,
                tracker: self.id,
                answers,
                outcome,
            };
            if done.send(d).await.is_err() {
                return;
            }
        }
    }

    async fn scrape(&mut self, hashes: &[InfoHash]) -> (Option<Vec<Option<Counts>>>, Outcome) {
        let limit = self.clock.now() + self.max_wait.as_millis() as u64;
        let mut answers = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(self.tracker.max_hashes().max(1)) {
            let ready = self.pacer.ready_at();
            if ready > limit {
                return (None, Outcome::Skipped);
            }
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
                    return (None, outcome);
                }
            }
        }
        (Some(answers), Outcome::Ok)
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
