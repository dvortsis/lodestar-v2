//! The scrape scheduler: lanes → rounds → trackers and the DHT → swarm
//! updates for the single writer.
//!
//! A **round** is up to `round_size` torrents. Every tracker gets the
//! round's hashes as one job (its task splits and paces them), and
//! torrents from the interest, new and rediscovered lanes also get a DHT
//! scrape. A round ends when every answer is in or its deadline passes;
//! its torrents then go to the writer as one batch, each with its next
//! scrape time from the decay schedule.
//!
//! Rounds start once per tracker interval with at most `max_rounds` in
//! flight, so a healthy tracker sees one request per interval and a slow
//! one only skips rounds. Private torrents, and those only private sources
//! reported, are never scraped: a public tracker or the DHT would learn
//! the hash (§18.7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{DhtEstimate, NodeHandle};
use lodestar_metainfo::InfoHash;
use lodestar_sources::Shutdown;
use lodestar_store::{Store, SwarmUpdate, TrackerCounts, flags, swarm_flags};
use lodestar_tracker_scrape::{
    Counts, Deny, HttpConfig, Tracker, TrackerUrl, UdpConfig, UdpTracker, http, udp,
};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::aggregate::{DhtSeen, aggregate};
use super::client::{ClientTask, Clock, Done, Gate, Job, Outcome, TrackerStatus};
use super::lanes::{Lane, Lanes, Weights};
use super::schedule::{Before, Schedule, Seen};
use super::trackers::{Health, Pacer};
use crate::rollups::{Rollups, metric};
use crate::unix_now;

#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// Time between requests to one UDP tracker; rounds start at this
    /// pace too.
    pub interval: Duration,
    /// Time between requests to one HTTP tracker.
    pub http_interval: Duration,
    /// Torrents per round, at most a UDP packet's worth.
    pub round_size: usize,
    pub max_rounds: usize,
    /// A round is closed with whatever answered by then.
    pub round_deadline: Duration,
    /// BEP 33 scrapes for interest, new and rediscovered torrents.
    pub dht: bool,
    /// DHT scrapes waiting on the node at once.
    pub dht_in_flight: usize,
    pub schedule: Schedule,
    pub weights: Weights,
    pub udp: UdpConfig,
    pub http: HttpConfig,
    /// How often the new and due lanes are topped up from the store.
    pub refill_every: Duration,
    /// Blocklisted addresses (§20.13): no tracker on them is contacted.
    /// Overrides `udp.deny` and `http.deny`.
    pub deny: Option<Deny>,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            interval: Duration::from_secs(2),
            http_interval: Duration::from_secs(6),
            round_size: udp::MAX_HASHES,
            max_rounds: 2,
            round_deadline: Duration::from_secs(60),
            dht: true,
            dht_in_flight: 8,
            schedule: Schedule::default(),
            weights: Weights::default(),
            udp: UdpConfig::default(),
            http: HttpConfig::default(),
            refill_every: Duration::from_secs(5),
            deny: None,
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct HealthStats {
    pub rounds: std::sync::atomic::AtomicU64,
    /// Torrents scraped, by result.
    pub alive: std::sync::atomic::AtomicU64,
    pub dead: std::sync::atomic::AtomicU64,
    pub unreached: std::sync::atomic::AtomicU64,
    pub suspect: std::sync::atomic::AtomicU64,
    /// Private torrents taken from a lane and dropped unscraped.
    pub private_skipped: std::sync::atomic::AtomicU64,
    /// Interest and rediscovery hints for torrents scraped recently.
    pub fresh_skipped: std::sync::atomic::AtomicU64,
    /// DHT scrapes not asked because the node was busy with others.
    pub dht_busy: std::sync::atomic::AtomicU64,
    pub dht_requested: std::sync::atomic::AtomicU64,
    pub dht_estimates: std::sync::atomic::AtomicU64,
    /// Interest and rediscovery hints dropped because a lane was full.
    pub hints_dropped: std::sync::atomic::AtomicU64,
    /// Tracker answers that came after their round closed, and were
    /// dropped.
    pub late: std::sync::atomic::AtomicU64,
}

