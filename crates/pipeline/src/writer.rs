//! The single writer (architecture §7), the coordinator's commit step
//! (§19.4): the one task that changes the store. It commits prepared
//! torrents under current leases, applies rediscoveries, and flushes the
//! Insights counters with them, or on their own every few seconds when
//! nothing else is committing.
//!
//! SQLite calls block, so each transaction runs on the blocking pool with
//! the store moved in and back out; the async side never waits on anything
//! that waits on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use lodestar_cluster::{Lease, WorkerId};
use lodestar_filter::{
    Action, Decision, Engine, Facts, Handle, HeldSet, Item, Pending, Sealer, Verdict,
};
use lodestar_metainfo::InfoHash;
use lodestar_sources::{Discovery, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::hot::flags as hot_flags;
use lodestar_store::{
    CohortCount, Counter, Event, Filtered, Held, HotColumns, Inserted, PreparedTorrent, QueuedKey,
    SHADOW, SourceId, Store, SwarmUpdate, Touch, WriteBatch, flags, swarm_flags,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, interval_at};

use crate::coordinator::Coordinator;
use crate::gate::{Excluded, GateStats, Origin};
use crate::have::{Bloom, Have, capacity_for};
use crate::health::HealthHandle;
use crate::rollups::{Rollups, metric};
use crate::worker::{Results, collect};
use crate::{event, unix_now};

#[derive(Debug, Clone)]
pub(crate) struct WriterConfig {
    pub flush_every: Duration,
    pub batch_max: usize,
    pub batch_wait: Duration,
    /// Torrents discovered before this (unix seconds) are flagged
    /// `BACKFILL`: their discovery date only means "when we first saw it".
    pub backfill_until: i64,
    /// Quarantined torrents are purged after this many seconds unless
    /// released or confirmed (§20.3).
    pub quarantine_secs: i64,
    /// Filter hits are kept this long (§20.11).
    pub hits_secs: i64,
}

/// Expired quarantine and old filter hits are purged this often.
const PURGE_EVERY: i64 = 3600;

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct WriterStats {
    pub stored: AtomicU64,
    /// Fetched, but already stored (a race with the have-filter).
    pub existing: AtomicU64,
    pub invalid: AtomicU64,
    /// Dropped by a filter rule (or refused by the floor before parsing).
    pub blocked: AtomicU64,
    /// Held in quarantine by a filter rule.
    pub quarantined: AtomicU64,
    pub rediscovered: AtomicU64,
    /// Have-filter "yes" answers the store contradicted; sent on to fetch.
    pub false_positives: AtomicU64,
    pub transactions: AtomicU64,
    pub errors: AtomicU64,
    pub filter_rebuilds: AtomicU64,
    /// Unix seconds of the last commit.
    pub last_commit: AtomicU64,
    /// Stored torrents enriched in the background (imports, older rows,
    /// records from an older parser).
    pub enriched: AtomicU64,
    /// Scrape results stored.
    pub swarm_updates: AtomicU64,
}

pub(crate) struct WriterIo {
    pub results: mpsc::Receiver<Results>,
    pub touches: mpsc::Receiver<Discovery>,
    /// Periodic snapshots of the fetch queue, to persist.
    pub queues: mpsc::Receiver<Vec<QueuedKey>>,
    /// Have-filter false positives go back to intake, to be claimed.
    pub retry: mpsc::Sender<Discovery>,
    /// The pool's queue at shutdown.
    pub finish: oneshot::Receiver<Vec<QueuedKey>>,
    /// Scrape rounds from the health service.
    pub swarm: mpsc::Receiver<Vec<SwarmUpdate>>,
}

pub(crate) struct Writer {
    pub store: Option<Store>,
    /// The background enrichment pass: the last id it reached, and whether
    /// it has found nothing left to do.
    pub backfill_after: i64,
    pub backfill_done: bool,
    /// Starts another background-enrichment pass (see `PipelineConfig`).
    pub reenrich: Arc<AtomicBool>,
    pub cfg: WriterConfig,
    pub have: Arc<Have>,
    pub funnel: Arc<FunnelCounts>,
    pub rollups: Arc<Rollups>,
    pub filter: Arc<Handle>,
    pub sealer: Option<Arc<Sealer>>,
    /// Tombstoned and quarantined keys, which intake refuses.
    pub held: Arc<HeldSet>,
    pub last_purge: i64,
    pub coordinator: Arc<Coordinator>,
    pub stats: Arc<WriterStats>,
    /// The crawler gate's commit-point counters.
    pub gate: Arc<GateStats>,
    /// Updated after each commit, so SQLite is always ahead of them.
    pub hot: Option<Arc<HotColumns>>,
    /// Told about rediscoveries, to scrape them sooner (§5).
    pub health: Option<HealthHandle>,
    /// The import-candidate refile sweep.
    pub candidates: CandidateSweep,
    /// Runs between a sweep or backfill batch's commit and its hold
    /// check, so tests can land a release or a rule change exactly there.
    #[cfg(test)]
    pub(crate) between: Option<fn(&mut Store, &HeldSet, &Handle)>,
    /// Runs right after a batch marks its hashes pending, before its store
    /// commit, in every arm (`2026-10-31-B-held-precommit.md`).
    #[cfg(test)]
    pub(crate) marked: Option<MarkedHook>,
}

/// A test hook run with the hashes a batch just marked pending.
#[cfg(test)]
pub(crate) type MarkedHook = fn(&HeldSet, &Handle, &[[u8; 20]]);

/// After a batch that marked `pending` committed or rolled back: tells
/// intake to refuse those the store holds and clears the marks, asked
/// under `HeldSet`'s lock, so a release since isn't undone and a hold is
/// never missed (`2026-10-31-B-held-exact.md`,
/// `2026-10-31-B-held-precommit.md`). On a store error every hash goes in.
fn hold_committed(store: &Store, pending: Option<Pending<'_>>) {
    let Some(pending) = pending else { return };
    if let Err(e) = pending.settle(|hs| store.held_among(hs)) {
        lodestar_log::log_event!(
            crate::log_events::ENRICHMENT_FAILED,
            error = crate::log_events::store_error(&e)
        );
    }
}

/// The sweep that judges waiting import candidates again whenever the
/// filter engine changes, and once per start
/// (`docs/decisions/2026-10-31-B-import-refile.md`).
#[derive(Debug, Default)]
pub(crate) struct CandidateSweep {
    /// The engine version it runs under; `None` until the first batch.
    pub version: Option<u64>,
    /// The last candidate id it reached.
    pub after: i64,
    /// Nothing is left to judge under `version`.
    pub done: bool,
    /// After a store error, when the flush tick lets it try again (Unix
    /// seconds); it waits until then.
    pub retry_at: Option<i64>,
    /// Store errors in a row, for the backoff.
    pub failures: u32,
}

impl CandidateSweep {
    /// Has a batch to judge now.
    fn ready(&self) -> bool {
        !self.done && self.retry_at.is_none()
    }

    /// A store error (`SQLITE_BUSY` past the busy timeout, say): waits,
    /// longer after each error in a row, and tries the same batch again.
    fn failed(&mut self, now: i64) {
        self.failures = self.failures.saturating_add(1);
        let backoff = REFILE_RETRY_FIRST
            .saturating_mul(1 << (self.failures - 1).min(10))
            .min(REFILE_RETRY_MAX);
        self.retry_at = Some(now.saturating_add(backoff));
    }

    /// Ends the wait once it's due.
    fn retry_due(&mut self, now: i64) {
        if self.retry_at.is_some_and(|at| now >= at) {
            self.retry_at = None;
        }
    }
}

/// A result under a current lease, ready to commit.
struct Committing {
    results: Results,
    lease: Lease,
    /// The authoritative filter verdict, and the sealed name for a
    /// quarantine.
    verdict: Option<Decision>,
    /// What rules in shadow mode would have done (logged only).
    shadow: Vec<Decision>,
    sealed: Option<Vec<u8>>,
}

/// What a transaction did, for the async side to account.
struct Outcome {
    /// Per stored or already-stored result: (index, new?, swarm hashes).
    stored: Vec<(usize, i64, bool, Vec<InfoHash>)>,
    /// Results the authoritative blocklist rejected.
    blocked: Vec<usize>,
    /// Per touch: the torrent's id, if the store had it.
    touched: Vec<Option<i64>>,
    /// Per swarm update: whether its torrent still exists.
    swarm: Vec<bool>,
    /// The classification rules changed under the batch (§20.9).
    stale_class: bool,
}

impl Writer {
    /// Runs until the pool has stopped (its output closes), then persists
    /// the final queue and returns the store.
    pub async fn run(mut self, mut io: WriterIo) -> Store {
        let start = Instant::now() + self.cfg.flush_every;
        let mut flush = interval_at(start, self.cfg.flush_every);
        loop {
            // Unbiased, so a steady stream of fetches can't starve the
            // rediscoveries or the flush.
            tokio::select! {
                first = io.results.recv() => {
                    let Some(first) = first else { break };
                    let batch =
                        collect(first, &mut io.results, self.cfg.batch_max, self.cfg.batch_wait).await;
                    self.commit(batch, Vec::new(), Vec::new(), Vec::new(), &io.retry).await;
                }
                Some(first) = io.touches.recv() => {
                    let mut touches = vec![first];
                    while touches.len() < 4 * self.cfg.batch_max {
                        match io.touches.try_recv() {
                            Ok(d) => touches.push(d),
                            Err(_) => break,
                        }
                    }
                    self.commit(Vec::new(), touches, Vec::new(), Vec::new(), &io.retry).await;
                }
                Some(swarm) = io.swarm.recv() => {
                    self.commit(Vec::new(), Vec::new(), swarm, Vec::new(), &io.retry).await;
                }
                Some(queue) = io.queues.recv() => self.save_queue(queue).await,
                // Always ready until done; the unbiased select gives it a
                // fair share between ingest batches.
                () = std::future::ready(()), if !self.backfill_done => self.backfill().await,
                () = std::future::ready(()), if self.candidates.ready() => self.refile_candidates().await,
                _ = flush.tick() => {
                    if self.reenrich.swap(false, Ordering::AcqRel) {
                        self.backfill_after = 0;
                        self.backfill_done = false;
                    }
                    self.note_engine();
                    self.candidates.retry_due(unix_now());
                    self.commit(Vec::new(), Vec::new(), Vec::new(), Vec::new(), &io.retry).await;
                    if unix_now() - self.last_purge >= PURGE_EVERY {
                        self.purge().await;
                    }
                }
            }
        }
        // Scrape rounds already handed over are kept: the health service
        // stopped before the pipeline's inputs did.
        let mut swarm = Vec::new();
        while let Ok(batch) = io.swarm.try_recv() {
            swarm.extend(batch);
        }
        let queue = io.finish.await.unwrap_or_default();
        let stop = Event {
            at: unix_now(),
            kind: event::STOP,
            detail: format!("{} keys queued", queue.len()),
        };
        self.commit(Vec::new(), Vec::new(), swarm, vec![stop], &io.retry)
            .await;
        self.save_queue(queue).await;
        let store = self.store.take().expect("store");
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.checkpoint() {
                lodestar_log::log_event!(
                    crate::log_events::CHECKPOINT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
            store
        })
        .await
        .expect("checkpoint task")
    }

    /// The authoritative filter check (§19.4, §20.4): a verdict from the
    /// built-in worker under the current rule set stands; anything else is
    /// judged again. Quarantined names are sealed here.
    fn judge(&self, mut committing: Vec<Committing>) -> Vec<Committing> {
        let engine = self.filter.current();
        for c in &mut committing {
            let r = &mut c.results;
            let verdict = if r.worker == WorkerId::LOCAL && r.version == engine.version {
                std::mem::take(&mut r.verdict)
            } else {
                rejudge(&engine, r, c.lease.discovered_at, unix_now())
            };
            c.verdict = verdict.decision;
            c.shadow = verdict.shadow;
            if c.verdict
                .as_ref()
                .is_some_and(|h| h.action == Action::Quarantine)
            {
                c.sealed = self
                    .sealer
                    .as_ref()
                    .and_then(|s| s.seal(&r.torrent.name).ok());
            }
        }
        committing
    }

    /// Removes expired quarantine (its keys become fetchable again, except
    /// adult holds, which stay refused as tombstones) and filter hits past
    /// their retention.
    async fn purge(&mut self) {
        let now = unix_now();
        self.last_purge = now;
        let hits_before = now - self.cfg.hits_secs;
        let mut store = self.store.take().expect("store");
        let held = self.held.clone();
        let (store, result) = tokio::task::spawn_blocking(move || {
            let r = store.purge_filter(now, hits_before);
            // Only what nothing holds again since: a batch on another
            // connection may have held an expired hash anew.
            if let Ok((expired, _)) = &r
                && let Err(e) = held.remove_released(expired, |hs| store.held_among(hs))
            {
                lodestar_log::log_event!(
                    crate::log_events::FILTER_PURGE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
            let snaps = store.purge_snapshots(now, hits_before);
            for s in &snaps.expired {
                let days = lodestar_store::snapshots::RETAIN_DAYS;
                lodestar_log::log_event!(
                    crate::log_events::SNAPSHOT_EXPIRED,
                    what = s.kind.describe(),
                    path = lodestar_log::Path::new(&s.path),
                    days = days
                );
            }
            // Their text names paths and errors: counted, not repeated.
            if !snaps.warnings.is_empty() {
                lodestar_log::log_event!(
                    crate::log_events::SNAPSHOT_PURGE_PROBLEMS,
                    count = snaps.warnings.len()
                );
            }
            (store, r)
        })
        .await
        .expect("purge task");
        self.store = Some(store);
        match result {
            Ok(_) => {}
            Err(e) => lodestar_log::log_event!(
                crate::log_events::FILTER_PURGE_FAILED,
                error = crate::log_events::store_error(&e)
            ),
        }
    }

    async fn commit(
        &mut self,
        results: Vec<Results>,
        touches: Vec<Discovery>,
        swarm: Vec<SwarmUpdate>,
        events: Vec<Event>,
        retry: &mpsc::Sender<Discovery>,
    ) {
        // Fencing (§19.3): only results under a current lease count. A
        // stale one comes from a holder whose lease expired; the key's new
        // holder will deliver it.
        let delivered = results.len();
        let committing: Vec<Committing> = results
            .into_iter()
            .filter_map(|r| {
                let lease = self
                    .coordinator
                    .current(&r.key, r.token)
                    .filter(|l| l.holder == r.worker)?;
                Some(Committing {
                    results: r,
                    lease,
                    verdict: None,
                    shadow: Vec::new(),
                    sealed: None,
                })
            })
            .collect();
        self.gate
            .exclude(Excluded::Duplicate, (delivered - committing.len()) as u64);
        // The classification rules' generation before the rules
        // themselves: a batch judged under rules older than the store's
        // generation is marked for enriching again (§20.9).
        let class_gen = self.store.as_ref().and_then(|s| s.class_gen().ok());
        let committing = self.judge(committing);
        let cohorts = self.funnel.drain();
        let mut counters = self.rollups.drain();
        // The same transitions by event time, at the flush (B11b).
        counters.extend(crate::rollups::funnel_events(&cohorts, unix_now()));
        if committing.is_empty()
            && touches.is_empty()
            && swarm.is_empty()
            && events.is_empty()
            && cohorts.is_empty()
            && counters.is_empty()
        {
            return;
        }
        let mut store = self.store.take().expect("store");
        let now = unix_now();
        let quarantine_until = now + self.cfg.quarantine_secs;
        let backfill_until = self.cfg.backfill_until;
        // Marked before the commit, so intake refuses them from before the
        // store holds them (`2026-10-31-B-held-precommit.md`).
        let holding: Vec<[u8; 20]> = committing
            .iter()
            .filter(|c| {
                matches!(
                    c.verdict.as_ref().map(|h| h.action),
                    Some(Action::Drop | Action::Quarantine)
                )
            })
            .flat_map(|c| c.results.torrent.swarm_hashes().map(|h| h.0))
            .collect();
        let held = self.held.clone();
        #[cfg(test)]
        let (between, marked, filter) = (self.between, self.marked, self.filter.clone());
        let (store, committing, touches, swarm, result) = tokio::task::spawn_blocking(move || {
            let pending = (!holding.is_empty()).then(|| held.pend(&holding));
            #[cfg(test)]
            if let (Some(f), Some(_)) = (marked, &pending) {
                f(&held, &filter, &holding);
            }
            let result = transaction(
                &mut store,
                &committing,
                &touches,
                &swarm,
                &cohorts,
                &counters,
                &events,
                backfill_until,
                now,
                quarantine_until,
                class_gen,
            );
            #[cfg(test)]
            if let (Some(f), Some(_)) = (between, &pending) {
                f(&mut store, &held, &filter);
            }
            // Committed or rolled back: the store says which hold.
            hold_committed(&store, pending);
            (store, committing, touches, swarm, result)
        })
        .await
        .expect("writer transaction task");
        self.store = Some(store);
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                // The batch and its counters are lost; keep going. Its
                // leases stay held, so the keys aren't fetched again in
                // this run.
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                self.gate
                    .exclude(Excluded::StoreFailed, committing.len() as u64);
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
                return;
            }
        };
        self.stats.transactions.fetch_add(1, Ordering::Relaxed);
        if outcome.stale_class {
            // Its torrents are marked: enrich them in another pass.
            self.reenrich.store(true, Ordering::Release);
        }
        self.stats
            .last_commit
            .store(unix_now() as u64, Ordering::Relaxed);
        // The have-filter learns the keys before their leases end, so a
        // rediscovery in between is held, never claimed and fetched again.
        for (_, _, _, hashes) in &outcome.stored {
            for h in hashes {
                self.have.insert(h);
            }
        }
        let committed_at = unix_now();
        for (i, id, new, _) in outcome.stored {
            let c = &committing[i];
            // The gate counts what the store says, whether or not the
            // lease is still current below: a new row is new once, and a
            // second delivery of it reads as already stored.
            // A refetch that stores a new row (the imported row was deleted
            // meanwhile) is a recovery, never the network's (B-refetch-funnel).
            let crawled = crate::in_funnel(c.lease.cohort);
            if new {
                let origin = if crawled {
                    Origin::Network
                } else {
                    Origin::Recovery
                };
                self.gate
                    .committed_new(origin, id, c.lease.discovered_at, committed_at);
            } else {
                self.gate.exclude(Excluded::AlreadyStored, 1);
            }
            // A new torrent was last seen when it was discovered, as
            // `torrents.last_seen_at` says: `seen:`, the seen sort and the
            // freshness prior read the hot column, not the row.
            if let (true, Some(hot)) = (new, &self.hot) {
                let at = c.lease.discovered_at.clamp(0, i64::from(u32::MAX - 1));
                hot.touch(id as u64, at as u32);
            }
            let Some(lease) = self.coordinator.complete(&c.results.key, c.results.token) else {
                continue;
            };
            if crawled {
                self.funnel
                    .record(lease.cohort, lease.source, Stage::Stored, None);
            }
            if new {
                self.stats.stored.fetch_add(1, Ordering::Relaxed);
                // At the fetch's time, as `torrents.metadata_at` holds it,
                // so `rollup --repair` recounts the same hours (B11b).
                if crawled {
                    self.rollups
                        .add(c.results.metadata_at, metric::STORED, lease.source, 0, 1);
                }
            } else {
                self.stats.existing.fetch_add(1, Ordering::Relaxed);
            }
        }
        for i in outcome.blocked {
            let c = &committing[i];
            let (counter, excluded) = match c.verdict.as_ref().map(|h| h.action) {
                Some(Action::Quarantine) => (&self.stats.quarantined, Excluded::Quarantined),
                _ => (&self.stats.blocked, Excluded::Filtered),
            };
            counter.fetch_add(1, Ordering::Relaxed);
            self.gate.exclude(excluded, 1);
            self.coordinator
                .lost(&c.results.key, c.results.token, Loss::Blocked);
        }
        if let Some(hot) = &self.hot {
            for (u, stored) in swarm.iter().zip(&outcome.swarm) {
                if let (true, Some(h)) = (*stored, u.hot()) {
                    let id = u.torrent_id as u64;
                    hot.set_swarm(id, h);
                    let suspect = u.flags & swarm_flags::SUSPECT != 0;
                    hot.set_flags(id, hot_flags::SUSPECT, suspect);
                }
            }
        }
        self.stats.swarm_updates.fetch_add(
            outcome.swarm.iter().filter(|s| **s).count() as u64,
            Ordering::Relaxed,
        );
        for (d, id) in touches.into_iter().zip(outcome.touched) {
            if let Some(id) = id {
                if let Some(hot) = &self.hot {
                    hot.touch(id as u64, d.at.clamp(0, i64::from(u32::MAX - 1)) as u32);
                }
                if let Some(health) = &self.health {
                    health.rediscovered(id);
                }
                self.stats.rediscovered.fetch_add(1, Ordering::Relaxed);
                self.gate.exclude(Excluded::Rediscovered, 1);
                self.rollups.add(
                    d.at,
                    metric::REDISCOVERED,
                    d.source,
                    i64::from(d.found.strategy),
                    1,
                );
            } else {
                self.stats.false_positives.fetch_add(1, Ordering::Relaxed);
                if retry.try_send(d.clone()).is_err() {
                    self.rollups.add(d.at, metric::DROPPED, d.source, 0, 1);
                }
            }
        }
        if self.have.is_full() {
            self.rebuild_have().await;
        }
    }

    /// Enriches the next batch of stored torrents that lack an up-to-date
    /// enrichment, and queues them for re-indexing. One pass per start:
    /// torrents arriving through the pipeline are enriched in prepare.
    async fn backfill(&mut self) {
        let store = self.store.take().expect("store");
        let after = self.backfill_after;
        let filter = self.filter.clone();
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let held = self.held.clone();
        #[cfg(test)]
        let (between, marked) = (self.between, self.marked);
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let ctx = crate::enrich::Backfill {
                filter: Some(&filter),
                sealer: sealer.as_deref(),
                quarantine_secs,
            };
            let pending = std::cell::RefCell::new(None);
            let mark = |hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(&held, &filter, hs);
                }
            };
            let result = crate::enrich::enrich_stored_marking(
                &mut store,
                after,
                BACKFILL_BATCH,
                &ctx,
                &mark,
            );
            let pending = pending.into_inner();
            #[cfg(test)]
            if let (Some(f), Some(_)) = (between, &pending) {
                f(&mut store, &held, &filter);
            }
            // Committed, rejected or failed: the store says which hold.
            hold_committed(&store, pending);
            (store, result)
        })
        .await
        .expect("backfill task");
        self.store = Some(store);
        match result {
            // The rules changed under the batch: the same batch again.
            Ok(Some(b)) if b.rejected => {}
            Ok(Some(b)) => {
                self.backfill_after = b.last;
                self.stats
                    .enriched
                    .fetch_add(b.enriched as u64, Ordering::Relaxed);
            }
            Ok(None) => self.backfill_done = true,
            Err(e) => {
                // Tried again at the next start.
                self.backfill_done = true;
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::ENRICHMENT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// When the rules changed since the candidate sweep started (or
    /// finished), starts it again from the first candidate.
    fn note_engine(&mut self) {
        let version = self.filter.current().version;
        if self.candidates.version != Some(version) {
            self.candidates = CandidateSweep {
                version: Some(version),
                ..CandidateSweep::default()
            };
        }
    }

    /// Judges the next batch of waiting import candidates under the
    /// current rules, and holds back those a rule now matches.
    async fn refile_candidates(&mut self) {
        let store = self.store.take().expect("store");
        let filter = self.filter.clone();
        let version = *self
            .candidates
            .version
            .get_or_insert_with(|| filter.current().version);
        let after = self.candidates.after;
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let held = self.held.clone();
        #[cfg(test)]
        let (between, marked) = (self.between, self.marked);
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let ctx = crate::enrich::Backfill {
                filter: None,
                sealer: sealer.as_deref(),
                quarantine_secs,
            };
            let pending = std::cell::RefCell::new(None);
            let mark = |hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(&held, &filter, hs);
                }
            };
            let result = crate::enrich::refile_candidates_marking(
                &mut store,
                &filter,
                version,
                after,
                REFILE_BATCH,
                &ctx,
                &mark,
            );
            let pending = pending.into_inner();
            #[cfg(test)]
            if let (Some(f), Some(_)) = (between, &pending) {
                f(&mut store, &held, &filter);
            }
            // Committed, rejected or failed: the store says which hold.
            hold_committed(&store, pending);
            (store, result)
        })
        .await
        .expect("candidate refile task");
        self.store = Some(store);
        match result {
            // The rules changed: all of them again, under the new ones.
            Ok(Some(b)) if b.rejected => {
                self.candidates = CandidateSweep {
                    version: Some(self.filter.current().version),
                    ..CandidateSweep::default()
                };
            }
            Ok(Some(b)) => {
                self.candidates.after = b.last;
                self.candidates.failures = 0;
            }
            Ok(None) => {
                self.candidates.done = true;
                self.candidates.failures = 0;
            }
            Err(e) => {
                // The same batch again after a backoff, so names the
                // current rules hold don't stay at rest until the next
                // rule change or start. The error carries no name.
                self.candidates.failed(unix_now());
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::ENRICHMENT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// Rebuilds the have-filter at twice the size from the store.
    async fn rebuild_have(&mut self) {
        let store = self.store.take().expect("store");
        let capacity = capacity_for(self.have.len());
        let (store, result) = tokio::task::spawn_blocking(move || {
            let bloom = Bloom::new(capacity);
            let result = store
                .for_each_swarm_hash(|h| bloom.insert(&h))
                .map(|()| bloom);
            (store, result)
        })
        .await
        .expect("have rebuild task");
        self.store = Some(store);
        match result {
            Ok(bloom) => {
                self.have.replace(bloom);
                self.stats.filter_rebuilds.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => lodestar_log::log_event!(
                crate::log_events::HAVE_REBUILD_FAILED,
                error = crate::log_events::store_error(&e)
            ),
        }
    }

    async fn save_queue(&mut self, queue: Vec<QueuedKey>) {
        let mut store = self.store.take().expect("store");
        let (store, result) = tokio::task::spawn_blocking(move || {
            let result = store.save_fetch_queue(&queue);
            (store, result)
        })
        .await
        .expect("queue save task");
        self.store = Some(store);
        if let Err(e) = result {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            lodestar_log::log_event!(
                crate::log_events::QUEUE_SAVE_FAILED,
                error = crate::log_events::store_error(&e)
            );
        }
    }
}

/// Judges a torrent again at commit (a remote worker's, or one prepared
/// under an older rule set), and stores what the classification rules say
/// now if they changed since prepare (§20.9).
pub(crate) fn rejudge(engine: &Engine, r: &mut Results, discovered_at: i64, now: i64) -> Verdict {
    let hashes: Vec<[u8; 20]> = r.torrent.swarm_hashes().map(|h| h.0).collect();
    let facts = Facts {
        class: &r.class,
        size: r.torrent.size,
        piece_len: r.torrent.piece_len,
        flags: r.torrent.flags,
        discovered_at,
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: &r.torrent.name,
        files: &r.files,
        content_type: r.class.content_type,
    };
    let (verdict, says) = engine.judge_classified(&item, Some(&facts), now);
    let fix = crate::enrich::rule_fix(says, &r.class);
    if fix != r.fix {
        r.torrent.enrichment = Some(crate::enrich::enrich_with_rule(
            &r.torrent.name,
            &r.files,
            fix,
        ));
        r.fix = fix;
    }
    verdict
}

/// Torrents enriched per background step.
const BACKFILL_BATCH: usize = 256;

/// Import candidates judged per refile step: one short transaction each.
const REFILE_BATCH: usize = 512;
/// The wait after a refile sweep's first store error in a row, doubling
/// after each further one up to the cap (seconds).
const REFILE_RETRY_FIRST: i64 = 5;
const REFILE_RETRY_MAX: i64 = 300;

/// One store transaction (runs on the blocking pool).
#[allow(clippy::too_many_arguments)]
fn transaction(
    store: &mut Store,
    committing: &[Committing],
    touches: &[Discovery],
    swarm: &[SwarmUpdate],
    cohorts: &[CohortCount],
    counters: &[Counter],
    events: &[Event],
    backfill_until: i64,
    now: i64,
    quarantine_until: i64,
    class_gen: Option<i64>,
) -> Result<Outcome, lodestar_store::Error> {
    // Filtered torrents are never stored: a drop leaves a tombstone, a
    // quarantine a sealed name (§20.3). Other actions (from rules that
    // don't hold torrents back) don't stop storage.
    let held = |c: &Committing| match c.verdict.as_ref().map(|h| h.action) {
        Some(Action::Drop) => Some(Held::Tombstone),
        Some(Action::Quarantine) => Some(Held::Quarantine),
        _ => None,
    };
    let (blocked, accepted): (Vec<usize>, Vec<usize>) =
        (0..committing.len()).partition(|&i| held(&committing[i]).is_some());
    // One hit per decision (held, marked or shadow), for the review
    // queue and the replay (§20.7).
    let records: Vec<(usize, &Decision, bool)> = committing
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            c.verdict
                .iter()
                .map(move |d| (i, d, false))
                .chain(c.shadow.iter().map(move |d| (i, d, true)))
        })
        .collect();
    let hashes: Vec<Vec<[u8; 20]>> = committing
        .iter()
        .map(|c| c.results.torrent.swarm_hashes().map(|h| h.0).collect())
        .collect();
    let owned: Vec<(String, Vec<u8>)> = records
        .iter()
        .map(|(_, d, _)| (d.rule.id(), d.evidence_codes()))
        .collect();
    let filtered: Vec<Filtered<'_>> = records
        .iter()
        .zip(&owned)
        .map(|(&(i, d, shadow), (rule, evidence))| {
            let c = &committing[i];
            Filtered {
                hashes: &hashes[i],
                rule,
                action: d.action.code() | if shadow { SHADOW } else { 0 },
                held: if shadow { None } else { held(c) },
                sealed: c.sealed.as_deref().filter(|_| !shadow),
                evidence,
                at: now,
                expires_at: quarantine_until,
                // Held by the Adult-only core only because your rule made
                // it adult: a tombstone when the quarantine ends (§20.9).
                adult_hold: !shadow
                    && c.results.fix == Some(true)
                    && lodestar_filter::reclassify::adult_hold(d),
            }
        })
        .collect();
    let prepared: Vec<PreparedTorrent<'_>> = accepted
        .iter()
        .map(|&i| {
            let Committing { results, lease, .. } = &committing[i];
            PreparedTorrent {
                torrent: &results.torrent,
                discovered_at: lease.discovered_at,
                metadata_at: results.metadata_at,
                extra_flags: (if lease.discovered_at < backfill_until {
                    flags::BACKFILL
                } else {
                    0
                }) | if results.private_source {
                    flags::PRIVATE_SOURCE
                } else {
                    0
                } | marks(committing[i].verdict.as_ref())
                    // A refetch's row is an import's, even when it comes
                    // back new: kept out of the crawl's cohorts and yield.
                    | if crate::in_funnel(lease.cohort) {
                        0
                    } else {
                        flags::IMPORTED
                    },
                source: lease.source,
                verified: true,
            }
        })
        .collect();
    // Other sources that reported a key while it was leased, then the
    // rediscoveries.
    let mut all_touches: Vec<Touch> = accepted
        .iter()
        .map(|&i| &committing[i])
        .map(|c| {
            (
                &c.results,
                c.lease.sources | c.results.sources,
                c.lease.source,
            )
        })
        .filter(|&(_, sources, first)| sources != bit(first))
        .map(|(r, sources, _)| Touch {
            hash: r.key,
            sources,
            at: r.metadata_at,
            public: !r.private_source,
        })
        .collect();
    let extra = all_touches.len();
    all_touches.extend(touches.iter().map(|d| Touch {
        hash: d.found.key,
        sources: bit(d.source),
        at: d.at,
        public: !d.found.private_source,
    }));
    let written = store.write(&WriteBatch {
        prepared: &prepared,
        touches: &all_touches,
        swarm,
        counters,
        cohorts,
        events,
        filtered: &filtered,
        class_gen,
        ..WriteBatch::default()
    })?;
    let stored = accepted
        .iter()
        .zip(&written.prepared)
        .map(|(&i, ins)| {
            (
                i,
                ins.id(),
                matches!(ins, Inserted::New(_)),
                committing[i].results.torrent.swarm_hashes().collect(),
            )
        })
        .collect();
    Ok(Outcome {
        stored,
        blocked,
        stale_class: written.stale_class,
        touched: written.touched[extra..].to_vec(),
        swarm: written.swarm,
    })
}

