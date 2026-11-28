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

/// A scrape error's kind for the log: never its message, which can carry
/// a tracker's own text (a refusal) or a URL.
fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::Timeout => "timeout",
        Error::Resolve => "resolve",
        Error::Denied => "denied",
        Error::Io(_) => "io",
        Error::Refused(_) => "refused",
        Error::Status(_) => "http_status",
        Error::Protocol(_) => "protocol",
        Error::TooLarge => "too_large",
        Error::BadRequest(_) => "bad_request",
        Error::Http(_) => "http",
        _ => "other",
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
    /// X-vpn-bind: while the crawl's interface is down, jobs are skipped
    /// (no request, no failure counted against the tracker).
    pub guard: Option<Arc<lodestar_network_policy::iface::Guard>>,
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

    /// Whether a job must be skipped because the crawl's interface is
    /// down (X-vpn-bind).
    fn vpn_down(&self) -> bool {
        self.guard.as_ref().is_some_and(|g| !g.state().is_up())
    }

    /// Whether a job must be skipped because this HTTP tracker's address is
    /// of a family the interface has no address of (X-vpn-bind): pinning
    /// the device alone, the request would leave with another interface's
    /// source address. UDP sockets check their family when pinned.
    fn off_family(&self) -> bool {
        match (&self.guard, &self.tracker) {
            (Some(guard), Tracker::Http(t)) => {
                t.literal_ip().is_some_and(|ip| !guard.has_family(ip))
            }
            _ => false,
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
        if take == 0 || self.vpn_down() || self.off_family() {
            return (Vec::new(), Outcome::Skipped, 0);
        }
        let hashes = &job.hashes[..take];
        let mut answers = Vec::with_capacity(take);
        for chunk in hashes.chunks(per_request) {
            let ready = self.pacer.ready_at();
            tokio::time::sleep_until(self.clock.at(ready)).await;
            // X-vpn-bind: again after the wait, which can be most of a
            // round; the HTTP client is pinned to the device only, so a
            // drop or a lost family meanwhile must stop the request here.
            if self.vpn_down() || self.off_family() {
                if answers.is_empty() {
                    return (answers, Outcome::Skipped, 0);
                }
                let asked = answers.len();
                return (answers, Outcome::Ok, asked);
            }
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
                    lodestar_log::log_event!(
                        crate::log_events::SCRAPE_FAILED,
                        tracker = lodestar_log::Count(u64::from(self.id)),
                        error = lodestar_log::Error::kind(error_kind(&e))
                    );
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

    /// A UDP tracker task with `guard`, aimed at a port nothing answers.
    fn task(guard: Arc<lodestar_network_policy::iface::Guard>) -> ClientTask {
        task_for(
            guard,
            Tracker::Udp(lodestar_tracker_scrape::UdpTracker::new(
                "192.0.2.1",
                9,
                Default::default(),
            )),
        )
    }

    fn task_for(guard: Arc<lodestar_network_policy::iface::Guard>, tracker: Tracker) -> ClientTask {
        let clock = Clock::new();
        ClientTask {
            id: 1,
            tracker,
            pacer: Pacer::new(Duration::from_secs(1), clock.now()),
            clock,
            max_wait: Duration::from_secs(60),
            status: Arc::new(Mutex::new(TrackerStatus {
                id: 1,
                url: String::new(),
                health: Health::Ok,
                failures: 0,
                rounds: [0; 6],
                last_ok_at: None,
            })),
            gate: Arc::default(),
            guard: Some(guard),
        }
    }

    fn job() -> Job {
        Job {
            round: 1,
            hashes: Arc::from(vec![InfoHash([1; 20])]),
            deadline_ms: u64::MAX,
        }
    }

    /// X-vpn-bind: while the crawl's interface is down a job is skipped,
    /// with no request and no failure against the tracker.
    #[tokio::test]
    async fn jobs_are_skipped_while_the_interface_is_down() {
        use lodestar_network_policy::iface::{Down, Guard, Interface, Link};
        let up = Arc::new(AtomicBool::new(false));
        let u = up.clone();
        let guard = Guard::with_probe(Interface::parse("wg0").unwrap(), move |_| {
            if u.load(Ordering::SeqCst) {
                Ok(Link::default())
            } else {
                Err(Down::Missing)
            }
        });
        let mut t = task(guard.clone());
        let (answers, outcome, asked) = t.scrape(&job()).await;
        assert_eq!((answers.len(), outcome, asked), (0, Outcome::Skipped, 0));
        assert_eq!(t.pacer.failures(), 0, "not a failure");
        up.store(true, Ordering::SeqCst);
        guard.poll();
        assert!(!t.vpn_down(), "up again: jobs run");
    }

    /// X-vpn-bind: an HTTP tracker whose address is of a family the
    /// interface has no address of is skipped, with no request: pinned to
    /// the device alone, it would leave with another interface's source.
    #[tokio::test]
    async fn an_http_tracker_of_a_family_the_interface_lacks_is_skipped() {
        use lodestar_network_policy::iface::{Guard, Interface, Link};
        let guard = Guard::with_probe(Interface::parse("wg0").unwrap(), |_| {
            Ok(Link {
                index: Some(1),
                addrs: vec!["10.64.0.2".parse().unwrap()],
            })
        });
        guard.poll();
        let client = lodestar_tracker_scrape::http::client(None, None).unwrap();
        let http = |u: &str| {
            let url = lodestar_tracker_scrape::TrackerUrl::parse(u).unwrap();
            Tracker::new(&url, Default::default(), Default::default(), &client).unwrap()
        };
        let mut t = task_for(guard.clone(), http("http://[2001:db8::66]:6969/announce"));
        let (answers, outcome, asked) = t.scrape(&job()).await;
        assert_eq!((answers.len(), outcome, asked), (0, Outcome::Skipped, 0));
        assert_eq!(t.pacer.failures(), 0, "not a failure");
        let v4 = task_for(guard, http("http://203.0.113.9:6969/announce"));
        assert!(!v4.off_family(), "a family it has: asked");
    }

    /// X-vpn-bind: the guard is checked again before each request, not
    /// only at the job's start: the wait for a tracker's next slot can be
    /// most of a round, and the HTTP client is pinned to the device only.
    /// Losing the tracker's family after the first request stops the rest.
    #[tokio::test]
    async fn an_http_job_stops_when_the_interface_loses_the_trackers_family() {
        use lodestar_network_policy::iface::{Guard, Interface, Link};
        let both = Link {
            index: Some(1),
            addrs: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
        };
        let link = Arc::new(Mutex::new(both));
        let l = link.clone();
        let guard = Guard::with_probe(Interface::parse("wg0").unwrap(), move |_| {
            Ok(l.lock().unwrap().clone())
        });
        guard.poll();
        let fake = lodestar_tracker_scrape::testing::http_tracker().await;
        let client = lodestar_tracker_scrape::http::client(None, None).unwrap();
        let url = lodestar_tracker_scrape::TrackerUrl::parse(&fake.url(false)).unwrap();
        let tracker = Tracker::new(&url, Default::default(), Default::default(), &client).unwrap();
        let per_request = tracker.max_hashes();
        let mut t = task_for(guard.clone(), tracker);
        // Far enough apart that the change below lands between the two.
        t.pacer = Pacer::new(Duration::from_secs(5), t.clock.now());
        let hashes: Vec<InfoHash> = (0..per_request + 8)
            .map(|i| InfoHash([i as u8; 20]))
            .collect();
        let job = Job {
            round: 1,
            hashes: Arc::from(hashes),
            deadline_ms: u64::MAX,
        };
        let state = fake.state.clone();
        let drop_v4 = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while state.scrapes.load(Ordering::SeqCst) == 0 {
                assert!(tokio::time::Instant::now() < deadline, "no first request");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            *link.lock().unwrap() = Link {
                index: Some(1),
                addrs: vec!["::1".parse().unwrap()],
            };
            guard.poll();
        });
        let (answers, outcome, asked) = t.scrape(&job).await;
        drop_v4.await.unwrap();
        assert_eq!(
            (answers.len(), outcome, asked),
            (per_request, Outcome::Ok, per_request)
        );
        assert_eq!(
            fake.state.scrapes.load(Ordering::SeqCst),
            1,
            "the second chunk wasn't requested"
        );
    }

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