fn inc(c: &std::sync::atomic::AtomicU64) {
    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A cheap, cloneable way in: hints and status.
#[derive(Debug, Clone)]
pub struct HealthHandle {
    interest: mpsc::Sender<i64>,
    rediscovered: mpsc::Sender<i64>,
    stats: Arc<HealthStats>,
    trackers: Arc<Vec<Arc<Mutex<TrackerStatus>>>>,
}

impl HealthHandle {
    /// Someone looked at this torrent: scrape it soon. Never waits.
    pub fn interest(&self, id: i64) {
        if self.interest.try_send(id).is_err() {
            inc(&self.stats.hints_dropped);
        }
    }

    /// The DHT saw this stored torrent again. Never waits.
    pub fn rediscovered(&self, id: i64) {
        if self.rediscovered.try_send(id).is_err() {
            inc(&self.stats.hints_dropped);
        }
    }

    pub fn stats(&self) -> &HealthStats {
        &self.stats
    }

    pub fn trackers(&self) -> Vec<TrackerStatus> {
        self.trackers
            .iter()
            .map(|t| t.lock().expect("tracker status lock").clone())
            .collect()
    }
}

/// Registers the configured trackers (blocking; at startup, while the
/// pipeline still holds the writer's store) and returns their ids.
pub fn register_trackers(
    store: &mut Store,
    urls: &[TrackerUrl],
) -> lodestar_store::Result<Vec<(u16, TrackerUrl)>> {
    let now = unix_now();
    urls.iter()
        .map(|u| Ok((store.upsert_tracker(u.as_str(), now)?, u.clone())))
        .collect()
}

pub(crate) struct Setup {
    pub cfg: HealthConfig,
    /// A read-only connection of the scheduler's own.
    pub reader: Store,
    pub trackers: Vec<(u16, TrackerUrl)>,
    pub dht: Arc<Mutex<Option<NodeHandle>>>,
    /// Batches for the writer.
    pub out: mpsc::Sender<Vec<SwarmUpdate>>,
    pub rollups: Arc<Rollups>,
    pub shutdown: Shutdown,
}

/// Starts the scheduler and one task per tracker. The returned task ends
/// at shutdown, handing back the reader.
pub(crate) fn spawn(mut s: Setup) -> (HealthHandle, JoinHandle<Store>) {
    let (interest_tx, interest_rx) = mpsc::channel(1024);
    let (redisc_tx, redisc_rx) = mpsc::channel(4096);
    let (done_tx, done_rx) = mpsc::channel(256);
    let (dht_tx, dht_rx) = mpsc::channel(256);
    let stats = Arc::new(HealthStats::default());
    let clock = Clock::new();
    s.cfg.udp.deny = s.cfg.deny.clone();
    s.cfg.http.deny = s.cfg.deny.clone();
    let client = match http::client(s.cfg.udp.bind.map(|a| a.ip()), s.cfg.deny.as_ref()) {
        Ok(c) => Some(c),
        Err(e) => {
            lodestar_log::log_event!(
                crate::log_events::HTTP_TRACKERS_DISABLED,
                error = lodestar_log::Error::new(&e)
            );
            None
        }
    };
    let mut jobs = Vec::new();
    let mut statuses = Vec::new();
    for (id, url) in &s.trackers {
        let status = Arc::new(Mutex::new(TrackerStatus {
            id: *id,
            url: url.to_string(),
            health: Health::Ok,
            failures: 0,
            rounds: [0; 6],
            last_ok_at: None,
        }));
        let udp = url.scheme() == lodestar_tracker_scrape::Scheme::Udp;
        let tracker = match &client {
            Some(c) => Tracker::new(url, s.cfg.udp.clone(), s.cfg.http.clone(), c),
            None if udp => Some(Tracker::Udp(UdpTracker::new(
                url.host(),
                url.port(),
                s.cfg.udp.clone(),
            ))),
            None => None,
        };
        let Some(tracker) = tracker else {
            // Its id only: an HTTP tracker URL may carry a passkey.
            lodestar_log::log_event!(
                crate::log_events::TRACKER_SKIPPED,
                tracker = lodestar_log::Count(u64::from(*id))
            );
            continue;
        };
        let interval = if udp {
            s.cfg.interval
        } else {
            s.cfg.http_interval
        };
        // A job may wait for one slot more than a full round takes (one
        // request over UDP, three over HTTP at 32 hashes), so a healthy
        // tracker never skips but a backed-off one does: backoff starts at
        // a minute, well past four default HTTP slots.
        let gate = Arc::new(Gate::default());
        let per_request = tracker.max_hashes().max(1);
        let requests = s.cfg.round_size.max(1).div_ceil(per_request);
        let max_wait = interval * (u32::try_from(requests).unwrap_or(u32::MAX - 1) + 1);
        let task = ClientTask {
            id: *id,
            tracker,
            pacer: Pacer::new(interval, clock.now()),
            clock,
            max_wait,
            status: status.clone(),
            gate: gate.clone(),
        };
        // The longest one request can take: over UDP a connect and a
        // scrape, each with its retries. Capped at half a round, so a
        // timeout longer than the round can't keep a tracker from ever
        // being asked.
        let request = if udp {
            s.cfg.udp.timeout * 2 * (s.cfg.udp.retries + 1)
        } else {
            s.cfg.http.timeout
        }
        .min(s.cfg.round_deadline / 2);
        let (job_tx, job_rx) = mpsc::channel(1);
        tokio::spawn(task.run(job_rx, done_tx.clone()));
        jobs.push(Slot {
            id: *id,
            jobs: job_tx,
            gate,
            status: status.clone(),
            request_ms: request.as_millis() as u64,
            max_wait_ms: max_wait.as_millis() as u64,
        });
        statuses.push(status);
    }
    drop(done_tx);
    let handle = HealthHandle {
        interest: interest_tx,
        rediscovered: redisc_tx,
        stats: stats.clone(),
        trackers: Arc::new(statuses),
    };
    let scheduler = Scheduler {
        dht_permits: Arc::new(Semaphore::new(s.cfg.dht_in_flight.max(1))),
        lanes: Lanes::new(s.cfg.weights),
        cfg: s.cfg,
        store: Some(s.reader),
        rounds: HashMap::new(),
        next_round: 0,
        jobs,
        clock,
        dht: s.dht,
        dht_tx,
        out: s.out,
        rollups: s.rollups,
        stats,
        new_after: 0,
        last_refill: None,
        last_launch: None,
    };
    let task = tokio::spawn(scheduler.run(
        Inputs {
            interest: interest_rx,
            rediscovered: redisc_rx,
            done: done_rx,
            dht: dht_rx,
        },
        s.shutdown,
    ));
    (handle, task)
}

/// Torrents read from the store per lane refill.
const REFILL: usize = 1000;

struct Inputs {
    interest: mpsc::Receiver<i64>,
    rediscovered: mpsc::Receiver<i64>,
    done: mpsc::Receiver<Done>,
    dht: mpsc::Receiver<(u64, usize, Option<DhtEstimate>)>,
}

/// One tracker task as the scheduler sees it.
struct Slot {
    id: u16,
    jobs: mpsc::Sender<Job>,
    gate: Arc<Gate>,
    status: Arc<Mutex<TrackerStatus>>,
    /// The longest one request can take.
    request_ms: u64,
    max_wait_ms: u64,
}

impl Slot {
    /// The last moment a request of a round closing at `deadline_ms` may
    /// start and still be answered in time.
    fn last_start(&self, deadline_ms: u64) -> u64 {
        deadline_ms.saturating_sub(self.request_ms)
    }

    /// Free, and able to start a request within its wait and in time for
    /// a round closing at `deadline_ms`: a job sent now would be worked
    /// on, not queued or skipped.
    fn ready(&self, now_ms: u64, deadline_ms: u64) -> bool {
        !self.gate.busy.load(std::sync::atomic::Ordering::Relaxed)
            && self
                .gate
                .can_start((now_ms + self.max_wait_ms).min(self.last_start(deadline_ms)))
    }
}

struct Item {
    id: i64,
    lane: Lane,
    before: Option<Before>,
    /// The stored DHT estimate, kept when this round doesn't ask the DHT.
    prev_dht: Option<(u32, u32)>,
}

struct Round {
    items: Vec<Item>,
    /// Per item: (tracker, its entry) for every tracker that answered.
    answers: Vec<Vec<(u16, Option<Counts>)>>,
    dht: Vec<Option<DhtSeen>>,
    /// Per item: whether the DHT was asked.
    dht_asked: Vec<bool>,
    waiting_trackers: usize,
    waiting_dht: usize,
    /// Per item: whether some tracker sent a request with its hash.
    asked: Vec<bool>,
    deadline: Instant,
}

/// Which of a round's items to write: those some tracker or the DHT was
/// asked about. The rest learned nothing, not even an outage, and go back
/// to their lanes.
fn settle(asked: &[bool], dht_asked: &[bool]) -> Vec<bool> {
    asked.iter().zip(dht_asked).map(|(t, d)| *t || *d).collect()
}

/// The rounds whose deadline has passed at `now`.
fn expired(rounds: &HashMap<u64, Round>, now: Instant) -> Vec<u64> {
    rounds
        .iter()
        .filter(|(_, r)| r.deadline <= now)
        .map(|(id, _)| *id)
        .collect()
}

struct Scheduler {
    cfg: HealthConfig,
    store: Option<Store>,
    lanes: Lanes,
    rounds: HashMap<u64, Round>,
    next_round: u64,
    jobs: Vec<Slot>,
    clock: Clock,
    dht: Arc<Mutex<Option<NodeHandle>>>,
    dht_tx: mpsc::Sender<(u64, usize, Option<DhtEstimate>)>,
    dht_permits: Arc<Semaphore>,
    out: mpsc::Sender<Vec<SwarmUpdate>>,
    rollups: Arc<Rollups>,
    stats: Arc<HealthStats>,
    /// Torrents up to this id all have a swarm row (or were seen without
    /// one and queued): where the next "new" refill starts.
    new_after: i64,
    last_refill: Option<Instant>,
    last_launch: Option<Instant>,
}

/// What a round needs from the store per torrent.
struct Loaded {
    hash: InfoHash,
    private: bool,
    before: Option<Before>,
    /// When a tracker or the DHT last answered for it.
    scraped_at: Option<i64>,
    /// Its swarm row's next scrape, if it has a row.
    next_scrape_at: Option<i64>,
    dht: Option<(u32, u32)>,
    /// Alive by the DHT alone: no tracker reported it last time.
    dht_only_alive: bool,
}

/// Hints don't rescrape fresh data: a torrent someone looks at is
/// refreshed when its numbers are older than this…
const INTEREST_FRESH: i64 = 30 * 60;
/// …and one the DHT sees again when they're older than an alive swarm's
/// shortest interval (popular torrents are rediscovered constantly).
fn rediscovered_fresh(s: &Schedule) -> i64 {
    s.alive_min.as_secs() as i64
}

impl Scheduler {
    async fn run(mut self, mut io: Inputs, mut shutdown: Shutdown) -> Store {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.requested() => break,
                Some(id) = io.interest.recv() => self.lanes.push(Lane::Interest, id),
                Some(id) = io.rediscovered.recv() => self.lanes.push(Lane::Rediscovered, id),
                Some(d) = io.done.recv() => self.on_done(d).await,
                Some((round, i, est)) = io.dht.recv() => self.on_dht(round, i, est).await,
                _ = tick.tick() => {
                    if !self.on_tick().await {
                        break;
                    }
                }
            }
        }
        // Rounds in flight are dropped: their torrents keep their old
        // schedule and come round again.
        self.store.take().expect("store")
    }

    /// Returns `false` once the writer has gone.
    async fn on_tick(&mut self) -> bool {
        let now = Instant::now();
        for id in expired(&self.rounds, now) {
            if !self.finish(id).await {
                return false;
            }
        }
        if self
            .last_refill
            .is_none_or(|t| t.elapsed() >= self.cfg.refill_every)
        {
            self.last_refill = Some(now);
            self.refill().await;
        }
        let paced = self
            .last_launch
            .is_none_or(|t| t.elapsed() >= self.cfg.interval);
        if paced
            && self.rounds.len() < self.cfg.max_rounds
            && !self.lanes.is_empty()
            && self.can_ask()
        {
            self.last_launch = Some(now);
            self.launch().await;
        }
        true
    }

    /// Whether a round started now could ask anyone: some tracker ready,
    /// or (with no trackers configured) the DHT. Otherwise rounds would
    /// only record outages for torrents nobody was asked about.
    fn can_ask(&self) -> bool {
        let now = self.clock.now();
        if self.jobs.is_empty() {
            return self.cfg.dht && self.dht.lock().expect("dht handle lock").is_some();
        }
        let deadline = now + self.cfg.round_deadline.as_millis() as u64;
        self.jobs.iter().any(|j| j.ready(now, deadline))
    }

    /// Runs `f` on the blocking pool with the store.
    async fn with_store<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut Store) -> T + Send + 'static,
    ) -> T {
        let mut store = self.store.take().expect("store");
        let (store, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut store);
            (store, out)
        })
        .await
        .expect("health store task");
        self.store = Some(store);
        out
    }

    /// Tops up the new and due lanes when they run low.
    async fn refill(&mut self) {
        let low = 4 * self.cfg.round_size;
        let want_new = self.lanes.len(Lane::New) < low;
        let want_due = self.lanes.len(Lane::Due) < low;
        if !want_new && !want_due {
            return;
        }
        let after = self.new_after;
        let now = unix_now();
        let result = self
            .with_store(move |s| -> lodestar_store::Result<_> {
                let new = if want_new {
                    // Read before the scan: anything stored after it has a
                    // higher id and is found next time.
                    let max = s.max_id()?;
                    let ids = s.unscraped_after(after, REFILL)?;
                    let through = match ids.last() {
                        Some((last, _, _)) if ids.len() == REFILL => *last,
                        _ => max,
                    };
                    Some((ids, through))
                } else {
                    None
                };
                let due = if want_due {
                    s.due_for_scrape(now, REFILL)?
                } else {
                    Vec::new()
                };
                Ok((new, due))
            })
            .await;
        match result {
            Ok((new, due)) => {
                if let Some((ids, through)) = new {
                    for (id, _, _) in ids {
                        self.lanes.push(Lane::New, id);
                    }
                    self.new_after = self.new_after.max(through);
                }
                for (id, _, _) in due {
                    self.lanes.push(Lane::Due, id);
                }
            }
            Err(e) => lodestar_log::log_event!(
                crate::log_events::SCHEDULER_STORE_FAILED,
                error = lodestar_log::Error::new(&e)
            ),
        }
    }

    async fn launch(&mut self) {
        let now = unix_now();
        let mut items = Vec::with_capacity(self.cfg.round_size);
        let mut hashes = Vec::with_capacity(self.cfg.round_size);
        let mut dht_wanted = Vec::with_capacity(self.cfg.round_size);
        // Torrents dropped below don't scrape, so top the round up from
        // the lanes, a few passes at most.
        for _ in 0..4 {
            let want = self.cfg.round_size - items.len();
            let taken = self.lanes.take(want);
            if taken.is_empty() {
                break;
            }
            let ids: Vec<i64> = taken.iter().map(|(_, id)| *id).collect();
            let loaded = self
                .with_store(move |s| ids.iter().map(|id| load(s, *id)).collect::<Vec<_>>())
                .await;
            let mut dropped = Vec::new();
            for ((lane, id), l) in taken.into_iter().zip(loaded) {
                // Deleted since it was queued.
                let Some(l) = l else {
                    dropped.push(id);
                    continue;
                };
                if l.private {
                    inc(&self.stats.private_skipped);
                    dropped.push(id);
                    continue;
                }
                let skip = match lane {
                    // Hints don't rescrape fresh numbers.
                    Lane::Interest => l.scraped_at.is_some_and(|t| now - t < INTEREST_FRESH),
                    Lane::Rediscovered => l
                        .scraped_at
                        .is_some_and(|t| now - t < rediscovered_fresh(&self.cfg.schedule)),
                    // Already scraped (through a hint, or an earlier
                    // queueing of the same torrent) since it was queued.
                    Lane::New => l.next_scrape_at.is_some(),
                    Lane::Due => l.next_scrape_at.is_none_or(|t| t > now),
                };
                if skip {
                    inc(&self.stats.fresh_skipped);
                    dropped.push(id);
                    continue;
                }
                // The DHT is asked for hinted and new torrents, and on due
                // rounds for swarms only the DHT showed alive: the trackers
                // alone would call those dead.
                dht_wanted.push(lane != Lane::Due || l.dht_only_alive);
                items.push(Item {
                    id,
                    lane,
                    before: l.before,
                    prev_dht: l.dht,
                });
                hashes.push(l.hash);
            }
            self.lanes.finish(dropped);
            if items.len() >= self.cfg.round_size {
                break;
            }
        }
        if items.is_empty() {
            return;
        }
        let round = self.next_round;
        self.next_round += 1;
        inc(&self.stats.rounds);
        let hashes: Arc<[InfoHash]> = hashes.into();
        let n = items.len();
        let mut r = Round {
            answers: vec![Vec::new(); n],
            dht: vec![None; n],
            dht_asked: vec![false; n],
            items,
            waiting_trackers: 0,
            waiting_dht: 0,
            asked: vec![false; n],
            deadline: Instant::now() + self.cfg.round_deadline,
        };
        let now_ms = self.clock.now();
        let deadline_ms = now_ms + self.cfg.round_deadline.as_millis() as u64;
        for slot in &self.jobs {
            let job = Job {
                round,
                hashes: hashes.clone(),
                deadline_ms: slot.last_start(deadline_ms),
            };
            // Busy with an earlier round, or backing off: it sits this one
            // out instead of holding it up.
            if slot.ready(now_ms, deadline_ms) && slot.jobs.try_send(job).is_ok() {
                r.waiting_trackers += 1;
            } else {
                self.rollups.add_dim(
                    now,
                    metric::SCRAPE_REQUESTS,
                    u32::from(slot.id),
                    Outcome::Skipped as i64,
                    1,
                );
            }
        }
        let node = self.dht.lock().expect("dht handle lock").clone();
        if let (true, Some(node)) = (self.cfg.dht, node) {
            for (i, wanted) in dht_wanted.into_iter().enumerate() {
                if !wanted {
                    continue;
                }
                // The node scrapes about one swarm a second (10% of its
                // query budget): with every permit taken, this torrent
                // goes without a DHT estimate rather than queueing behind
                // an ever-growing backlog.
                let Ok(permit) = self.dht_permits.clone().try_acquire_owned() else {
                    inc(&self.stats.dht_busy);
                    continue;
                };
                r.waiting_dht += 1;
                r.dht_asked[i] = true;
                inc(&self.stats.dht_requested);
                let (node, tx, hash) = (node.clone(), self.dht_tx.clone(), hashes[i]);
                tokio::spawn(async move {
                    let est = node.scrape(hash).await;
                    drop(permit);
                    let _ = tx.send((round, i, est)).await;
                });
            }
        }
        let empty = r.waiting_trackers == 0 && r.waiting_dht == 0;
        self.rounds.insert(round, r);
        if empty {
            self.finish(round).await;
        }
    }

    async fn on_done(&mut self, d: Done) {
        // Answers for a round already closed are dropped: counted as late,
        // not as a success that went somewhere.
        let late = d.outcome == Outcome::Ok && !self.rounds.contains_key(&d.round);
        let outcome = if late { Outcome::Late } else { d.outcome };
        self.rollups.add_dim(
            unix_now(),
            metric::SCRAPE_REQUESTS,
            u32::from(d.tracker),
            outcome as i64,
            1,
        );
        if late {
            inc(&self.stats.late);
            if let Some(slot) = self.jobs.iter().find(|j| j.id == d.tracker) {
                let mut st = slot.status.lock().expect("tracker status lock");
                st.rounds[Outcome::Ok as usize] = st.rounds[Outcome::Ok as usize].saturating_sub(1);
                st.rounds[Outcome::Late as usize] += 1;
            }
        }
        let Some(r) = self.rounds.get_mut(&d.round) else {
            return;
        };
        r.waiting_trackers = r.waiting_trackers.saturating_sub(1);
        for a in r.asked.iter_mut().take(d.asked) {
            *a = true;
        }
        for (slot, a) in r.answers.iter_mut().zip(d.answers) {
            slot.push((d.tracker, a));
        }
        if r.waiting_trackers == 0 && r.waiting_dht == 0 {
            self.finish(d.round).await;
        }
    }

    async fn on_dht(&mut self, round: u64, i: usize, est: Option<DhtEstimate>) {
        self.rollups.add_dim(
            unix_now(),
            metric::DHT_SCRAPES,
            0,
            i64::from(est.is_none()),
            1,
        );
        let Some(r) = self.rounds.get_mut(&round) else {
            return;
        };
        r.waiting_dht = r.waiting_dht.saturating_sub(1);
        if let Some(e) = est {
            inc(&self.stats.dht_estimates);
            if let Some(slot) = r.dht.get_mut(i) {
                *slot = Some(DhtSeen {
                    seeds: e.seeds,
                    peers: e.peers,
                    responders: e.responders,
                });
            }
        }
        if r.waiting_trackers == 0 && r.waiting_dht == 0 {
            self.finish(round).await;
        }
    }

    /// Closes a round and hands its updates to the writer. Returns `false`
    /// once the writer has gone.
    async fn finish(&mut self, round: u64) -> bool {
        let Some(r) = self.rounds.remove(&round) else {
            return true;
        };
        let now = unix_now();
        let write = settle(&r.asked, &r.dht_asked);
        self.lanes.finish(r.items.iter().map(|i| i.id));
        let mut updates = Vec::with_capacity(r.items.len());
        for (i, item) in r.items.iter().enumerate() {
            // Nobody asked about it (every tracker skipped, or a partial
            // job stopped short, and no DHT): nothing was learned, not
            // even an outage. Back to its lane.
            if !write[i] {
                self.lanes.push(item.lane, item.id);
                continue;
            }
            let answers = &r.answers[i];
            // A DHT estimate not asked for this round carries over, as
            // long as some tracker answered: alone it would make an outage
            // look like an answer.
            let dht = match (r.dht_asked[i], item.prev_dht) {
                (true, _) => r.dht[i],
                (false, Some((seeds, peers))) if !answers.is_empty() => Some(DhtSeen {
                    seeds,
                    peers,
                    // Counts for the swarm, not as evidence of fake
                    // seeding (that takes 2).
                    responders: 1,
                }),
                (false, _) => None,
            };
            let agg = aggregate(answers, dht);
            let result = match agg.found {
                Some(seen) if seen.is_alive() => {
                    inc(&self.stats.alive);
                    0
                }
                Some(_) => {
                    inc(&self.stats.dead);
                    1
                }
                None => {
                    inc(&self.stats.unreached);
                    2
                }
            };
            if agg.suspect {
                inc(&self.stats.suspect);
            }
            self.rollups
                .add_dim(now, metric::SCRAPED, item.lane.code() as u32, result, 1);
            let interval = self
                .cfg
                .schedule
                .next_interval(item.id, item.before, agg.found);
            updates.push(SwarmUpdate {
                torrent_id: item.id,
                at: now,
                seeders: agg.hot_seeders(),
                leechers: agg.hot_leechers(),
                completed: agg.completed,
                trackers_reporting: agg.trackers_reporting,
                trackers_checked: agg.trackers_checked,
                dht: agg.dht,
                per_tracker: agg
                    .per_tracker
                    .iter()
                    .map(|(t, c)| TrackerCounts {
                        tracker: *t,
                        seeders: c.seeders,
                        leechers: c.leechers,
                        completed: c.completed,
                    })
                    .collect(),
                next_scrape_at: now + interval,
                flags: if agg.suspect { swarm_flags::SUSPECT } else { 0 },
                interest: item.lane == Lane::Interest,
            });
        }
        if updates.is_empty() {
            return true;
        }
        self.out.send(updates).await.is_ok()
    }
}