/// The flag bits for a rule that keeps a torrent but changes how it's
/// shown (§20.3).
fn marks(decision: Option<&Decision>) -> i64 {
    match decision.map(|d| d.action) {
        Some(Action::Hide) => flags::HIDDEN,
        Some(Action::Demote) => flags::DEMOTED,
        Some(Action::Flag) => flags::FLAGGED,
        _ => 0,
    }
}

fn bit(source: SourceId) -> u64 {
    1u64 << source.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_bencode::{OwnedValue, to_vec};
    use lodestar_classify::{ContentType, Record, classify};
    use lodestar_cluster::Token;
    use lodestar_filter::{Core, CoreRule, CsList, Lists, RuleRef, Switches, TermSet};
    use lodestar_metainfo::Info;
    use lodestar_store::{Store, StoreConfig};

    use crate::gate::GateStats;

    /// A child-role and a sexual stand-in ten words apart: only the
    /// Adult-only CS-5a (window 12) can catch it, not CS-4a (window 8).
    const NAME: &str = "Studiox Rolea one two three four five six seven eight nine Lexa";

    fn engine() -> Engine {
        let lists = Lists {
            sexual: TermSet::of(&["lexa"]),
            child_roles: TermSet::of(&["rolea"]),
            codes: TermSet::of(&["codex"]),
        };
        Engine::new(Core::new(lists, CsList::default(), Switches::default()))
    }

    fn results() -> Results {
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(format!("{NAME}.mp4").into_bytes()),
                ),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = Info::parse(&raw).unwrap();
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut torrent = store.preparer().prepare(&info, None).unwrap();
        let files = crate::enrich::file_infos(&info.files, info.path_encoding);
        let (enriched, class) = crate::enrich::enrich_infos(&torrent.name, &files);
        torrent.enrichment = Some(enriched);
        Results {
            key: info.primary_hash(),
            token: Token(1),
            worker: WorkerId(7),
            torrent,
            sources: 1,
            metadata_at: 0,
            files,
            class,
            verdict: Verdict::default(),
            version: 0,
            fix: None,
            private_source: false,
        }
    }

    fn writer(store: Store, coordinator: Arc<Coordinator>, gate: Arc<GateStats>) -> Writer {
        Writer {
            store: Some(store),
            backfill_after: 0,
            backfill_done: true,
            reenrich: Arc::default(),
            cfg: WriterConfig {
                flush_every: Duration::from_secs(60),
                batch_max: 8,
                batch_wait: Duration::from_millis(1),
                backfill_until: 0,
                quarantine_secs: 86_400,
                hits_secs: 86_400,
            },
            have: Arc::new(Have::new(1024)),
            funnel: Arc::default(),
            rollups: Arc::default(),
            filter: Arc::new(Handle::new(engine())),
            sealer: None,
            held: Arc::default(),
            last_purge: 0,
            coordinator,
            stats: Arc::default(),
            gate,
            hot: None,
            health: None,
            candidates: Default::default(),
            between: None,
            marked: None,
        }
    }

    /// Leases `r`'s key to its worker and gives it the token.
    fn leased(c: &Coordinator, mut r: Results) -> Results {
        let claim = lodestar_cluster::Claim {
            key: r.key,
            source: SourceId::new(1).unwrap(),
            strategy: 0,
            at: 0,
        };
        let lodestar_cluster::Claimed::Granted(token) = c.claim(r.worker, claim, 0) else {
            panic!("granted")
        };
        r.token = token;
        r
    }

    /// The gate at the writer's commit: new once, then already stored; a
    /// stale lease is a duplicate; a failed transaction loses its batch.
    #[tokio::test]
    async fn the_writer_commit_counts_each_gate_outcome() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut w = writer(store, coordinator.clone(), gate.clone());
        let (retry, _retry_rx) = mpsc::channel(4);
        let count = |g: &GateStats| {
            (
                g.new_verified(Origin::Network),
                g.excluded(Excluded::AlreadyStored),
                g.excluded(Excluded::Duplicate),
                g.excluded(Excluded::StoreFailed),
            )
        };

        w.commit(
            vec![leased(&coordinator, results())],
            vec![],
            vec![],
            vec![],
            &retry,
        )
        .await;
        assert_eq!(count(&gate), (1, 0, 0, 0));
        w.commit(
            vec![leased(&coordinator, results())],
            vec![],
            vec![],
            vec![],
            &retry,
        )
        .await;
        assert_eq!(count(&gate), (1, 1, 0, 0));
        // Under a token that was never current.
        w.commit(vec![results()], vec![], vec![], vec![], &retry)
            .await;
        assert_eq!(count(&gate), (1, 1, 1, 0));

        let mut r = results();
        r.key = lodestar_metainfo::InfoHash([9; 20]);
        let r = leased(&coordinator, r);
        w.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        w.commit(vec![r], vec![], vec![], vec![], &retry).await;
        assert_eq!(count(&gate), (1, 1, 1, 1));
        assert_eq!(w.stats.errors.load(Ordering::Relaxed), 1);
        assert_eq!(w.stats.stored.load(Ordering::Relaxed), 1);
    }

    /// A refetch is never the network's and never the funnel's: one that
    /// stores a new row (its imported row was deleted meanwhile) is a
    /// recovery, kept an import, with no Stored or `STORED`; one whose row
    /// is there is already stored (B-refetch-funnel).
    #[tokio::test]
    async fn a_refetch_commit_is_a_recovery_and_not_the_funnels() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let gate = Arc::new(GateStats::new(0));
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut w = writer(store, coordinator.clone(), gate.clone());
        let (retry, _retry_rx) = mpsc::channel(4);
        let refetch = |c: &Coordinator| {
            let mut r = results();
            r.token = c.restore(
                r.worker,
                r.key,
                lodestar_cluster::RestoredLease {
                    cohort: crate::REFETCH_COHORT,
                    source: SourceId::new(1).unwrap(),
                    sources: 0b10,
                    strategy: crate::strategy::REFETCH,
                    discovered_at: 3600,
                },
                0,
            );
            r
        };
        w.commit(vec![refetch(&coordinator)], vec![], vec![], vec![], &retry)
            .await;
        assert_eq!(gate.new_verified(Origin::Recovery), 1);
        assert_eq!(gate.new_verified(Origin::Network), 0);
        w.commit(vec![refetch(&coordinator)], vec![], vec![], vec![], &retry)
            .await;
        assert_eq!(gate.excluded(Excluded::AlreadyStored), 1);
        assert_eq!(gate.new_verified(Origin::Recovery), 1);
        assert!(funnel.drain().is_empty(), "no funnel row");
        assert!(w.rollups.drain().is_empty(), "no STORED");
        let store = w.store.as_mut().unwrap();
        let id = store.find(&results().key).unwrap().unwrap();
        let row = store.torrent(id).unwrap().unwrap();
        assert_ne!(row.flags & flags::IMPORTED, 0);
    }

    fn stored_type(r: &Results) -> ContentType {
        let e = r.torrent.enrichment.as_ref().unwrap();
        Record::decode(&e.data).unwrap().content_type
    }

    #[test]
    fn rejudging_applies_an_is_adult_rule_and_the_adult_only_core() {
        let mut r = results();
        let detected = classify(&r.torrent.name, &r.files).content_type;
        assert_ne!(detected, ContentType::Adult);
        let mut e = engine();
        assert_eq!(rejudge(&e, &mut r, 0, 1000).decision, None);
        assert_eq!((r.fix, stored_type(&r)), (None, detected));

        // A rule added since prepare: stored as adult, and held by CS-5a.
        e.load_class_rules(&[(1, "studiox".into(), true)]);
        let d = rejudge(&e, &mut r, 0, 1000).decision.expect("held");
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs5a));
        assert_eq!(d.action, Action::Quarantine);
        assert_eq!((r.fix, stored_type(&r)), (Some(true), ContentType::Adult));

        // And removed again: back to detection's type.
        e.load_class_rules(&[]);
        assert_eq!(rejudge(&e, &mut r, 0, 1000).decision, None);
        assert_eq!((r.fix, stored_type(&r)), (None, detected));
    }

    /// The writer's sweep holds back a waiting import candidate the core
    /// now matches, and intake refuses its hash from then on.
    #[tokio::test]
    async fn the_writer_refiles_waiting_import_candidates() {
        use lodestar_store::import::{
            Decision as Pass, ImportBatch, ImportCandidate, ImportRecord, NewImportJob, Phase,
            Privacy,
        };
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = store.register_source("magnetico", "1", 1).unwrap();
        let job = store
            .create_import_job(&NewImportJob {
                adapter: "magnetico",
                adapter_ver: 1,
                dataset: b"ds",
                schema_sig: b"sig",
                options: b"",
                credential: None,
                at: 1,
            })
            .unwrap();
        let job = store
            .set_import_phase(job.id, job.revision, Phase::Reading, 1)
            .unwrap();
        let record = |h: u8, name: &'static [u8]| {
            ImportRecord::Candidate(ImportCandidate {
                hash: InfoHash([h; 20]),
                hash_v2: None,
                name: Some(name),
                size: None,
                privacy: Privacy::Unknown,
                source_at: None,
                decision: Pass {
                    hits: &[],
                    marks: 0,
                },
            })
        };
        store
            .commit_import_batch(&ImportBatch {
                job: job.id,
                seq: 1,
                position: b"",
                source: src,
                source_bytes: 0,
                at: 1,
                class_gen: None,
                records: &[record(1, b"Rolea Lexa clip"), record(2, b"Plain Film")],
            })
            .unwrap();
        capture::install();
        let mut w = writer(store, coordinator, gate);
        async fn sweep(w: &mut Writer) {
            while w.candidates.ready() {
                w.refile_candidates().await;
            }
        }
        sweep(&mut w).await;
        assert!(w.held.contains(&[1; 20]));
        assert!(!w.held.contains(&[2; 20]));
        let open = |w: &Writer| {
            let store = w.store.as_ref().unwrap();
            let job = store.import_job(job.id).unwrap().unwrap();
            assert!(job.counters.conserved());
            job.open_candidates
        };
        assert_eq!(open(&w), 1);

        // A rule added after the sweep finished: the flush tick starts it
        // again, and the plain candidate goes too.
        w.note_engine();
        assert!(w.candidates.done, "same rules, no new sweep");
        w.filter.update_engine(|e| {
            let notes = e.load_rules(&lodestar_store::RuleSet {
                rules: vec![lodestar_store::RuleRow {
                    id: 1,
                    query: "plain".into(),
                    action: Action::Drop.code(),
                    enabled: true,
                    shadow_until: None,
                    unless_query: None,
                }],
                ..Default::default()
            });
            assert!(notes.is_empty(), "{notes:?}");
        });
        w.note_engine();
        assert!(!w.candidates.done);
        sweep(&mut w).await;
        assert!(w.held.contains(&[2; 20]));
        assert_eq!(open(&w), 0);

        // A batch under rules that changed since is refused, and the
        // sweep starts over under the current ones.
        let current = w.filter.current().version;
        w.candidates = CandidateSweep {
            version: Some(current - 1),
            after: 7,
            ..CandidateSweep::default()
        };
        w.refile_candidates().await;
        assert_eq!(
            (w.candidates.version, w.candidates.after, w.candidates.done),
            (Some(current), 0, false)
        );

        // A store error: the sweep waits, then tries again on a later
        // tick, and isn't marked done. A temporary table shadows the
        // candidates with an unreadable row.
        let conn = |w: &Writer, sql: &str| {
            w.store
                .as_ref()
                .unwrap()
                .connection()
                .execute_batch(sql)
                .unwrap();
        };
        conn(
            &w,
            "CREATE TEMP TABLE import_candidates
                 (id INTEGER, hash BLOB, hash_v2 BLOB, name BLOB, size INTEGER, source_at INTEGER);
             INSERT INTO temp.import_candidates VALUES (1, x'00', NULL, NULL, NULL, NULL);",
        );
        w.candidates = CandidateSweep {
            version: Some(current),
            ..CandidateSweep::default()
        };
        let errors = w.stats.errors.load(Ordering::Relaxed);
        let before = unix_now();
        w.refile_candidates().await;
        assert_eq!(w.stats.errors.load(Ordering::Relaxed), errors + 1);
        let at = w.candidates.retry_at.expect("waits after an error");
        assert!(at >= before + REFILE_RETRY_FIRST, "{at} {before}");
        assert!(!w.candidates.done && !w.candidates.ready());
        w.candidates.retry_due(at - 1);
        assert!(!w.candidates.ready(), "not before it's due");
        w.candidates.retry_due(at);
        assert!(w.candidates.ready());
        let before = unix_now();
        w.refile_candidates().await;
        let again = w.candidates.retry_at.expect("waits longer");
        assert!(again >= before + 2 * REFILE_RETRY_FIRST, "{again} {before}");
        conn(&w, "DROP TABLE temp.import_candidates;");
        w.candidates.retry_due(again);
        sweep(&mut w).await;
        assert!(w.candidates.done && w.candidates.failures == 0);

        // The sink is live: the error's event reached it (so the checks
        // below would see a name that reached one).
        assert!(!capture::lines("pipeline.enrichment_failed").is_empty());
        // Nothing the sweep logged carries a candidate's name.
        for needle in ["Rolea", "Lexa", "Plain Film"] {
            assert_eq!(capture::lines(needle), Vec::<String>::new());
        }
    }

    /// A writer whose store has one waiting import candidate the core
    /// holds ([1; 20]) and one stored torrent an Is adult rule makes CS-5a
    /// hold, for the two arms of `hold_committed`.
    fn arms_writer() -> (Writer, [u8; 20]) {
        use lodestar_store::NewTorrent;
        use lodestar_store::import::{
            Decision as Pass, ImportBatch, ImportCandidate, ImportRecord, NewImportJob, Phase,
            Privacy,
        };
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = store.register_source("magnetico", "1", 1).unwrap();
        let job = store
            .create_import_job(&NewImportJob {
                adapter: "magnetico",
                adapter_ver: 1,
                dataset: b"ds",
                schema_sig: b"sig",
                options: b"",
                credential: None,
                at: 1,
            })
            .unwrap();
        let job = store
            .set_import_phase(job.id, job.revision, Phase::Reading, 1)
            .unwrap();
        store
            .commit_import_batch(&ImportBatch {
                job: job.id,
                seq: 1,
                position: b"",
                source: src,
                source_bytes: 0,
                at: 1,
                class_gen: None,
                records: &[ImportRecord::Candidate(ImportCandidate {
                    hash: InfoHash([1; 20]),
                    hash_v2: None,
                    name: Some(b"Rolea Lexa clip"),
                    size: None,
                    privacy: Privacy::Unknown,
                    source_at: None,
                    decision: Pass {
                        hits: &[],
                        marks: 0,
                    },
                })],
            })
            .unwrap();
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(format!("{NAME}.mp4").into_bytes()),
                ),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = Info::parse(&raw).unwrap();
        let id = store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: src,
            }])
            .unwrap()[0]
            .id();
        store.mark_unenriched(&[id]).unwrap();
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        w.filter
            .update_engine(|e| e.load_class_rules(&[(1, "studiox".into(), true)]));
        w.backfill_done = false;
        (w, info.primary_hash().0)
    }

    /// Runs both arms once each: the sweep, then backfill.
    async fn run_arms(w: &mut Writer) {
        w.refile_candidates().await;
        w.backfill().await;
    }

    /// Each arm alone on a fresh writer, `between` landing after its
    /// commit; returns the writer and the hash that arm held.
    async fn each_arm(between: fn(&mut Store, &HeldSet, &Handle)) -> Vec<(Writer, [u8; 20])> {
        let mut out = Vec::new();
        for sweep in [true, false] {
            let (mut w, stored) = arms_writer();
            w.between = Some(between);
            if sweep {
                w.refile_candidates().await;
                out.push((w, [1; 20]));
            } else {
                w.backfill().await;
                out.push((w, stored));
            }
        }
        out
    }

    /// What intake refuses is what the store holds, as a restart would
    /// load it.
    fn agrees_with_store(w: &Writer) {
        let store = w.store.as_ref().unwrap();
        let held: std::collections::HashSet<_> = store
            .filter_state()
            .unwrap()
            .held
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(w.held.snapshot(), held);
    }

    /// A release landing between a batch's commit and its hold check
    /// isn't undone, in both arms: the hash stays fetchable.
    #[tokio::test]
    async fn a_release_after_the_commit_stays_released() {
        fn release_all(store: &mut Store, held: &HeldSet, _: &Handle) {
            let hashes: Vec<_> = store
                .filter_state()
                .unwrap()
                .held
                .into_iter()
                .map(|(h, _)| h)
                .collect();
            assert!(!hashes.is_empty(), "the batch committed a hold");
            store.release_many(&hashes).unwrap();
            held.remove_released(&hashes, |hs| store.held_among(hs))
                .unwrap();
        }
        for (w, h) in each_arm(release_all).await {
            assert!(!w.held.contains(&h), "released");
            agrees_with_store(&w);
        }
    }

    /// A rule change that releases nothing (an edited class rule, a
    /// narrowed rule) landing there leaves both arms' holds held: the
    /// store still holds them, whatever the rules say now.
    #[tokio::test]
    async fn a_rule_change_after_the_commit_keeps_the_hold() {
        fn change_rules(_: &mut Store, _: &HeldSet, filter: &Handle) {
            filter.update_engine(|e| {
                e.load_class_rules(&[(1, "studioy".into(), true)]);
                let notes = e.load_rules(&lodestar_store::RuleSet {
                    rules: vec![lodestar_store::RuleRow {
                        id: 1,
                        query: "rolea lexa nothing".into(),
                        action: Action::Drop.code(),
                        enabled: true,
                        shadow_until: None,
                        unless_query: Some("clip".into()),
                    }],
                    ..Default::default()
                });
                assert!(notes.is_empty(), "{notes:?}");
            });
        }
        for (w, h) in each_arm(change_rules).await {
            assert!(w.held.contains(&h), "still held");
            agrees_with_store(&w);
        }
    }

    /// The hole before this slice: a release deletes its row (S), a batch
    /// holds the hash again and inserts it (C, I), and only then does the
    /// release update the set (M). M asks the store, finds the new hold
    /// and keeps it. In the reverse order (S, M, C, I) the batch's insert
    /// comes last. Either way, in both arms, the set ends as the store has
    /// it: held.
    #[tokio::test]
    async fn a_release_racing_a_new_hold_keeps_it_held() {
        // M between the commit and the writer's check (S, C, M, Q+I): it
        // finds the new hold and keeps it, then the writer inserts it.
        fn release_held(store: &mut Store, held: &HeldSet, _: &Handle) {
            let hashes: Vec<_> = store
                .filter_state()
                .unwrap()
                .held
                .into_iter()
                .map(|(h, _)| h)
                .collect();
            held.remove_released(&hashes, |hs| store.held_among(hs))
                .unwrap();
        }
        for sweep in [true, false] {
            let (mut w, stored) = arms_writer();
            let h = if sweep { [1; 20] } else { stored };
            w.held.insert(h);
            w.between = Some(release_held);
            if sweep {
                w.refile_candidates().await;
            } else {
                w.backfill().await;
            }
            assert!(w.held.contains(&h), "S, C, M, Q+I; sweep {sweep}");
            agrees_with_store(&w);
        }
        for release_last in [true, false] {
            for sweep in [true, false] {
                let (mut w, stored) = arms_writer();
                let h = if sweep { [1; 20] } else { stored };
                // S has run: the store holds nothing, the set still has h.
                w.held.insert(h);
                let release = |w: &Writer| {
                    let store = w.store.as_ref().unwrap();
                    w.held
                        .remove_released(&[h], |hs| store.held_among(hs))
                        .unwrap();
                };
                if !release_last {
                    release(&w);
                    assert!(!w.held.contains(&h), "M ran before the hold");
                }
                if sweep {
                    w.refile_candidates().await;
                } else {
                    w.backfill().await;
                }
                if release_last {
                    release(&w);
                }
                assert!(
                    w.held.contains(&h),
                    "release_last {release_last}, sweep {sweep}"
                );
                agrees_with_store(&w);
            }
        }
    }

    /// Intake refuses a batch's hashes from its pre-commit mark to its
    /// reconcile, in all three writer arms: a release reconciling before
    /// the commit (M, reading the store as not holding them) can't make
    /// them fetchable, and after the reconcile the set equals the store's
    /// with no mark left (`2026-10-31-B-held-precommit.md`).
    #[tokio::test]
    async fn a_new_hold_is_refused_from_before_its_commit() {
        use std::sync::atomic::AtomicUsize;
        static HOOKS: AtomicUsize = AtomicUsize::new(0);
        fn marked(held: &HeldSet, _: &Handle, hs: &[[u8; 20]]) {
            HOOKS.fetch_add(1, Ordering::Relaxed);
            assert!(!hs.is_empty() && held.pending_len() > 0);
            // M before C: the store doesn't hold them yet.
            held.remove_released::<()>(hs, |c| Ok(vec![false; c.len()]))
                .unwrap();
            for h in hs {
                assert!(held.contains(h), "pending, so refused");
                assert!(!held.snapshot().contains(h), "M removed the old entry");
            }
        }
        fn between(store: &mut Store, held: &HeldSet, _: &Handle) {
            HOOKS.fetch_add(1, Ordering::Relaxed);
            assert!(held.pending_len() > 0, "C done, Q+I not yet");
            for (h, _) in store.filter_state().unwrap().held {
                assert!(held.contains(&h), "committed, so refused");
            }
        }
        for arm in 0..3 {
            HOOKS.store(0, Ordering::Relaxed);
            let (mut w, stored) = arms_writer();
            w.marked = Some(marked);
            w.between = Some(between);
            let h = match arm {
                0 => {
                    w.held.insert([1; 20]);
                    w.refile_candidates().await;
                    [1; 20]
                }
                1 => {
                    w.held.insert(stored);
                    w.backfill().await;
                    stored
                }
                _ => {
                    // The ingest commit, of a fetch the class rule holds.
                    let funnel = Arc::new(FunnelCounts::default());
                    let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
                    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
                    w = writer(store, coordinator, Arc::new(GateStats::new(0)));
                    w.filter
                        .update_engine(|e| e.load_class_rules(&[(1, "studiox".into(), true)]));
                    w.marked = Some(marked);
                    w.between = Some(between);
                    let r = leased(&w.coordinator, results());
                    let h = r.key.0;
                    w.held.insert(h);
                    let (retry, _rx) = mpsc::channel(4);
                    w.commit(vec![r], vec![], vec![], vec![], &retry).await;
                    assert_eq!(w.stats.quarantined.load(Ordering::Relaxed), 1);
                    h
                }
            };
            assert_eq!(
                HOOKS.load(Ordering::Relaxed),
                2,
                "arm {arm}: marked before C"
            );
            assert!(w.held.contains(&h), "arm {arm}: held");
            assert_eq!(w.held.pending_len(), 0, "arm {arm}: no mark left");
            agrees_with_store(&w);
        }
    }

    /// A batch the store refuses (the rules moved between its mark and its
    /// commit) leaves no mark and no hold behind: the store holds nothing.
    #[tokio::test]
    async fn a_rolled_back_batch_leaves_nothing_refused() {
        fn change_rules(_: &HeldSet, filter: &Handle, _: &[[u8; 20]]) {
            filter.update_engine(|e| e.load_class_rules(&[(1, "studiox".into(), true)]));
        }
        let (mut w, _) = arms_writer();
        w.marked = Some(change_rules);
        w.refile_candidates().await;
        assert!(!w.held.contains(&[1; 20]), "rolled back, so fetchable");
        assert_eq!(w.held.pending_len(), 0);
        agrees_with_store(&w);
    }

    /// Without anything in between, both arms hold, and the set equals
    /// what a restart loads from the store; a purge that expires the
    /// quarantine drops it from both.
    #[tokio::test]
    async fn the_held_set_follows_the_store_through_a_purge() {
        let (mut w, stored) = arms_writer();
        // Quarantine that has expired by the purge below.
        w.cfg.quarantine_secs = -1;
        run_arms(&mut w).await;
        assert!(w.held.contains(&[1; 20]) && w.held.contains(&stored));
        agrees_with_store(&w);
        w.purge().await;
        agrees_with_store(&w);
        assert!(
            !w.held.contains(&[1; 20]),
            "the sweep's quarantine, expired"
        );
        assert!(w.held.contains(&stored), "the adult hold, now a tombstone");
    }

    /// The refile sweep's backoff doubles after each error in a row, up to
    /// its cap.
    #[test]
    fn the_refile_backoff_doubles_to_a_cap() {
        let mut c = CandidateSweep::default();
        let waits: Vec<i64> = (0..10)
            .map(|_| {
                c.failed(1_000);
                c.retry_at.unwrap() - 1_000
            })
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300, 300, 300]);
        c.failures = u32::MAX;
        c.failed(i64::MAX);
        assert_eq!(c.retry_at, Some(i64::MAX));
    }

    /// Every log event this test binary emits, rendered with every field
    /// shown (no mask), so a name that reached one would be seen.
    mod capture {
        use lodestar_log::{Mask, Record, Sink};
        use std::sync::{Mutex, Once};

        static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

        struct Capture;

        impl Sink for Capture {
            fn record(&self, r: &Record<'_>) {
                let line = format!("{} {}", r.event.id, r.text(Mask(u32::MAX), None));
                LINES.lock().unwrap_or_else(|e| e.into_inner()).push(line);
            }
        }

        static CAPTURE: Capture = Capture;

        pub(super) fn install() {
            static ONCE: Once = Once::new();
            ONCE.call_once(|| {
                let _ = lodestar_log::set_sink(&CAPTURE);
            });
        }

        pub(super) fn lines(needle: &str) -> Vec<String> {
            let needle = needle.to_lowercase();
            LINES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|l| l.to_lowercase().contains(&needle))
                .cloned()
                .collect()
        }
    }
}
