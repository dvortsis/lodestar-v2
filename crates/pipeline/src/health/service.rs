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
    Counts, HttpConfig, Tracker, TrackerUrl, UdpConfig, UdpTracker, http, udp,
};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::aggregate::{DhtSeen, aggregate};
use super::client::{ClientTask, Clock, Done, Job, Outcome, TrackerStatus};
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
    pub dht_requested: std::sync::atomic::AtomicU64,
    pub dht_estimates: std::sync::atomic::AtomicU64,
    /// Interest and rediscovery hints dropped because a lane was full.
    pub hints_dropped: std::sync::atomic::AtomicU64,
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
pub(crate) fn spawn(s: Setup) -> (HealthHandle, JoinHandle<Store>) {
    let (interest_tx, interest_rx) = mpsc::channel(1024);
    let (redisc_tx, redisc_rx) = mpsc::channel(4096);
    let (done_tx, done_rx) = mpsc::channel(256);
    let (dht_tx, dht_rx) = mpsc::channel(256);
    let stats = Arc::new(HealthStats::default());
    let clock = Clock::new();
    let client = match http::client(s.cfg.udp.bind.map(|a| a.ip())) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!("HTTP trackers disabled: {e}");
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
            rounds: [0; 5],
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
            tracing::warn!(tracker = %url, "tracker can't be scraped; skipped");
            continue;
        };
        let interval = if udp {
            s.cfg.interval
        } else {
            s.cfg.http_interval
        };
        // A job may wait for one more slot than the round cadence, so a
        // healthy tracker never skips but a backed-off one does.
        let task = ClientTask {
            id: *id,
            tracker,
            pacer: Pacer::new(interval, clock.now()),
            clock,
            max_wait: interval * 2,
            status: status.clone(),
        };
        let (job_tx, job_rx) = mpsc::channel(1);
        tokio::spawn(task.run(job_rx, done_tx.clone()));
        jobs.push((*id, job_tx));
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

struct Item {
    id: i64,
    lane: Lane,
    before: Option<Before>,
}

struct Round {
    items: Vec<Item>,
    /// Per item: (tracker, its entry) for every tracker that answered.
    answers: Vec<Vec<(u16, Option<Counts>)>>,
    dht: Vec<Option<DhtSeen>>,
    waiting_trackers: usize,
    waiting_dht: usize,
    deadline: Instant,
}

struct Scheduler {
    cfg: HealthConfig,
    store: Option<Store>,
    lanes: Lanes,
    rounds: HashMap<u64, Round>,
    next_round: u64,
    jobs: Vec<(u16, mpsc::Sender<Job>)>,
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
        let expired: Vec<u64> = self
            .rounds
            .iter()
            .filter(|(_, r)| r.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
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
        if paced && self.rounds.len() < self.cfg.max_rounds && !self.lanes.is_empty() {
            self.last_launch = Some(now);
            self.launch().await;
        }
        true
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
            Err(e) => tracing::warn!("scrape scheduler can't read the store: {e}"),
        }
    }

    async fn launch(&mut self) {
        let taken = self.lanes.take(self.cfg.round_size);
        if taken.is_empty() {
            return;
        }
        let ids: Vec<i64> = taken.iter().map(|(_, id)| *id).collect();
        let loaded = self
            .with_store(move |s| ids.iter().map(|id| load(s, *id)).collect::<Vec<_>>())
            .await;
        let mut items = Vec::with_capacity(taken.len());
        let mut hashes = Vec::with_capacity(taken.len());
        let mut dropped = Vec::new();
        for ((lane, id), l) in taken.into_iter().zip(loaded) {
            match l {
                Some(l) if !l.private => {
                    items.push(Item {
                        id,
                        lane,
                        before: l.before,
                    });
                    hashes.push(l.hash);
                }
                Some(_) => {
                    inc(&self.stats.private_skipped);
                    dropped.push(id);
                }
                // Deleted since it was queued.
                None => dropped.push(id),
            }
        }
        self.lanes.finish(dropped);
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
            items,
            waiting_trackers: 0,
            waiting_dht: 0,
            deadline: Instant::now() + self.cfg.round_deadline,
        };
        let now = unix_now();
        for (tracker, tx) in &self.jobs {
            let job = Job {
                round,
                hashes: hashes.clone(),
            };
            if tx.try_send(job).is_ok() {
                r.waiting_trackers += 1;
            } else {
                // Still busy with an earlier round: it sits this one out.
                self.rollups.add_dim(
                    now,
                    metric::SCRAPE_REQUESTS,
                    u32::from(*tracker),
                    Outcome::Skipped as i64,
                    1,
                );
            }
        }
        let node = self.dht.lock().expect("dht handle lock").clone();
        if let (true, Some(node)) = (self.cfg.dht, node) {
            for (i, item) in r.items.iter().enumerate() {
                if item.lane == Lane::Due {
                    continue;
                }
                r.waiting_dht += 1;
                inc(&self.stats.dht_requested);
                let (node, tx, permits, hash) = (
                    node.clone(),
                    self.dht_tx.clone(),
                    self.dht_permits.clone(),
                    hashes[i],
                );
                tokio::spawn(async move {
                    let est = match permits.acquire_owned().await {
                        Ok(_permit) => node.scrape(hash).await,
                        Err(_) => None,
                    };
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
        self.rollups.add_dim(
            unix_now(),
            metric::SCRAPE_REQUESTS,
            u32::from(d.tracker),
            d.outcome as i64,
            1,
        );
        let Some(r) = self.rounds.get_mut(&d.round) else {
            return;
        };
        r.waiting_trackers = r.waiting_trackers.saturating_sub(1);
        if let Some(answers) = d.answers {
            for (slot, a) in r.answers.iter_mut().zip(answers) {
                slot.push((d.tracker, a));
            }
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
        let mut updates = Vec::with_capacity(r.items.len());
        for ((item, answers), dht) in r.items.iter().zip(&r.answers).zip(&r.dht) {
            let agg = aggregate(answers, *dht);
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
        self.lanes.finish(r.items.iter().map(|i| i.id));
        self.out.send(updates).await.is_ok()
    }
}

/// A torrent's hash, privacy and last scrape (blocking).
fn load(s: &mut Store, id: i64) -> Option<Loaded> {
    let row = s.torrent(id).ok()??;
    let private = row.flags & (flags::PRIVATE | flags::PRIVATE_SOURCE) != 0;
    let before = s.swarm(id).ok().flatten().map(|w| Before {
        seen: Seen {
            seeders: w.seeders,
            leechers: w.leechers,
        },
        interval: (w.next_scrape_at - w.last_scrape_at).max(0),
    });
    Some(Loaded {
        hash: row.info_hash,
        private,
        before,
    })
}