/// A torrent's hash, privacy and last scrape (blocking).
fn load(s: &mut Store, id: i64) -> Option<Loaded> {
    let row = s.torrent(id).ok()??;
    let private = row.flags & (flags::PRIVATE | flags::PRIVATE_SOURCE) != 0;
    let swarm = s.swarm(id).ok().flatten();
    let before = swarm.as_ref().map(|w| Before {
        seen: Seen {
            seeders: w.seeders,
            leechers: w.leechers,
        },
        interval: (w.next_scrape_at - w.last_scrape_at).max(0),
        unreached: swarm_flags::unreached(w.flags),
    });
    let next_scrape_at = swarm.as_ref().map(|w| w.next_scrape_at);
    let dht = swarm.as_ref().and_then(|w| w.dht);
    let dht_only_alive = swarm
        .as_ref()
        .is_some_and(|w| w.trackers_reporting == 0 && w.dht.is_some_and(|(s, p)| s + p > 0));
    // A row nobody answered for (`trackers_checked` 0, no DHT estimate)
    // has no data to be fresh.
    let scraped_at = swarm
        .filter(|w| w.trackers_checked > 0 || w.dht.is_some())
        .map(|w| w.last_scrape_at);
    Some(Loaded {
        hash: row.info_hash,
        private,
        before,
        scraped_at,
        next_scrape_at,
        dht,
        dht_only_alive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(n: usize, deadline: Instant) -> Round {
        Round {
            items: (0..n)
                .map(|i| Item {
                    id: i as i64,
                    lane: Lane::New,
                    before: None,
                    prev_dht: None,
                })
                .collect(),
            answers: vec![Vec::new(); n],
            dht: vec![None; n],
            dht_asked: vec![false; n],
            waiting_trackers: 0,
            waiting_dht: 0,
            asked: vec![false; n],
            deadline,
        }
    }

    #[test]
    fn only_what_someone_asked_about_is_written() {
        // Nobody asked: every item goes back to its lane.
        assert_eq!(settle(&[false; 3], &[false; 3]), [false; 3]);
        // A partial job asked about the first two; the DHT about the last.
        assert_eq!(
            settle(&[true, true, false, false], &[false, false, false, true]),
            [true, true, false, true]
        );
    }

    #[test]
    fn rounds_expire_at_their_deadline() {
        let now = Instant::now();
        let mut rounds = HashMap::new();
        rounds.insert(1, round(2, now - Duration::from_millis(1)));
        rounds.insert(2, round(2, now));
        rounds.insert(3, round(2, now + Duration::from_secs(1)));
        let mut gone = expired(&rounds, now);
        gone.sort_unstable();
        assert_eq!(gone, [1, 2]);
    }
}
