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
use lodestar_filter::packs::sp4::AtCommit;
use lodestar_filter::{
    Action, Decision, Engine, Facts, Handle, HeldSet, Item, Pending, Sealer, Verdict,
};
use lodestar_metainfo::InfoHash;
use lodestar_sources::{Discovery, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::hot::flags as hot_flags;
use lodestar_store::unverified::{Network, Promoted, RefiledUnverified};
use lodestar_store::{
    CohortCount, Counter, Event, Filtered, Held, HotColumns, Inserted, Prepared, PreparedTorrent,
    QueuedKey, SHADOW, SourceId, Store, SwarmUpdate, Touch, WriteBatch, flags, swarm_flags,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, interval_at};

use crate::coordinator::Coordinator;
use crate::gate::{Excluded, GateStats, Origin};
use crate::have::{Bloom, Have, capacity_for};
use crate::health::HealthHandle;
use crate::rollups::{Rollups, metric};
use crate::source_adult::AdultSources;
use crate::unverified::{self, UnverifiedStats};
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
    /// The unverified pipeline: claims judged, stored, held, promoted.
    pub unverified: UnverifiedStats,
    /// Torrent-cache files judged and committed (SRC-13b).
    pub torrent_cache: crate::torrent_cache::CacheStats,
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
    /// Discoveries with claims, from intake: judged, then stored as
    /// unverified items or claims (SRC-2).
    pub claims: mpsc::Receiver<unverified::Claimed>,
    /// Torrent-cache files, judged (SRC-13b): verified metadata that came
    /// through no lease, committed by [`Writer::commit_cache`].
    pub cache: mpsc::Receiver<crate::torrent_cache::ToCommit>,
}

pub(crate) struct Writer {
    pub store: Option<Store>,
    /// The sources whose every find is adult ([`crate::source_adult`]).
    pub adult: AdultSources,
    /// SP-3's rate state (RP-4): judged here, at commit, in order.
    pub flood: Flooding,
    /// The adult-enrichment sources: only they may attach an enrichment
    /// section (`unverified::Ctx::enrichers`, ENR-1b).
    pub enrichers: crate::adult_enrich::EnrichSources,
    /// The background enrichment pass: the last id it reached, and whether
    /// it has found nothing left to do.
    pub backfill_after: i64,
    pub backfill_done: bool,
    /// The backfill's wait after a store error.
    pub backfill_backoff: Backoff,
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
    /// The unverified-item and claims refile sweep (SRC-2).
    pub sweep: unverified::Sweep,
    /// The verification cursor (`sources.verify_unverified`).
    pub verify: unverified::Verify,
    /// Runs between a commit's transaction and its adult reclassification
    /// (`adult_by_source`); `true` skips that step, as a crash there would.
    #[cfg(test)]
    pub(crate) crash_before_adult: bool,
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
pub(crate) type MarkedHook = fn(&Store, &HeldSet, &Handle, &[[u8; 20]]);

/// After a batch that marked `pending` committed or rolled back: tells
/// intake to refuse those the store holds and clears the marks, asked
/// under `HeldSet`'s lock, so a release since isn't undone and a hold is
/// never missed (`2026-10-31-B-held-exact.md`,
/// `2026-10-31-B-held-precommit.md`). On a store error every hash goes in.
fn hold_committed(store: &Store, pending: Option<Pending<'_>>, ingest: bool) {
    let Some(pending) = pending else { return };
    if let Err(e) = pending.settle(|hs| store.held_among(hs)) {
        let error = crate::log_events::store_error(&e);
        if ingest {
            lodestar_log::log_event!(crate::log_events::STORE_WRITE_FAILED, error = error);
        } else {
            lodestar_log::log_event!(crate::log_events::ENRICHMENT_FAILED, error = error);
        }
    }
}

/// A store-error backoff shared by the background sweeps (the candidate
/// refile and the enrichment backfill): after an error the sweep waits,
/// longer after each error in a row, and the flush tick ends the wait.
/// It tries the same batch again, so a transient error doesn't end the
/// sweep and a persistent one doesn't spin.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    /// After a store error, when the flush tick lets it try again (Unix
    /// seconds); it waits until then.
    pub retry_at: Option<i64>,
    /// Store errors in a row.
    pub failures: u32,
}

impl Backoff {
    /// Not waiting out an error.
    pub(crate) fn ready(&self) -> bool {
        self.retry_at.is_none()
    }

    /// A store error (`SQLITE_BUSY` past the busy timeout, say).
    pub(crate) fn failed(&mut self, now: i64) {
        self.failures = self.failures.saturating_add(1);
        let backoff = RETRY_FIRST
            .saturating_mul(1 << (self.failures - 1).min(10))
            .min(RETRY_MAX);
        self.retry_at = Some(now.saturating_add(backoff));
    }

    /// A batch went through: the next error waits the shortest time.
    pub(crate) fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// Ends the wait once it's due.
    pub(crate) fn retry_due(&mut self, now: i64) {
        if self.retry_at.is_some_and(|at| now >= at) {
            self.retry_at = None;
        }
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
    /// The wait after a store error.
    pub backoff: Backoff,
}

impl CandidateSweep {
    /// Has a batch to judge now.
    fn ready(&self) -> bool {
        !self.done && self.backoff.ready()
    }
}

/// A result under a current lease, ready to commit, or verified metadata
/// from a torrent cache, which came through no lease.
struct Committing {
    results: Results,
    /// The coordinator's lease; for a cache's file, one made up for it that
    /// the coordinator never issued ([`Committing::lease_free`]).
    lease: Lease,
    /// Under a lease the coordinator issued: the funnel, the gate and the
    /// lease's end are accounted for it. `false` for a cache's file.
    leased: bool,
    /// The authoritative filter verdict, and the sealed name for a
    /// quarantine.
    verdict: Option<Decision>,
    /// What rules in shadow mode would have done (logged only).
    shadow: Vec<Decision>,
    sealed: Option<Vec<u8>>,
    /// The publisher a torrent cache's claims name for this file (SP-3);
    /// a fetched torrent's is read from its unverified item at commit.
    publisher: Option<String>,
}

impl Committing {
    /// A cache's admitted file from source `source`, found at `at`: the
    /// record a worker would hand over for a BEP 9 fetch, under a lease
    /// that names the plugin as its only finder. Never in the funnel or
    /// the gate, and never ends a lease (`leased` is `false`).
    fn lease_free(a: crate::torrent_cache::Admitted, source: SourceId, at: i64) -> Committing {
        Committing {
            results: Results {
                key: a.key,
                token: lodestar_cluster::Token(0),
                worker: WorkerId::LOCAL,
                torrent: a.torrent,
                sources: bit(source),
                metadata_at: at,
                files: a.files,
                class: a.class,
                verdict: a.verdict,
                version: a.version,
                fix: a.fix,
                // A private key is never looked up (§18.7); whether a
                // private source stored it since is read at the commit's
                // judge (`stored_private`).
                private_source: false,
                // Decided at commit (`source_adult`): the item it promotes,
                // or a stored torrent under its hash.
                source_adult: false,
            },
            lease: Lease {
                holder: WorkerId::LOCAL,
                token: lodestar_cluster::Token(0),
                cohort: at.div_euclid(3600),
                source,
                sources: bit(source),
                strategy: 0,
                discovered_at: at,
            },
            leased: false,
            verdict: None,
            shadow: Vec::new(),
            sealed: None,
            publisher: None,
        }
    }
}

/// What a transaction did, for the async side to account.
struct Outcome {
    /// Per stored or already-stored result: (index, new?, swarm hashes).
    stored: Vec<(usize, i64, bool, Vec<InfoHash>)>,
    /// Promoted results, by index, with the cohort and source their
    /// torrent row records (the item's first sighting and finder): the
    /// funnel's Stored stage goes there, where Insights counts it
    /// searchable (`Store::promote`).
    promoted_as: Vec<(usize, i64, SourceId)>,
    /// The batch's write failed after its promotions committed: only
    /// those (and promotion holds) are in `stored` and `blocked`.
    failed: Option<lodestar_store::Error>,
    /// Results the authoritative blocklist rejected.
    blocked: Vec<usize>,
    /// Results the store refused: every source that found them had
    /// stopped (`Inserted::Refused`, SRC-14b). Never stored.
    refused: Vec<usize>,
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
        self.arm_adult_mark();
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
                Some(first) = io.claims.recv() => {
                    // Up to `batch_max` discoveries and `CLAIMS_STEP_KIB`
                    // of claims, so one step stays short.
                    let mut kib = first.1.num_permits();
                    let mut batch = vec![first];
                    while batch.len() < self.cfg.batch_max
                        && kib < unverified::CLAIMS_STEP_KIB as usize
                    {
                        match io.claims.try_recv() {
                            Ok(d) => {
                                kib += d.1.num_permits();
                                batch.push(d);
                            }
                            Err(_) => break,
                        }
                    }
                    self.ingest_claims(batch).await;
                }
                Some(first) = io.cache.recv() => {
                    let mut batch = vec![first];
                    while batch.len() < self.cfg.batch_max {
                        match io.cache.try_recv() {
                            Ok(c) => batch.push(c),
                            Err(_) => break,
                        }
                    }
                    self.commit_cache(batch, &io.retry).await;
                }
                Some(queue) = io.queues.recv() => self.save_queue(queue).await,
                // Always ready until done; the unbiased select gives it a
                // fair share between ingest batches.
                () = std::future::ready(()), if self.backfill_ready() => self.backfill().await,
                () = std::future::ready(()), if self.candidates.ready() => self.refile_candidates().await,
                () = std::future::ready(()), if self.sweep.ready() => self.refile_unverified().await,
                _ = flush.tick() => {
                    if self.reenrich.swap(false, Ordering::AcqRel) {
                        self.backfill_after = 0;
                        self.backfill_done = false;
                    }
                    self.note_engine();
                    self.candidates.backoff.retry_due(unix_now());
                    self.sweep.backoff.retry_due(unix_now());
                    self.backfill_backoff.retry_due(unix_now());
                    self.verify(&io.retry).await;
                    self.commit(Vec::new(), Vec::new(), Vec::new(), Vec::new(), &io.retry).await;
                    if unix_now() - self.last_purge >= PURGE_EVERY {
                        self.purge().await;
                    }
                }
            }
        }
        // Claims already handed over are judged and kept: intake stopped
        // before the writer, so this is bounded by its channel.
        let mut batch = Vec::new();
        while let Ok(d) = io.claims.try_recv() {
            batch.push(d);
            if batch.len() >= self.cfg.batch_max.max(1) {
                self.ingest_claims(std::mem::take(&mut batch)).await;
            }
        }
        if !batch.is_empty() {
            self.ingest_claims(batch).await;
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

    /// Rows an always-adult source finds are marked for the backfill in the
    /// commit's own transaction (SRC-13b), so a crash before
    /// [`Writer::adult_by_source`] can't leave one non-adult with no retry.
    fn arm_adult_mark(&mut self) {
        if let Some(store) = self.store.as_mut() {
            store.set_adult_mark(lodestar_store::AdultMark {
                sources: self.adult.0,
                content_type: lodestar_classify::ContentType::Adult.code(),
            });
        }
    }

    /// The authoritative filter check (§19.4, §20.4), on the blocking pool:
    /// it reads the store ([`judge`]).
    async fn judge(&mut self, committing: Vec<Committing>) -> Vec<Committing> {
        if committing.is_empty() {
            return committing;
        }
        let store = self.store.take().expect("store");
        let engine = self.filter.current();
        let (adult, sealer, have) = (self.adult, self.sealer.clone(), self.have.clone());
        let mut flood = std::mem::take(&mut self.flood);
        let (store, flood, committing) = tokio::task::spawn_blocking(move || {
            let committing = judge(
                &store,
                &engine,
                adult,
                sealer.as_deref(),
                &mut flood,
                &have,
                committing,
            );
            (store, flood, committing)
        })
        .await
        .expect("judge task");
        self.store = Some(store);
        self.flood = flood;
        committing
    }

    /// Marks for the backfill the enrichment matches on `ids`, rows this
    /// commit found already stored, whose record it may have rewritten
    /// without them (a partial import's upgrade; ENR-1b), and starts the
    /// pass. A separate transaction after the commit: a crash between them
    /// leaves the kind unpinned until the next rules or parser change.
    async fn mark_rewritten(&mut self, ids: Vec<i64>) {
        // Skipped while no adult-enrichment source is listed (ENR-1c). A
        // match stored in an earlier run under a source no longer listed
        // isn't re-marked then; it keeps its kind until the next rules or
        // parser change (decisions file).
        if ids.is_empty() || self.enrichers.is_empty() {
            return;
        }
        let mut store = self.store.take().expect("store");
        let (store, result, fallback) = tokio::task::spawn_blocking(move || {
            let result = crate::enrich::mark_matched(&mut store, &ids);
            // Failed (a busy claims read): mark them all, as
            // `adult_by_source` does, so the backfill finds out (ENR-1c).
            let fallback = result.is_err().then(|| store.mark_unenriched(&ids));
            (store, result, fallback)
        })
        .await
        .expect("mark task");
        self.store = Some(store);
        match fallback {
            Some(Ok(_)) => self.reenrich.store(true, Ordering::Release),
            // Both failed: the rows keep their kind until the next rules or
            // parser change re-marks them.
            Some(Err(e)) => lodestar_log::log_event!(
                crate::log_events::ENRICHMENT_FAILED,
                error = crate::log_events::store_error(&e)
            ),
            None => {}
        }
        match result {
            Ok(marked) if !marked.is_empty() => self.reenrich.store(true, Ordering::Release),
            Ok(_) => {}
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::ENRICHMENT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// Classifies as adult the stored torrents among `ids` that an
    /// always-adult source has just found ([`crate::enrich::adult_by_source`]),
    /// marking what it holds for intake before its write.
    async fn adult_by_source(&mut self, ids: Vec<i64>) {
        if ids.is_empty() || self.adult.is_empty() {
            return;
        }
        let store = self.store.take().expect("store");
        let filter = self.filter.clone();
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let held = self.held.clone();
        let adult = self.adult;
        let (store, result, marked) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let ctx = crate::enrich::Backfill {
                filter: Some(&filter),
                sealer: sealer.as_deref(),
                quarantine_secs,
                adult,
            };
            let pending = std::cell::RefCell::new(None);
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
            };
            let result = crate::enrich::adult_by_source(&mut store, &ids, &ctx, &mark);
            hold_committed(&store, pending.into_inner(), false);
            // Failed: the backfill applies the same once it reaches them.
            // If even the mark fails, the next sighting tries again.
            let marked = result.is_err() && store.mark_unenriched(&ids).is_ok();
            (store, result, marked)
        })
        .await
        .expect("source adult task");
        self.store = Some(store);
        if marked {
            self.reenrich.store(true, Ordering::Release);
        }
        match result {
            Ok(b) => {
                if b.rejected {
                    // Marked for the backfill, which applies the same.
                    self.reenrich.store(true, Ordering::Release);
                }
                self.stats
                    .enriched
                    .fetch_add(b.enriched as u64, Ordering::Relaxed);
            }
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::ENRICHMENT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
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
        self.commit_all(results, Vec::new(), touches, swarm, events, retry)
            .await;
    }

    /// [`Writer::commit`], with `cache`'s lease-free files committed in the
    /// same transaction. Returns those of them it stored: the index in
    /// `cache`, the torrent's id, whether it's a new row, and whether it
    /// was committed as `private_source` (read from the store at judging).
    async fn commit_all(
        &mut self,
        results: Vec<Results>,
        cache: Vec<Committing>,
        touches: Vec<Discovery>,
        swarm: Vec<SwarmUpdate>,
        events: Vec<Event>,
        retry: &mpsc::Sender<Discovery>,
    ) -> Vec<(usize, i64, bool, bool)> {
        // Fencing (§19.3): only results under a current lease count. A
        // stale one comes from a holder whose lease expired; the key's new
        // holder will deliver it.
        let delivered = results.len();
        let mut committing: Vec<Committing> = results
            .into_iter()
            .filter_map(|r| {
                let lease = self
                    .coordinator
                    .current(&r.key, r.token)
                    .filter(|l| l.holder == r.worker)?;
                Some(Committing {
                    results: r,
                    lease,
                    leased: true,
                    verdict: None,
                    shadow: Vec::new(),
                    sealed: None,
                    publisher: None,
                })
            })
            .collect();
        self.gate
            .exclude(Excluded::Duplicate, (delivered - committing.len()) as u64);
        // A cache's files follow the leased results: `cache`'s index `j`
        // is `first_cache + j` below.
        let first_cache = committing.len();
        committing.extend(cache);
        // The classification rules' generation before the rules
        // themselves: a batch judged under rules older than the store's
        // generation is marked for enriching again (§20.9).
        let class_gen = self.store.as_ref().and_then(|s| s.class_gen().ok());
        let committing = self.judge(committing).await;
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
            return Vec::new();
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
        let stats = self.stats.clone();
        // Promotion judges the item's stored claims again under these rules.
        let engine = self.filter.current();
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let adult = self.adult;
        #[cfg(test)]
        let (between, marked, filter) = (self.between, self.marked, self.filter.clone());
        let (store, committing, touches, swarm, result) = tokio::task::spawn_blocking(move || {
            // Held since it was judged, before this batch marks its own: a
            // claim's hold (SRC-2), a report, a list. Its verified metadata
            // passed, but nothing is stored under a held hash.
            let refused: Vec<bool> = committing
                .iter()
                .map(|c| {
                    c.results
                        .torrent
                        .swarm_hashes()
                        .any(|h| held.contains(&h.0))
                })
                .collect();
            let pending = (!holding.is_empty()).then(|| held.pend(&holding));
            // A promotion that a claim now holds marks its hashes as the
            // batch's own holds are marked: before its write.
            let promo_pending = std::cell::RefCell::new(Vec::new());
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                promo_pending.borrow_mut().push(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(_st, &held, &filter, hs);
                }
            };
            let promo = Promo {
                engine: &engine,
                ctx: unverified::Ctx {
                    sealer: sealer.as_deref(),
                    quarantine_secs,
                    adult,
                    ..Default::default()
                },
                stats: &stats.unverified,
                mark: &mark,
            };
            #[cfg(test)]
            if let (Some(f), Some(_)) = (marked, &pending) {
                f(&store, &held, &filter, &holding);
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
                &refused,
                &promo,
            );
            // SP-4's signatures this batch added (RP-5), in one row: after
            // the batch, whether or not it committed, so the set in memory
            // and the store agree. A failed write stays pending for the
            // next batch's flush.
            if let Err(e) = engine.spam_sigs.flush(&mut store) {
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
            let promo_pending = promo_pending.into_inner();
            #[cfg(test)]
            if let Some(f) = between
                && (pending.is_some() || !promo_pending.is_empty())
            {
                f(&mut store, &held, &filter);
            }
            // Committed or rolled back: the store says which hold.
            hold_committed(&store, pending, true);
            for p in promo_pending {
                hold_committed(&store, Some(p), true);
            }
            (store, committing, touches, swarm, result)
        })
        .await
        .expect("writer transaction task");
        self.store = Some(store);
        let outcome = match result {
            Ok(mut o) => {
                if let Some(e) = o.failed.take() {
                    // The batch is lost but its promotions committed in
                    // their own transactions: they're accounted below.
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    let lost = |leased: bool| {
                        (0..committing.len())
                            .filter(|i| committing[*i].leased == leased)
                            .filter(|i| !o.stored.iter().any(|s| s.0 == *i))
                            .filter(|i| !o.blocked.contains(i))
                            .count() as u64
                    };
                    self.gate.exclude(Excluded::StoreFailed, lost(true));
                    self.stats
                        .torrent_cache
                        .errors
                        .fetch_add(lost(false), Ordering::Relaxed);
                    lodestar_log::log_event!(
                        crate::log_events::STORE_WRITE_FAILED,
                        error = crate::log_events::store_error(&e)
                    );
                }
                o
            }
            Err(e) => {
                // The batch and its counters are lost; keep going. Its
                // leases stay held, so the keys aren't fetched again in
                // this run.
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                let cache = committing.iter().filter(|c| !c.leased).count() as u64;
                self.gate
                    .exclude(Excluded::StoreFailed, committing.len() as u64 - cache);
                self.stats
                    .torrent_cache
                    .errors
                    .fetch_add(cache, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
                return Vec::new();
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
        // Promoted under their own ids, below every watermark: published
        // for whatever follows them (SRC-2b's percolator pickup).
        let kept: Vec<i64> = outcome
            .promoted_as
            .iter()
            .filter_map(|&(i, _, _)| {
                outcome
                    .stored
                    .iter()
                    .find(|(j, _, new, _)| *j == i && *new)
                    .map(|&(_, id, _, _)| id)
            })
            .collect();
        self.stats.unverified.promoted_hook.publish(&kept);
        // Stored torrents an always-adult source found that may not be
        // stored as adult yet: merged into, or seen again.
        let mut adult_ids: Vec<i64> = outcome
            .stored
            .iter()
            .filter(|(i, _, _, _)| committing[*i].results.source_adult)
            .map(|&(_, id, _, _)| id)
            .collect();
        // Rows already stored that this commit rewrote (a partial import's
        // upgrade writes prepare's record, which knows no match: ENR-1b).
        let rewritten: Vec<i64> = outcome
            .stored
            .iter()
            .filter(|s| !s.2)
            .map(|s| s.1)
            .collect();
        let committed_at = unix_now();
        let mut cache_stored = Vec::new();
        for (i, id, new, _) in outcome.stored {
            let c = &committing[i];
            // A cache's file: no lease to end, and neither the crawl's
            // funnel nor its gate (SRC-13b).
            if !c.leased {
                if let (true, Some(hot)) = (new, &self.hot) {
                    let at = c.lease.discovered_at.clamp(0, i64::from(u32::MAX - 1));
                    hot.touch(id as u64, at as u32);
                }
                let cs = &self.stats.torrent_cache;
                if new {
                    cs.stored.fetch_add(1, Ordering::Relaxed);
                    if outcome.promoted_as.iter().any(|(j, _, _)| *j == i) {
                        cs.promoted.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    cs.existing.fetch_add(1, Ordering::Relaxed);
                }
                cache_stored.push((i - first_cache, id, new, c.results.private_source));
                continue;
            }
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
                let (cohort, source) = outcome
                    .promoted_as
                    .iter()
                    .find(|(j, _, _)| *j == i)
                    .map_or((lease.cohort, lease.source), |&(_, c, s)| (c, s));
                self.funnel.record(cohort, source, Stage::Stored, None);
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
        // Never stored: not in the have-filter or the hot columns, and the
        // lease ends as an eviction does. The pool remembers the key as
        // recently fetched, so a live source's sighting of it is passed
        // over until that memory rolls over or the next start; then it's
        // fetched and stored as new.
        for i in outcome.refused {
            let c = &committing[i];
            if !c.leased {
                self.stats
                    .torrent_cache
                    .refused
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            self.gate.exclude(Excluded::SourceStopped, 1);
            self.coordinator
                .lost(&c.results.key, c.results.token, Loss::Evicted);
        }
        for i in outcome.blocked {
            let c = &committing[i];
            if !c.leased {
                self.stats
                    .torrent_cache
                    .held
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
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
                if self.adult.has(d.source) {
                    adult_ids.push(id);
                }
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
        #[cfg(test)]
        if self.crash_before_adult {
            return cache_stored;
        }
        self.adult_by_source(adult_ids).await;
        self.mark_rewritten(rewritten).await;
        if self.have.is_full() {
            self.rebuild_have().await;
        }
        cache_stored
    }

    /// Commits a batch of judged torrent-cache files (SRC-13b), the
    /// writer's lease-free entry beside the pool's results:
    /// - **`Store`** commits as verified metadata from the plugin's source,
    ///   through the same transaction, re-judge and promotion as a fetched
    ///   result (an unverified item holding the key is promoted under its
    ///   own id, §18.5). The writer judges it again when the rules moved on
    ///   since [`torrent_cache::judge`], or when an always-adult source
    ///   found it, so it stays adult. Once stored, the file's trackers,
    ///   comment and creation date are stored as that source's claims,
    ///   judged as any source's are.
    /// - **`Held`** is written as a held ingest verdict is (a tombstone or
    ///   quarantine, with the rule id and evidence, and the sealed name of
    ///   a quarantine), with intake told before the write. One held under
    ///   rules that have since changed is judged again under the current
    ///   ones first ([`Writer::rejudge_stale_holds`]); should they change
    ///   once more meanwhile, it's dropped unwritten.
    /// - **`Refused`** is counted and dropped (the judge task counts it).
    ///
    /// [`torrent_cache::judge`]: crate::torrent_cache::judge
    async fn commit_cache(
        &mut self,
        batch: Vec<crate::torrent_cache::ToCommit>,
        retry: &mpsc::Sender<Discovery>,
    ) {
        use crate::torrent_cache::Judged;
        let batch = self.rejudge_stale_holds(batch).await;
        let version = self.filter.current().version;
        let cs = &self.stats.torrent_cache;
        let mut admitted = Vec::new();
        let mut claims = Vec::new();
        let mut holds = Vec::new();
        for t in batch {
            match t.judged {
                Judged::Store(a) => {
                    let publisher = t.claims.publisher.clone();
                    let found = lodestar_sources::Found::key(a.key, 0)
                        .with_claims(t.claims)
                        .with_provenance(t.provenance);
                    claims.push(Discovery {
                        source: t.source,
                        found,
                        at: t.at,
                        reply: false,
                    });
                    let mut c = Committing::lease_free(*a, t.source, t.at);
                    c.publisher = publisher;
                    admitted.push(c);
                }
                Judged::Held {
                    hashes,
                    verdict,
                    version: judged_under,
                    sealed,
                    adult_hold,
                } => {
                    // Counted as `stale` when it was judged again.
                    let Some(d) = verdict.decision.filter(|_| judged_under == version) else {
                        continue;
                    };
                    let held = match d.action {
                        Action::Drop => Held::Tombstone,
                        Action::Quarantine => Held::Quarantine,
                        // `judge` holds only these two.
                        _ => continue,
                    };
                    holds.push(unverified::Hold {
                        hashes,
                        rule: d.rule.id(),
                        action: d.action.code(),
                        held,
                        sealed,
                        evidence: d.evidence_codes(),
                        adult_hold,
                        core: matches!(d.rule, lodestar_filter::RuleRef::Core(_)),
                    });
                }
                Judged::Refused(_) => {
                    cs.refused.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if !holds.is_empty() {
            self.hold_cache(holds).await;
        }
        if admitted.is_empty() {
            return;
        }
        let stored = self
            .commit_all(
                Vec::new(),
                admitted,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                retry,
            )
            .await;
        // The fields outside the info dict, as the plugin's claims on what
        // it stored: never on what was held or refused.
        // Marked private as the commit was: storing them must never make a
        // torrent public that the commit kept private (`put_one` clears
        // the mark for a public sighting).
        let mut keep: Vec<Option<bool>> = vec![None; claims.len()];
        for (j, _, _, private) in stored {
            keep[j] = Some(private);
        }
        let found: Vec<Discovery> = claims
            .into_iter()
            .zip(keep)
            .filter_map(|(mut d, k)| {
                d.found.private_source = k?;
                Some(d)
            })
            .collect();
        if !found.is_empty() {
            self.ingest_found(found, false).await;
        }
    }

    /// Judges again, on the blocking pool and under the current rules,
    /// each held cache file that was judged under rules that have changed
    /// since: a looser rule set may now store it, a stricter one still
    /// holds it. Counted as `stale`. A file whose judging panics stays
    /// held as it was (counted as an error), never stored; the panic
    /// doesn't reach the writer, whose store the task holds.
    async fn rejudge_stale_holds(
        &mut self,
        mut batch: Vec<crate::torrent_cache::ToCommit>,
    ) -> Vec<crate::torrent_cache::ToCommit> {
        use crate::torrent_cache::Judged;
        let engine = self.filter.current();
        let stale = |t: &crate::torrent_cache::ToCommit| matches!(t.judged, Judged::Held { version, .. } if version != engine.version);
        let n = batch.iter().filter(|t| stale(t)).count();
        if n == 0 {
            return batch;
        }
        self.stats
            .torrent_cache
            .stale
            .fetch_add(n as u64, Ordering::Relaxed);
        let store = self.store.take().expect("store");
        let sealer = self.sealer.clone();
        let (store, batch, failed) = tokio::task::spawn_blocking(move || {
            let mut preparer = store.preparer();
            let mut failed = 0u64;
            for t in &mut batch {
                if matches!(t.judged, Judged::Held { version, .. } if version != engine.version) {
                    // A panic judging one file mustn't take the writer
                    // down: it stays held, as it was, under these rules.
                    let judged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::torrent_cache::judge_sealed(
                            &engine,
                            &mut preparer,
                            sealer.as_deref(),
                            t.key,
                            &t.info_raw,
                            unix_now(),
                        )
                    }));
                    match judged {
                        Ok(j) => t.judged = j,
                        Err(_) => {
                            failed += 1;
                            if let Judged::Held { version, .. } = &mut t.judged {
                                *version = engine.version;
                            }
                        }
                    }
                }
            }
            (store, batch, failed)
        })
        .await
        .expect("cache rejudge task");
        self.store = Some(store);
        if failed > 0 {
            self.stats
                .torrent_cache
                .errors
                .fetch_add(failed, Ordering::Relaxed);
        }
        batch
    }

    /// Writes held cache files' holds (SRC-13b) as a held ingest verdict is
    /// written: marked for intake first, then the hold, removing the
    /// unverified item under the key, or a stored torrent under it (an
    /// import whose verified name is now known to be held), in the same
    /// transaction.
    async fn hold_cache(&mut self, holds: Vec<unverified::Hold>) {
        let mut store = self.store.take().expect("store");
        let held = self.held.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let n = holds.len() as u64;
        let (store, written, result) = tokio::task::spawn_blocking(move || {
            let now = unix_now();
            let hashes: Vec<[u8; 20]> = holds.iter().flat_map(|h| h.hashes.clone()).collect();
            let pending = held.pend(&hashes);
            // One transaction per hold: those before an error are written.
            let mut written = 0u64;
            let result = (|| {
                for h in &holds {
                    let f = h.filtered(now, quarantine_secs);
                    let item = h
                        .hashes
                        .iter()
                        .find_map(|k| store.unverified(Network::Bt, k).transpose())
                        .transpose()?;
                    match item {
                        Some(item) => {
                            let refiled = RefiledUnverified {
                                id: item.id,
                                filtered: Some(f),
                            };
                            store.refile_unverified(&[refiled], || true)?;
                        }
                        None => {
                            store.hold_claims(&[f])?;
                        }
                    }
                    written += 1;
                }
                Ok::<_, lodestar_store::Error>(())
            })();
            // Written or failed: the store says which hold.
            hold_committed(&store, Some(pending), true);
            (store, written, result)
        })
        .await
        .expect("cache hold task");
        self.store = Some(store);
        let cs = &self.stats.torrent_cache;
        cs.held.fetch_add(written, Ordering::Relaxed);
        match result {
            Ok(()) => {}
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                cs.errors.fetch_add(n - written, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// The backfill has a batch to enrich now: not done, and not waiting
    /// out a store error.
    fn backfill_ready(&self) -> bool {
        !self.backfill_done && self.backfill_backoff.ready()
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
        let adult = self.adult;
        #[cfg(test)]
        let (between, marked) = (self.between, self.marked);
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let ctx = crate::enrich::Backfill {
                filter: Some(&filter),
                sealer: sealer.as_deref(),
                quarantine_secs,
                adult,
            };
            let pending = std::cell::RefCell::new(None);
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(_st, &held, &filter, hs);
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
            hold_committed(&store, pending, false);
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
                self.backfill_backoff.succeeded();
                self.stats
                    .enriched
                    .fetch_add(b.enriched as u64, Ordering::Relaxed);
            }
            Ok(None) => {
                self.backfill_done = true;
                self.backfill_backoff.succeeded();
            }
            Err(e) => {
                // The same batch again after a backoff (#282): a transient
                // error doesn't end backfill for the run.
                self.backfill_backoff.failed(unix_now());
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
        if self.sweep.version != Some(version) {
            self.sweep.restart(version);
        }
    }

    /// Judges a batch of discoveries with claims and stores what the rules
    /// let through ([`unverified::ingest`]): filtering comes before
    /// storage, and a hold is marked for intake before its commit.
    async fn ingest_claims(&mut self, claimed: Vec<unverified::Claimed>) {
        // The budget is given back when the batch is done, stored or not.
        let (batch, _permits): (Vec<Discovery>, Vec<_>) = claimed.into_iter().unzip();
        self.ingest_found(batch, true).await;
    }

    /// [`Writer::ingest_claims`]'s work, for discoveries that hold no
    /// budget. `rediscovery`: a sighting of a stored torrent is counted as
    /// a rediscovery (a feed's). A cache's claims on the torrent it has
    /// just stored aren't one (SRC-13b), so they skip that accounting.
    async fn ingest_found(&mut self, batch: Vec<Discovery>, rediscovery: bool) {
        // A cache's claims on the torrent it has just stored are counted
        // at its commit, by their publisher: nothing to note.
        if rediscovery {
            self.flood.note_claims(&batch, &self.have);
        }
        let store = self.store.take().expect("store");
        let filter = self.filter.clone();
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let held = self.held.clone();
        let stats = self.stats.clone();
        let adult = self.adult;
        let (enrichers, reenrich) = (self.enrichers, self.reenrich.clone());
        #[cfg(test)]
        let (between, marked) = (self.between, self.marked);
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let engine = filter.current();
            let pending = std::cell::RefCell::new(None);
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(_st, &held, &filter, hs);
                }
            };
            let ctx = unverified::Ctx {
                sealer: sealer.as_deref(),
                quarantine_secs,
                adult,
                enrichers,
                filter: Some(&filter),
                reenrich: Some(&reenrich),
            };
            let result = unverified::ingest(
                &mut store,
                &engine,
                ctx,
                &batch,
                &stats.unverified,
                unix_now(),
                &mark,
            );
            let pending = pending.into_inner();
            #[cfg(test)]
            if let (Some(f), Some(_)) = (between, &pending) {
                f(&mut store, &held, &filter);
            }
            // Committed or failed: the store says which hold.
            hold_committed(&store, pending, true);
            (store, result)
        })
        .await
        .expect("claims task");
        self.store = Some(store);
        match result {
            Ok(mut done) => {
                self.stats.transactions.fetch_add(1, Ordering::Relaxed);
                let adult_ids = std::mem::take(&mut done.adult);
                // A feed's sighting of a stored torrent is a rediscovery, as
                // a bare one is in `commit`.
                let touched = if rediscovery {
                    std::mem::take(&mut done.touched)
                } else {
                    Vec::new()
                };
                for (id, at, source, strategy) in touched {
                    if let Some(hot) = &self.hot {
                        hot.touch(id as u64, at.clamp(0, i64::from(u32::MAX - 1)) as u32);
                    }
                    if let Some(health) = &self.health {
                        health.rediscovered(id);
                    }
                    self.stats.rediscovered.fetch_add(1, Ordering::Relaxed);
                    self.gate.exclude(Excluded::Rediscovered, 1);
                    self.rollups
                        .add(at, metric::REDISCOVERED, source, i64::from(strategy), 1);
                }
                #[cfg(test)]
                if self.crash_before_adult {
                    return;
                }
                self.adult_by_source(adult_ids).await;
            }
            Err(e) => {
                // The batch is lost; its sources send their items again.
                // The error carries no claim text.
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// One step of the unverified refile sweep ([`unverified::refile_step`]).
    async fn refile_unverified(&mut self) {
        let store = self.store.take().expect("store");
        let filter = self.filter.clone();
        let version = *self
            .sweep
            .version
            .get_or_insert_with(|| filter.current().version);
        let (phase, after_item, after_claims) = (
            self.sweep.phase,
            self.sweep.after_item,
            self.sweep.after_claims,
        );
        let sealer = self.sealer.clone();
        let quarantine_secs = self.cfg.quarantine_secs;
        let held = self.held.clone();
        let adult = self.adult;
        #[cfg(test)]
        let (between, marked) = (self.between, self.marked);
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let pending = std::cell::RefCell::new(None);
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(_st, &held, &filter, hs);
                }
            };
            let ctx = unverified::Ctx {
                sealer: sealer.as_deref(),
                quarantine_secs,
                adult,
                ..Default::default()
            };
            let result = unverified::refile_step(
                &mut store,
                &filter,
                version,
                phase,
                after_item,
                after_claims,
                ctx,
                unix_now(),
                &mark,
            );
            let pending = pending.into_inner();
            #[cfg(test)]
            if let (Some(f), Some(_)) = (between, &pending) {
                f(&mut store, &held, &filter);
            }
            // Committed, rejected or failed: the store says which hold.
            hold_committed(&store, pending, false);
            (store, result)
        })
        .await
        .expect("unverified refile task");
        self.store = Some(store);
        match result {
            // The rules changed: all of it again, under the new ones.
            Ok(step) if step.rejected => self.sweep.restart(self.filter.current().version),
            Ok(step) => {
                self.sweep.backoff.succeeded();
                self.stats
                    .unverified
                    .refiled
                    .fetch_add(step.removed as u64, Ordering::Relaxed);
                self.sweep.after_item = step.after_item;
                self.sweep.after_claims = step.after_claims;
                if step.end {
                    match phase {
                        unverified::Phase::Items => self.sweep.phase = unverified::Phase::Claims,
                        unverified::Phase::Claims => {
                            self.sweep.done = true;
                            self.stats.unverified.sweeps.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
            Err(e) => {
                // The same step again after a backoff, so what the current
                // rules hold doesn't stay at rest. No claim text in it.
                self.sweep.backoff.failed(unix_now());
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::ENRICHMENT_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
        }
    }

    /// Hands the next few unverified keys to intake for fetching, when
    /// verification is on ([`unverified::Verify`]). Never more than half
    /// of what intake's retry channel has room for, so have-filter
    /// retries keep theirs, and at most [`unverified::VERIFY_PER_FLUSH`].
    async fn verify(&mut self, retry: &mpsc::Sender<Discovery>) {
        if !self.verify.on {
            return;
        }
        let limit = unverified::VERIFY_PER_FLUSH.min(retry.capacity() / 2);
        if limit == 0 {
            return;
        }
        let store = self.store.take().expect("store");
        let mut verify = std::mem::take(&mut self.verify);
        let (store, verify, result) = tokio::task::spawn_blocking(move || {
            let r = verify.next(&store, limit, unix_now());
            (store, verify, r)
        })
        .await
        .expect("verify task");
        self.store = Some(store);
        self.verify = verify;
        match result {
            Ok(keys) => {
                for d in keys {
                    // Room was checked above; a key that still finds it
                    // full comes round again with the next pass.
                    if retry.try_send(d).is_err() {
                        break;
                    }
                    self.stats
                        .unverified
                        .verify_sent
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::log_events::STORE_WRITE_FAILED,
                    error = crate::log_events::store_error(&e)
                );
            }
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
                adult: Default::default(),
            };
            let pending = std::cell::RefCell::new(None);
            let mark = |_st: &Store, hs: &[[u8; 20]]| {
                *pending.borrow_mut() = Some(held.pend(hs));
                #[cfg(test)]
                if let Some(f) = marked {
                    f(_st, &held, &filter, hs);
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
            hold_committed(&store, pending, false);
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
                self.candidates.backoff.succeeded();
            }
            Ok(None) => {
                self.candidates.done = true;
                self.candidates.backoff.succeeded();
            }
            Err(e) => {
                // The same batch again after a backoff, so names the
                // current rules hold don't stay at rest until the next
                // rule change or start. The error carries no name.
                self.candidates.backoff.failed(unix_now());
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

/// The authoritative filter check (§19.4, §20.4): a verdict from the
/// built-in worker under the current rule set stands; anything else is
/// judged again. Quarantined names are sealed here. Blocking: it reads
/// the store.
fn judge(
    store: &Store,
    engine: &Engine,
    adult: AdultSources,
    sealer: Option<&Sealer>,
    flood: &mut Flooding,
    have: &Have,
    mut committing: Vec<Committing>,
) -> Vec<Committing> {
    // At most one read per batch, and only when it's needed: by the
    // adult check (none while no always-adult source is registered) or
    // by SP-3's publisher lookup. A read error counts as having items:
    // stricter for the adult check, and SP-3's lookup only reads.
    let any = std::cell::OnceCell::new();
    let any_unverified = || *any.get_or_init(|| store.has_unverified().unwrap_or(true));
    let unverified = !adult.is_empty() && any_unverified();
    let now = flood.now();
    let sp3_on = engine.packs.is_on(lodestar_filter::PackRule::Sp3);
    let sp4_on = engine.packs.is_on(lodestar_filter::PackRule::Sp4);
    let sigs = &engine.spam_sigs;
    for c in &mut committing {
        // A cache's file was looked up as public. Whether a private
        // source has stored the key since is read here, so the commit
        // never clears a private mark from the lookup's old view.
        if !c.leased {
            c.results.private_source = stored_private(store, &c.results);
        }
        // Found by an always-adult source the worker didn't see: the
        // lease's, or the source of an unverified item it promotes.
        // Judged again as adult before anything is stored.
        let newly_adult = !c.results.source_adult && source_adult(store, adult, c, unverified);
        let r = &mut c.results;
        r.source_adult |= newly_adult;
        let mut verdict =
            if r.worker == WorkerId::LOCAL && r.version == engine.version && !newly_adult {
                std::mem::take(&mut r.verdict)
            } else {
                rejudge(engine, r, c.lease.discovered_at, unix_now())
            };
        // SP-3 (RP-4) and SP-4 (RP-5): prepare's verdict leaves them out;
        // they're judged here, in commit order.
        // SP-3 is counted with it off too, so switching it on finds the
        // rate as it is; judged again only while it's on.
        let flood_level = flood
            .count(store, have, c, &any_unverified, now)
            .filter(|_| sp3_on);
        // SP-4's payload signature, computed once and only when needed:
        // to look a clone up (SP-4 on, the set not empty) or to add an
        // original. A crawl commit while the set is empty pays nothing.
        let mut payload: Option<Option<u64>> = None;
        let sig_of = |r: &Results| {
            lodestar_filter::spam_sigs::usable(lodestar_filter::payload_sig(&r.files))
        };
        let clone_of = if sp4_on && !sigs.is_empty() {
            payload
                .get_or_insert_with(|| sig_of(&c.results))
                .and_then(|s| sigs.get(s))
        } else {
            None
        };
        let at = AtCommit {
            flood: flood_level,
            clone_of,
        };
        if at.any() {
            let again = judge_at_commit(engine, &c.results, c.lease.discovered_at, at, now);
            verdict = crate::source_adult::stricter(verdict, again);
            // A shadow entry the two judgments both logged with their
            // own decision's action (a CSX-5 entry in shadow mode) is
            // logged once, with the decision that stands.
            let mut seen = Vec::new();
            verdict.shadow.retain(|d| {
                let first = !seen.contains(&d.rule);
                seen.push(d.rule);
                first || !matches!(d.rule, lodestar_filter::RuleRef::Override(_))
            });
            if let Some(d) = &verdict.decision {
                for e in &mut verdict.shadow {
                    if matches!(e.rule, lodestar_filter::RuleRef::Override(_)) {
                        e.action = d.action;
                    }
                }
            }
        }
        // SP-4's set (RP-5): SP-2 hiding, or SP-3 demoting or hiding, a
        // torrent this commit stores adds its signature, in memory now so
        // a clone later in the batch is caught, and written once the batch
        // is ([`Writer::commit_all`]). Added with SP-4 off too, so turning
        // it on finds the originals. A clone SP-4 hid adds nothing: its
        // signature is its original's.
        if let Some(original) = verdict.decision.as_ref().and_then(|d| match d.rule {
            lodestar_filter::RuleRef::Pack(r) => {
                lodestar_filter::spam_sigs::Original::of(r, d.action)
            }
            _ => None,
        }) && let Some(s) = *payload.get_or_insert_with(|| sig_of(&c.results))
        {
            sigs.add(s, original);
        }
        c.verdict = verdict.decision;
        c.shadow = verdict.shadow;
        if c.verdict
            .as_ref()
            .is_some_and(|h| h.action == Action::Quarantine)
        {
            c.sealed = sealer.and_then(|s| s.seal(&c.results.torrent.name).ok());
        }
    }
    committing
}

/// Whether the store holds `r`'s key as private now: a torrent under one
/// of its swarm hashes marked `private_source`, or an unverified item
/// under one that is. A read error counts as private: stricter. Blocking.
/// The writer is the store's only writer apart from imports, which never
/// clear a private mark, so what's read here holds at the commit.
fn stored_private(store: &Store, r: &Results) -> bool {
    r.torrent.swarm_hashes().any(|h| {
        let torrent = match store.find(&h) {
            Ok(Some(id)) => store.torrent(id).map_or(true, |t| {
                t.is_some_and(|t| t.flags & flags::PRIVATE_SOURCE != 0)
            }),
            Ok(None) => false,
            Err(_) => true,
        };
        torrent
            || store
                .unverified(Network::Bt, &h.0)
                .map_or(true, |i| i.is_some_and(|i| i.private_source))
    })
}

/// Whether an always-adult source found what `c` commits, besides the
/// sources its worker saw: the lease's, a stored torrent's under one of
/// its swarm hashes (a partial import being upgraded), or any finder of
/// an unverified item under one (the one promotion stores it as).
/// Costs nothing while no such source is registered. Blocking.
fn source_adult(store: &Store, adult: AdultSources, c: &Committing, unverified: bool) -> bool {
    if adult.is_empty() {
        return false;
    }
    let r = &c.results;
    if adult.any(r.sources | c.lease.sources | bit(c.lease.source)) {
        return true;
    }
    // Already stored and found by one before: a refetch of a partial
    // import (its lease is the peer finder's) upgrades that row, so
    // the row's own finders count too. A read error counts as adult.
    let stored = r.torrent.swarm_hashes().any(|h| match store.find(&h) {
        Ok(None) => false,
        Ok(Some(id)) => store
            .torrent(id)
            .map_or(true, |t| t.is_some_and(|t| adult.any(t.sources))),
        Err(_) => true,
    });
    if stored {
        return true;
    }
    if !unverified {
        return false;
    }
    r.torrent.swarm_hashes().any(|h| {
        // A read error counts as adult: stricter, and the commit's
        // own reads will fail the same way.
        store.unverified(Network::Bt, &h.0).map_or(true, |i| {
            i.is_some_and(|i| adult.any(i.sources) || adult.has(i.source))
        })
    })
}

/// Judges a torrent again at commit (a remote worker's, one prepared
/// under an older rule set, or one an always-adult source found that its
/// worker didn't see), and stores what the classification rules (and the
/// source) say now if they changed since prepare (§20.9).
pub(crate) fn rejudge(engine: &Engine, r: &mut Results, discovered_at: i64, now: i64) -> Verdict {
    // An always-adult source found it: adult whatever the rules say.
    let (verdict, fix) = with_item(r, discovered_at, now, |item, facts| {
        crate::source_adult::judge_maybe(engine, item, facts, r.source_adult, now)
    });
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

/// A commit as the engine judges it: its swarm hashes, name, files and
/// classification, and the stored fields. Shared by [`rejudge`] and
/// [`judge_at_commit`], so the two readings of one commit can't drift.
fn with_item<T>(
    r: &Results,
    discovered_at: i64,
    now: i64,
    f: impl FnOnce(&Item<'_>, &Facts<'_>) -> T,
) -> T {
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
    f(&item, &facts)
}

/// The sources that deliver a live feed of new items, by plugin id: the
/// only ones whose finds count against their own source when no
/// publisher is named (runner, #495, §2.33). Every other source counts
/// only against a publisher its claims name, since its finds can be a
/// backlog: a full scrape sends whole catalogues (BEP 48), a download
/// client's first poll your whole library, `arr`, Internet Archive and
/// Academic Torrents their own collections, Prowlarr every indexer at
/// once, and the torrent cache and importers lookups and history. The
/// ids are the plugins' own `ID`s in `crates/sources-plugins`.
pub const LIVE_FEEDS: &[&str] = &["nyaa", "sukebei"];

/// SP-3's state in the writer (RP-4, plan 2026-11-04 Q10): the rate map,
/// and which sources count, with a publisher or without.
#[derive(Debug, Default)]
pub(crate) struct Flooding {
    pub map: lodestar_filter::flood::Flood,
    /// The sources, a bitset over [`SourceId`]s, whose claims' publisher
    /// counts: every registered source but a peer finder (the DHT, which
    /// publishes nothing, Q10) and an enrichment source, whose matches
    /// describe torrents others published. The one exception is the
    /// torrent cache (an enrichment source): a cache's file counts
    /// against the publisher its claims name ([`Flooding::who`]).
    pub claims_from: u64,
    /// Of them, the sources whose finds also count against the source
    /// itself when no publisher is named: the live feeds
    /// ([`LIVE_FEEDS`]).
    pub per_source: u64,
    /// Whom the claims the writer stored named, by key, so a commit
    /// that promotes their item needn't read it ([`Flooding::who`]).
    pub notes: ClaimNotes,
    /// The clock, for tests; Unix time otherwise.
    #[cfg(test)]
    pub clock: Option<i64>,
}

/// Keys whose claims the writer handed to the store, at most
/// [`ClaimNotes::CAP`], the oldest forgotten first: per key, the flood
/// map's key for the publisher the claims named (with the source that
/// named it), or the source that made them. Memory only. A key it doesn't
/// hold (forgotten, or claimed before this start) costs the commit one
/// read of its unverified item when a live feed is among its finders,
/// and isn't counted otherwise ([`Flooding::who`]).
#[derive(Debug, Default)]
pub(crate) struct ClaimNotes {
    by_key: std::collections::HashMap<[u8; 20], Who>,
    order: std::collections::VecDeque<[u8; 20]>,
}

impl ClaimNotes {
    /// About 40 bytes a key, so a few MB at the cap.
    pub const CAP: usize = 65_536;

    fn insert(&mut self, key: [u8; 20], who: Who) {
        if self.by_key.insert(key, who).is_none() {
            self.order.push_back(key);
            if self.order.len() > ClaimNotes::CAP
                && let Some(old) = self.order.pop_front()
            {
                self.by_key.remove(&old);
            }
        }
    }
}

/// Whom SP-3 counts a commit against: a publisher by its flood-map key
/// ([`lodestar_filter::flood::Flood::key`]), or a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    Publisher(u64),
    Source(SourceId),
}

impl Flooding {
    /// The state for these registered sources ([`Flooding::claims_from`],
    /// [`Flooding::per_source`]).
    pub(crate) fn for_sources<'a>(
        sources: impl IntoIterator<Item = (SourceId, &'a lodestar_sources::Manifest)>,
    ) -> Flooding {
        use lodestar_sources::Kind;
        let mut f = Flooding::default();
        for (id, m) in sources {
            if m.is(Kind::PeerFinder) || m.is(Kind::Enrichment) {
                continue;
            }
            f.claims_from |= bit(id);
            if LIVE_FEEDS.contains(&m.id) && !m.is(Kind::Importer) {
                f.per_source |= bit(id);
            }
        }
        f
    }

    fn now(&self) -> i64 {
        #[cfg(test)]
        if let Some(t) = self.clock {
            return t;
        }
        unix_now()
    }

    /// Counts `c`'s name against its publisher, or else its source
    /// ([`Flooding::who`]), and returns the level the near-duplicates
    /// have reached. `None` for a find that counts against no one, or a
    /// name with no title. Blocking: it may read the store.
    ///
    /// A find no source that counts made (the crawl's usual commit, the
    /// DHT's alone) costs a bit test and nothing more. A torrent already
    /// stored (in the have-filter) isn't counted: a refetch, or a cache
    /// offering a file again, is the same name, not a new one. A
    /// have-filter false positive leaves one out, which only undercounts.
    fn count(
        &mut self,
        store: &Store,
        have: &Have,
        c: &Committing,
        unverified: &dyn Fn() -> bool,
        now: i64,
    ) -> Option<lodestar_filter::flood::Level> {
        use lodestar_filter::flood::{Origin, signature};
        let named = c.publisher.as_deref().is_some_and(|p| !p.is_empty());
        if !named && (!c.leased || self.finders(c) & self.claims_from == 0) {
            return None;
        }
        if c.results.torrent.swarm_hashes().any(|h| have.contains(&h)) {
            return None;
        }
        let who = self.who(store, c, unverified)?;
        let sig = signature(&c.results.class.title.release.title)?;
        let key = match who {
            Who::Publisher(p) => p,
            Who::Source(s) => self.map.key(Origin::Source(s.get())),
        };
        self.map.record_key(key, sig, now)
    }

    fn finders(&self, c: &Committing) -> u64 {
        c.lease.sources | bit(c.lease.source) | c.results.sources
    }

    /// The flood-map key of `publisher` as `source` names it, when it
    /// names one.
    fn publisher(&self, source: SourceId, publisher: Option<&str>) -> Option<Who> {
        use lodestar_filter::flood::Origin;
        publisher
            .filter(|p| !p.is_empty())
            .map(|p| Who::Publisher(self.map.key(Origin::Publisher(source.get(), p))))
    }

    /// Whom a claim from `source` naming `publisher` counts against.
    fn claim_counts(&self, source: SourceId, publisher: Option<&str>) -> Option<Who> {
        if self.claims_from & bit(source) == 0 {
            return None;
        }
        self.publisher(source, publisher)
            .or_else(|| (self.per_source & bit(source) != 0).then_some(Who::Source(source)))
    }

    /// Notes whom each of `batch`'s claims count against, as the writer
    /// hands them to the store ([`ClaimNotes`]). A claim that counts
    /// against no one isn't noted, nor one for a key already stored,
    /// whose commit is never counted: so a busy cache or enrichment source
    /// never pushes a live feed's notes out.
    fn note_claims(&mut self, batch: &[Discovery], have: &Have) {
        for d in batch {
            let Some(c) = d.found.claims.as_deref() else {
                continue;
            };
            if have.contains(&d.found.key) {
                continue;
            }
            if let Some(who) = self.claim_counts(d.source, c.publisher.as_deref()) {
                self.notes.insert(d.found.key.0, who);
            }
        }
    }

    /// The publisher a cache's claims name, or the claims of the
    /// unverified item the commit promotes; else, for a live feed
    /// ([`LIVE_FEEDS`]), that item's source or the lease's. A publisher
    /// is keyed with the source that named it. An empty publisher is
    /// none. A store read that fails reads as no item.
    ///
    /// A non-live source's claim is found only through its note: its item
    /// is read only when a live feed is among the commit's finders.
    ///
    /// A claimed item the DHT alone fetched, before any feed's sighting
    /// reached its lease, isn't counted.
    fn who(&self, store: &Store, c: &Committing, unverified: &dyn Fn() -> bool) -> Option<Who> {
        // A torrent cache's file counts only against a publisher its
        // claims name (runner, #495): a cache's backfill is lookup
        // traffic, not a stream from one uploader.
        if !c.leased {
            return self.publisher(c.lease.source, c.publisher.as_deref());
        }
        if self.finders(c) & self.claims_from != 0 && unverified() {
            // Claims this start stored: no read.
            if let Some(w) = c
                .results
                .torrent
                .swarm_hashes()
                .find_map(|h| self.notes.by_key.get(&h.0))
            {
                return Some(*w);
            }
            // Without a note, the item is read only when a live feed is
            // among the finders (runner, #495). A non-live source's claim
            // counts only through its note, so one claimed before a
            // restart isn't counted: that undercounts, never overcounts,
            // and keeps a full scrape's bulk commits free of a read.
            if self.finders(c) & self.per_source == 0 {
                return None;
            }
            for h in c.results.torrent.swarm_hashes() {
                let Ok(Some(item)) = store.unverified(Network::Bt, &h.0) else {
                    continue;
                };
                let claimed = item.claims.as_ref().and_then(|x| x.publisher.as_deref());
                if let Some(w) = self.claim_counts(item.source, claimed) {
                    return Some(w);
                }
            }
        }
        let s = c.lease.source;
        (self.per_source & bit(s) != 0).then_some(Who::Source(s))
    }
}

/// A commit judged again with SP-3 at its level and SP-4 inheriting its
/// original, as `at` says ([`Engine::judge_at_commit`]), read as
/// [`rejudge`] reads it ([`with_item`]), an always-adult source's find as
/// adult too.
fn judge_at_commit(
    engine: &Engine,
    r: &Results,
    discovered_at: i64,
    at: AtCommit,
    now: i64,
) -> Verdict {
    with_item(r, discovered_at, now, |item, facts| {
        engine.judge_at_commit(item, facts, now, at, r.source_adult)
    })
}

/// Torrents enriched per background step.
const BACKFILL_BATCH: usize = 256;

/// Import candidates judged per refile step: one short transaction each.
const REFILE_BATCH: usize = 512;
/// The wait after a sweep's first store error in a row, doubling after
/// each further one up to the cap (seconds).
const RETRY_FIRST: i64 = 5;
const RETRY_MAX: i64 = 300;

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
    refused: &[bool],
    promo: &Promo<'_>,
) -> Result<Outcome, lodestar_store::Error> {
    // Filtered torrents are never stored: a drop leaves a tombstone, a
    // quarantine a sealed name (§20.3). Other actions (from rules that
    // don't hold torrents back) don't stop storage.
    let held = |c: &Committing| match c.verdict.as_ref().map(|h| h.action) {
        Some(Action::Drop) => Some(Held::Tombstone),
        Some(Action::Quarantine) => Some(Held::Quarantine),
        _ => None,
    };
    let (mut blocked, accepted): (Vec<usize>, Vec<usize>) = (0..committing.len())
        .partition(|&i| held(&committing[i]).is_some() || refused.get(i).copied().unwrap_or(false));
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
    let all_prepared: Vec<PreparedTorrent<'_>> = accepted
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
                    // So is a torrent cache's (SRC-13b): it never came
                    // through the crawl's funnel, live or in a repair.
                    | if crate::in_funnel(lease.cohort) && committing[i].leased {
                        0
                    } else {
                        flags::IMPORTED
                    },
                source: lease.source,
                verified: true,
            }
        })
        .collect();
    // Promotion (§18.5): a verified torrent whose hash an unverified item
    // holds promotes it under the item's id, whichever path fetched it
    // (a verification fetch, or the crawler finding the same hash), and is
    // never stored twice. Before the batch's inserts, so the id is kept.
    // A store with no unverified items (no source with claims) skips it.
    let mut promoted: Vec<(usize, i64, bool)> = Vec::new();
    let mut promoted_as: Vec<(usize, i64, SourceId)> = Vec::new();
    let mut promo_held: Vec<usize> = Vec::new();
    let mut rest: Vec<usize> = Vec::with_capacity(accepted.len());
    let mut prepared: Vec<PreparedTorrent<'_>> = Vec::with_capacity(accepted.len());
    let check = !accepted.is_empty() && store.has_unverified()?;
    // The classification rules changed since the batch was classified
    // (§20.9): a promotion is stored without its enrichment, so the
    // background pass enriches it under the current rules, as `write`
    // marks its own rows.
    let stale_promo = check && class_gen.is_some() && store.class_gen().ok() != class_gen;
    for (&i, pt) in accepted.iter().zip(&all_prepared) {
        if !check {
            rest.push(i);
            prepared.push(*pt);
            continue;
        }
        match promote(store, &committing[i].results, pt, now, stale_promo, promo)? {
            Promotion::None => {
                rest.push(i);
                prepared.push(*pt);
            }
            Promotion::Stored(id, new, by) => {
                promoted.push((i, id, new));
                if let Some((cohort, source)) = by {
                    promoted_as.push((i, cohort, source));
                }
            }
            Promotion::Held => promo_held.push(i),
        }
    }
    blocked.extend(&promo_held);
    // Other sources that reported a key while it was leased, then the
    // rediscoveries.
    let mut all_touches: Vec<Touch> = accepted
        .iter()
        .filter(|i| !blocked.contains(i))
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
    let hashes_of = |i: usize| committing[i].results.torrent.swarm_hashes().collect();
    let written = match store.write(&WriteBatch {
        prepared: &prepared,
        touches: &all_touches,
        swarm,
        counters,
        cohorts,
        events,
        filtered: &filtered,
        class_gen,
        ..WriteBatch::default()
    }) {
        Ok(w) => w,
        // The promotions before it committed on their own: report them.
        Err(e) if !promoted.is_empty() || !promo_held.is_empty() => {
            return Ok(Outcome {
                stored: promoted
                    .into_iter()
                    .map(|(i, id, new)| (i, id, new, hashes_of(i)))
                    .collect(),
                promoted_as,
                failed: Some(e),
                blocked: promo_held,
                refused: Vec::new(),
                touched: Vec::new(),
                swarm: Vec::new(),
                stale_class: stale_promo,
            });
        }
        Err(e) => return Err(e),
    };
    let mut refused = Vec::new();
    let stored = rest
        .iter()
        .zip(&written.prepared)
        .filter_map(|(&i, ins)| match ins.stored() {
            Some(id) => Some((i, id, matches!(ins, Inserted::New(_)))),
            None => {
                refused.push(i);
                None
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .chain(promoted)
        .map(|(i, id, new)| (i, id, new, hashes_of(i)))
        .collect();
    Ok(Outcome {
        stored,
        promoted_as,
        failed: None,
        blocked,
        refused,
        stale_class: written.stale_class || stale_promo,
        touched: written.touched[extra..].to_vec(),
        swarm: written.swarm,
    })
}

/// What promotion needs besides the store: the rules to judge an item's
/// claims again under, and how to hold and mark what they hold.
struct Promo<'a> {
    engine: &'a Engine,
    ctx: unverified::Ctx<'a>,
    stats: &'a UnverifiedStats,
    mark: &'a dyn Fn(&Store, &[[u8; 20]]),
}

/// What [`promote`] did with one verified torrent.
enum Promotion {
    /// No unverified item holds its hashes: it's inserted as usual.
    None,
    /// Promoted (`true`: under the item's id) or merged into a torrent
    /// already stored under the hash (`false`). A kept one carries the
    /// cohort and source its row records, for the funnel.
    Stored(i64, bool, Option<(i64, SourceId)>),
    /// One of its hashes was held since it was judged: not stored.
    Held,
}

/// Promotes the unverified items under a verified torrent's swarm hashes
/// (one, or two for a hybrid whose v1 and v2 keys were found apart).
///
/// **The duplicate check costs one probe of `unverified_items`' unique
/// `(network, key)` index per swarm hash**, on every crawler insert (the
/// per-insert measurement in `2026-11-02-SRC-2.md`). There's no trigger
/// on `torrents` inserts (SRC-1).
///
/// The item's claims never change the verified metadata: a claimed file
/// list that contradicts it only adds the `claim-mismatch` junk reason
/// (§18.5). The promotion is counted as newly searchable when its id is
/// already below the index watermark (`Store::promote`).
fn promote(
    store: &mut Store,
    r: &Results,
    pt: &PreparedTorrent<'_>,
    now: i64,
    stale_class: bool,
    promo: &Promo<'_>,
) -> Result<Promotion, lodestar_store::Error> {
    let stats = promo.stats;
    let mut out = Promotion::None;
    let hashes: Vec<[u8; 20]> = r.torrent.swarm_hashes().map(|h| h.0).collect();
    for h in r.torrent.swarm_hashes() {
        let Some(item) = store.unverified(Network::Bt, &h.0)? else {
            continue;
        };
        // The claims are judged again under the current rules first: a
        // rule added since they were stored, which the sweep hasn't
        // reached yet, must not let them be promoted.
        let hold = match &item.claims {
            Some(c) => {
                let adult = promo.ctx.adult.any(item.sources) || promo.ctx.adult.has(item.source);
                unverified::judge(promo.engine, h.0, c, promo.ctx.sealer, adult, now)
            }
            None => None,
        };
        match hold {
            // The floor or the core: the hash is held, and nothing is
            // stored under it (the item goes, journalled).
            Some(mut hold) if hold.core => {
                hold.hashes = hashes.clone();
                (promo.mark)(store, &hold.hashes);
                let f = hold.filtered(now, promo.ctx.quarantine_secs);
                let refiled = RefiledUnverified {
                    id: item.id,
                    filtered: Some(f),
                };
                store.refile_unverified(&[refiled], || true)?;
                stats.held.fetch_add(1, Ordering::Relaxed);
                return Ok(Promotion::Held);
            }
            // Any other hold, or claims that can't be read: the claims
            // can't override verified metadata, so they're dropped (the
            // item goes) and the torrent is judged on its own.
            Some(_) => {
                let refiled = RefiledUnverified {
                    id: item.id,
                    filtered: None,
                };
                store.refile_unverified(&[refiled], || true)?;
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            None if item.unreadable => {
                let refiled = RefiledUnverified {
                    id: item.id,
                    filtered: None,
                };
                store.refile_unverified(&[refiled], || true)?;
                continue;
            }
            None => {}
        }
        let fixed = if stale_class {
            Some(Prepared {
                enrichment: None,
                ..r.torrent.clone()
            })
        } else {
            item.claims
                .as_ref()
                .and_then(|c| unverified::with_mismatch(&r.torrent, &r.files, c))
        };
        // Counted once stored under the item's own id: a merge keeps the
        // stored torrent's enrichment, so there's no mismatch recorded.
        let mismatch = !stale_class && fixed.is_some();
        let t = PreparedTorrent {
            torrent: fixed.as_ref().unwrap_or(pt.torrent),
            ..*pt
        };
        match store.promote(item.id, &t, unverified::SEARCHABLE, now)? {
            Promoted::Kept(id) => {
                stats.promoted.fetch_add(1, Ordering::Relaxed);
                if mismatch {
                    stats.mismatches.fetch_add(1, Ordering::Relaxed);
                }
                // As the row records it: the earlier discovery, and the
                // item's own source as `first_source`.
                let cohort = item.first_at.min(pt.discovered_at).div_euclid(3600);
                out = Promotion::Stored(id, true, Some((cohort, item.source)));
            }
            Promoted::Merged(id) => {
                stats.merged.fetch_add(1, Ordering::Relaxed);
                if matches!(out, Promotion::None) {
                    out = Promotion::Stored(id, false, None);
                }
            }
            Promoted::Held => return Ok(Promotion::Held),
            Promoted::Gone => {}
        }
    }
    Ok(out)
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
        results_named(NAME)
    }

    fn info_named(name: &str) -> Vec<u8> {
        to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(format!("{name}.mp4").into_bytes()),
                ),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ))
    }

    fn results_named(name: &str) -> Results {
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(format!("{name}.mp4").into_bytes()),
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
            source_adult: false,
        }
    }

    fn writer(store: Store, coordinator: Arc<Coordinator>, gate: Arc<GateStats>) -> Writer {
        Writer {
            store: Some(store),
            adult: AdultSources::default(),
            flood: Default::default(),
            enrichers: Default::default(),
            backfill_after: 0,
            backfill_done: true,
            backfill_backoff: Default::default(),
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
            // Off unless a test starts it.
            sweep: unverified::Sweep {
                done: true,
                ..Default::default()
            },
            verify: Default::default(),
            between: None,
            marked: None,
            crash_before_adult: false,
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

    /// A refetch the peer finder leased (a partial import's upgrade) of a
    /// torrent an always-adult source found before counts as that source's:
    /// the stored row's finders are read, not only the lease's.
    #[test]
    fn a_stored_rows_always_adult_finder_counts_at_commit() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 1).unwrap();
        let adult = store.register_source("adult", "1", 1).unwrap();
        let r = results();
        let lease = Lease {
            holder: WorkerId::LOCAL,
            token: Token(1),
            cohort: 0,
            source: dht,
            sources: bit(dht),
            strategy: 0,
            discovered_at: 0,
        };
        let mut w = writer(store, coordinator, gate);
        w.adult = AdultSources(bit(adult));
        let c = Committing {
            results: r,
            lease,
            leased: true,
            verdict: None,
            shadow: Vec::new(),
            sealed: None,
            publisher: None,
        };
        // Not stored yet, and only the DHT found it: not adult.
        assert!(!source_adult(w.store.as_ref().unwrap(), w.adult, &c, false));
        let store = w.store.as_mut().unwrap();
        store
            .write(&WriteBatch {
                prepared: &[PreparedTorrent {
                    torrent: &c.results.torrent,
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: 0,
                    source: adult,
                    verified: true,
                }],
                ..WriteBatch::default()
            })
            .unwrap();
        assert!(
            source_adult(w.store.as_ref().unwrap(), w.adult, &c, false),
            "the stored row's finder"
        );
        w.adult = AdultSources::default();
        assert!(
            !source_adult(w.store.as_ref().unwrap(), w.adult, &c, false),
            "none registered"
        );
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

    /// A commit that finds its row already stored (a partial import's
    /// upgrade rewrites it) marks the row's enrichment match for the
    /// backfill and starts the pass (ENR-1b).
    #[tokio::test]
    async fn a_rewritten_match_is_marked_at_commit() {
        use lodestar_store::claims::{AdultEnrichment, AdultKind, Claims};
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let enr = store.register_source("enr", "1", 1).unwrap();
        let mut w = writer(store, coordinator.clone(), gate);
        w.enrichers = crate::adult_enrich::EnrichSources::listed(bit(enr));
        let (retry, _retry_rx) = mpsc::channel(4);
        w.commit(
            vec![leased(&coordinator, results())],
            vec![],
            vec![],
            vec![],
            &retry,
        )
        .await;
        let store = w.store.as_mut().unwrap();
        let id = store.find(&results().key).unwrap().unwrap();
        let section = AdultEnrichment {
            kind: Some(AdultKind::Vr),
            ..AdultEnrichment::new("1")
        };
        let claims = Claims {
            adult: Some(section),
            ..Claims::default()
        };
        assert!(store.put_claims(id, enr, &claims, false).unwrap());
        let current = |w: &Writer| {
            w.store
                .as_ref()
                .unwrap()
                .enrichment(id)
                .unwrap()
                .unwrap()
                .version
                == lodestar_classify::ENRICHMENT_VERSION
        };
        assert!(current(&w));
        assert!(!w.reenrich.load(Ordering::Acquire));
        w.commit(
            vec![leased(&coordinator, results())],
            vec![],
            vec![],
            vec![],
            &retry,
        )
        .await;
        assert!(!current(&w), "marked for the backfill");
        assert!(w.reenrich.load(Ordering::Acquire));
    }

    /// The commit hook's store errors (ENR-1c): a failed claims read marks
    /// every rewritten row for the backfill and starts the pass, and with no
    /// adult-enrichment source listed the hook does nothing.
    #[tokio::test]
    async fn a_failed_match_read_marks_every_rewritten_row() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let enr = store.register_source("enr", "1", 1).unwrap();
        let mut w = writer(store, coordinator.clone(), gate);
        let (retry, _retry_rx) = mpsc::channel(4);
        let lease = |c: &Coordinator| vec![leased(c, results())];
        w.commit(lease(&coordinator), vec![], vec![], vec![], &retry)
            .await;
        let id = w
            .store
            .as_ref()
            .unwrap()
            .find(&results().key)
            .unwrap()
            .unwrap();
        let current = |w: &Writer| {
            w.store
                .as_ref()
                .unwrap()
                .enrichment(id)
                .unwrap()
                .unwrap()
                .version
                == lodestar_classify::ENRICHMENT_VERSION
        };
        // The claims read fails: its table is gone.
        w.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("DROP TABLE torrent_claims")
            .unwrap();
        // No adult-enrichment source listed: the hook doesn't run.
        w.commit(lease(&coordinator), vec![], vec![], vec![], &retry)
            .await;
        assert!(current(&w) && !w.reenrich.load(Ordering::Acquire));
        assert_eq!(w.stats.errors.load(Ordering::Relaxed), 0);
        // Listed: the read fails, so every rewritten row is marked.
        w.enrichers = crate::adult_enrich::EnrichSources::listed(bit(enr));
        w.commit(lease(&coordinator), vec![], vec![], vec![], &retry)
            .await;
        assert!(!current(&w), "marked for the backfill");
        assert!(w.reenrich.load(Ordering::Acquire));
        assert_eq!(w.stats.errors.load(Ordering::Relaxed), 1);
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

    /// An always-adult source's result re-judged at commit (a remote
    /// worker's, an older rule set's) is stored adult and judged as adult,
    /// and no Not adult classification rule takes that away.
    #[test]
    fn rejudging_keeps_an_always_adult_sources_result_adult() {
        let mut r = results();
        let detected = classify(&r.torrent.name, &r.files).content_type;
        assert_ne!(detected, ContentType::Adult);
        r.source_adult = true;
        let mut e = engine();
        let d = rejudge(&e, &mut r, 0, 1000).decision.expect("held");
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs5a));
        assert_eq!((r.fix, stored_type(&r)), (Some(true), ContentType::Adult));
        // A Not adult rule on its name: still adult, still held.
        e.load_class_rules(&[(1, "studiox".into(), false)]);
        let d = rejudge(&e, &mut r, 0, 1000).decision.expect("held");
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs5a));
        assert_eq!((r.fix, stored_type(&r)), (Some(true), ContentType::Adult));
        // The same result from a source that isn't: detection's, unheld.
        r.source_adult = false;
        assert_eq!(rejudge(&e, &mut r, 0, 1000).decision, None);
        assert_ne!(stored_type(&r), ContentType::Adult);
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
        let at = w.candidates.backoff.retry_at.expect("waits after an error");
        assert!(at >= before + RETRY_FIRST, "{at} {before}");
        assert!(!w.candidates.done && !w.candidates.ready());
        w.candidates.backoff.retry_due(at - 1);
        assert!(!w.candidates.ready(), "not before it's due");
        w.candidates.backoff.retry_due(at);
        assert!(w.candidates.ready());
        let before = unix_now();
        w.refile_candidates().await;
        let again = w.candidates.backoff.retry_at.expect("waits longer");
        assert!(again >= before + 2 * RETRY_FIRST, "{again} {before}");
        conn(&w, "DROP TABLE temp.import_candidates;");
        w.candidates.backoff.retry_due(again);
        sweep(&mut w).await;
        assert!(w.candidates.done && w.candidates.backoff.failures == 0);

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
        fn marked(store: &Store, held: &HeldSet, _: &Handle, hs: &[[u8; 20]]) {
            HOOKS.fetch_add(1, Ordering::Relaxed);
            assert!(!hs.is_empty() && held.pending_len() > 0);
            // Before C: the store doesn't hold them yet.
            assert!(
                store.held_among(hs).unwrap().iter().all(|h| !h),
                "marked before C"
            );
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

    /// A batch the store refuses or fails leaves no mark and no hold
    /// behind, in all three writer arms: the store holds nothing. The sweep
    /// sees the rules move between its mark and its commit, backfill the
    /// classification generation, and the ingest commit a failed write.
    #[tokio::test]
    async fn a_rolled_back_batch_leaves_nothing_refused() {
        fn change_rules(_: &Store, _: &HeldSet, filter: &Handle, _: &[[u8; 20]]) {
            filter.update_engine(|e| e.load_class_rules(&[(1, "studiox".into(), true)]));
        }
        fn bump_class_gen(store: &Store, _: &HeldSet, _: &Handle, _: &[[u8; 20]]) {
            store
                .connection()
                .execute(
                    "INSERT INTO settings (key, value) VALUES ('class_rules_gen', 99)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    [],
                )
                .unwrap();
        }
        let (mut w, _) = arms_writer();
        w.marked = Some(change_rules);
        w.refile_candidates().await;
        assert!(
            !w.held.contains(&[1; 20]),
            "sweep rolled back, so fetchable"
        );
        assert_eq!(w.held.pending_len(), 0);
        agrees_with_store(&w);

        let (mut w, stored) = arms_writer();
        w.marked = Some(bump_class_gen);
        w.backfill().await;
        assert!(!w.held.contains(&stored), "backfill refused, so fetchable");
        assert_eq!(w.held.pending_len(), 0);
        agrees_with_store(&w);

        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut w = writer(store, coordinator.clone(), Arc::new(GateStats::new(0)));
        w.filter
            .update_engine(|e| e.load_class_rules(&[(1, "studiox".into(), true)]));
        w.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        let r = leased(&coordinator, results());
        let h = r.key.0;
        let (retry, _rx) = mpsc::channel(4);
        w.commit(vec![r], vec![], vec![], vec![], &retry).await;
        assert_eq!(
            w.stats.errors.load(Ordering::Relaxed),
            1,
            "the write failed"
        );
        assert!(!w.held.contains(&h), "ingest failed, so fetchable");
        assert_eq!(w.held.pending_len(), 0);
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

    /// The sweeps' backoff doubles after each error in a row, up to its
    /// cap.
    #[test]
    fn the_backoff_doubles_to_a_cap() {
        let mut c = Backoff::default();
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

    /// A store error doesn't end backfill for the run (#282): it waits,
    /// longer after each error in a row, then takes the same batch again
    /// and finishes. While it waits, the run loop doesn't call it (no hot
    /// loop) and shuts down promptly.
    #[tokio::test]
    async fn backfill_backs_off_after_a_store_error_and_resumes() {
        let (mut w, stored) = arms_writer();
        let id = {
            let store = w.store.as_mut().unwrap();
            store.find(&InfoHash(stored)).unwrap().unwrap()
        };
        let conn = |w: &Writer, sql: &str| {
            w.store
                .as_ref()
                .unwrap()
                .connection()
                .execute_batch(sql)
                .unwrap();
        };
        // A temporary table shadows `torrent_parsed` without the column
        // the backfill's query reads: every batch fails.
        conn(&w, "CREATE TEMP TABLE torrent_parsed (x INTEGER);");
        // Partway through: the cursor is past the one row.
        w.backfill_after = id;
        let errors = w.stats.errors.load(Ordering::Relaxed);
        let before = unix_now();
        w.backfill().await;
        assert_eq!(w.stats.errors.load(Ordering::Relaxed), errors + 1);
        let at = w.backfill_backoff.retry_at.expect("waits after an error");
        assert!(at >= before + RETRY_FIRST, "{at} {before}");
        assert!(!w.backfill_done && !w.backfill_ready());
        assert_eq!(w.backfill_after, id, "where it left off");
        w.backfill_backoff.retry_due(at - 1);
        assert!(!w.backfill_ready(), "not before it's due");
        w.backfill_backoff.retry_due(at);
        assert!(w.backfill_ready());
        let before = unix_now();
        w.backfill().await;
        let again = w.backfill_backoff.retry_at.expect("waits longer");
        assert!(again >= before + 2 * RETRY_FIRST, "{again} {before}");
        assert!(!w.backfill_done);

        assert_eq!(w.backfill_after, id);

        // The error clears: it resumes past the cursor, where nothing is
        // left, rather than starting over.
        conn(&w, "DROP TABLE temp.torrent_parsed;");
        w.backfill_backoff.retry_due(again);
        w.backfill().await;
        assert!(w.backfill_done && w.backfill_backoff.failures == 0);
        assert_eq!(w.stats.enriched.load(Ordering::Relaxed), 0, "not redone");

        // Waiting out an error (the loop's branch is off, as checked
        // above), shutdown doesn't wait for the backoff.
        conn(&w, "CREATE TEMP TABLE torrent_parsed (x INTEGER);");
        w.backfill_after = 0;
        w.backfill_done = false;
        w.candidates.done = true;
        w.backfill().await;
        assert!(!w.backfill_ready());
        let (results_tx, results) = mpsc::channel(1);
        let (touches_tx, touches) = mpsc::channel(1);
        let (queues_tx, queues) = mpsc::channel(1);
        let (swarm_tx, swarm) = mpsc::channel(1);
        let (retry, _retry_rx) = mpsc::channel(1);
        let (finish_tx, finish) = oneshot::channel();
        let (claims_tx, claims) = mpsc::channel(1);
        let run = tokio::spawn(w.run(WriterIo {
            results,
            touches,
            queues,
            retry,
            finish,
            swarm,
            claims,
            cache: mpsc::channel(1).1,
        }));
        drop((results_tx, touches_tx, queues_tx, swarm_tx, claims_tx));
        finish_tx.send(Vec::new()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("prompt shutdown")
            .unwrap();
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

    mod promotion {
        use super::*;
        use lodestar_classify::JunkReason;
        use lodestar_store::claims::{ClaimedFile, Claims};
        use lodestar_store::hot::Col;
        use lodestar_store::unverified::{NewUnverified, Put};

        fn sum(store: &Store, metric: u32) -> i64 {
            store
                .connection()
                .query_row(
                    "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = ?1",
                    [metric],
                    |r| r.get(0),
                )
                .unwrap()
        }

        /// A store with the DHT (0) and a feed (1), and an unverified item
        /// from the feed under `key`, claiming files the torrent hasn't.
        fn with_item(key: [u8; 20]) -> (Store, i64) {
            let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
            store.register_source("dht", "1", 0).unwrap();
            let feed = store.register_source("feed", "1", 0).unwrap();
            let claims = Claims {
                name: Some("Claimed name".into()),
                files: vec![ClaimedFile {
                    path: "Other.mkv".into(),
                    size: Some(5),
                }],
                ..Claims::default()
            };
            let put = store
                .put_unverified(&[NewUnverified {
                    network: Network::Bt,
                    key: &key,
                    source: feed,
                    private_source: false,
                    claims: Some(&claims),
                    at: 100,
                }])
                .unwrap();
            let Put::New(id) = put[0] else {
                panic!("{put:?}")
            };
            (store, id)
        }

        /// The duplicate-insert gate (plan SRC-2): the crawler (or a
        /// verification fetch) delivers the hash a source found first. It
        /// promotes the item under the item's id, never a second row; the
        /// claims move, a contradicting file list only adds the junk
        /// reason, and Insights counts it once, so deleting it later
        /// reconciles.
        #[tokio::test]
        async fn a_fetched_unverified_item_is_promoted_under_its_own_id() {
            let funnel = Arc::new(FunnelCounts::default());
            let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
            let gate = Arc::new(GateStats::new(0));
            let r = results();
            let key = r.key;
            let (mut store, id) = with_item(key.0);
            // The index is already past the item's id (later torrents were
            // indexed), so `record_searchable` would never count it.
            store
                .record_searchable(id + 10, 7200, unverified::SEARCHABLE)
                .unwrap();
            assert_eq!(sum(&store, metric::NEWLY_SEARCHABLE), 0);
            let hot = Arc::new(HotColumns::new());
            let mut w = writer(store, coordinator.clone(), gate.clone());
            w.hot = Some(hot.clone());
            let published = Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = published.clone();
            w.stats
                .unverified
                .promoted_hook
                .set(move |ids| sink.lock().unwrap().extend_from_slice(ids));
            let (retry, _rx) = mpsc::channel(4);
            w.commit(
                vec![leased(&coordinator, r)],
                vec![],
                vec![],
                vec![],
                &retry,
            )
            .await;

            let u = &w.stats.unverified;
            assert_eq!(u.promoted.load(Ordering::Relaxed), 1);
            assert_eq!(u.mismatches.load(Ordering::Relaxed), 1);
            assert_eq!(w.stats.stored.load(Ordering::Relaxed), 1);
            assert_eq!(gate.new_verified(Origin::Network), 1);
            assert_ne!(hot.get(Col::LastSeen, id as u64), lodestar_store::NEVER);
            let store = w.store.as_mut().unwrap();
            assert_eq!(store.find(&key).unwrap(), Some(id), "the item's id");
            assert_eq!(*published.lock().unwrap(), vec![id], "published");
            assert!(store.unverified(Network::Bt, &key.0).unwrap().is_none());
            assert_eq!(store.torrent_claims(id).unwrap().len(), 1);
            // Verified metadata stands; the mismatch is a junk reason.
            let (name, junk): (String, i64) = store
                .connection()
                .query_row(
                    "SELECT name, junk_score FROM torrents WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(name, format!("{NAME}.mp4"));
            assert!(junk >= i64::from(JunkReason::ClaimMismatch.weight()));
            let data: Vec<u8> = store
                .connection()
                .query_row(
                    "SELECT data FROM torrent_parsed WHERE torrent_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            let record = Record::decode(&data).unwrap();
            assert!(record.junk.reasons.contains(&JunkReason::ClaimMismatch));
            // Counted once as newly searchable; its deletion as removed.
            assert_eq!(sum(store, metric::NEWLY_SEARCHABLE), 1);
            store.delete_torrents(&[key.0]).unwrap();
            assert_eq!(sum(store, metric::REMOVED), 1);
            let left: i64 = store
                .connection()
                .query_row("SELECT count(*) FROM torrents", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                sum(store, metric::NEWLY_SEARCHABLE) - sum(store, metric::REMOVED),
                left
            );
            // It's in `reindex_log`, so the index replays it.
            let replay = store.reindex_after(0, 10).unwrap();
            assert!(replay.iter().any(|&(_, t)| t == id));
        }

        /// A torrent stored under the hash before the writer saw the item
        /// (a crawled row the writer couldn't merge yet): the item merges
        /// into it, and nothing is stored twice.
        #[tokio::test]
        async fn a_crawled_duplicate_merges() {
            let funnel = Arc::new(FunnelCounts::default());
            let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
            let gate = Arc::new(GateStats::new(0));
            let r = results();
            let key = r.key;
            let (mut store, item) = with_item(key.0);
            let dht = SourceId::new(0).unwrap();
            let existing = store
                .write(&WriteBatch {
                    prepared: &[PreparedTorrent {
                        torrent: &r.torrent,
                        discovered_at: 0,
                        metadata_at: 0,
                        extra_flags: 0,
                        source: dht,
                        verified: true,
                    }],
                    ..WriteBatch::default()
                })
                .unwrap()
                .prepared[0]
                .id();
            assert_ne!(existing, item);
            let mut w = writer(store, coordinator.clone(), gate.clone());
            let (retry, _rx) = mpsc::channel(4);
            w.commit(
                vec![leased(&coordinator, r)],
                vec![],
                vec![],
                vec![],
                &retry,
            )
            .await;
            assert_eq!(w.stats.unverified.merged.load(Ordering::Relaxed), 1);
            assert_eq!(gate.excluded(Excluded::AlreadyStored), 1);
            let store = w.store.as_ref().unwrap();
            assert_eq!(store.find(&key).unwrap(), Some(existing));
            assert!(store.unverified(Network::Bt, &key.0).unwrap().is_none());
            assert_eq!(store.torrent_claims(existing).unwrap().len(), 1);
            let rows: i64 = store
                .connection()
                .query_row("SELECT count(*) FROM torrents", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1);
        }

        /// The running writer stores discoveries with claims, and its sweep
        /// judges stored ones again at start.
        #[tokio::test]
        async fn the_writer_stores_claims_and_sweeps_at_start() {
            let funnel = Arc::new(FunnelCounts::default());
            let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
            let (mut store, _) = with_item([0x11; 20]);
            let feed = SourceId::new(1).unwrap();
            // Stored before a rule that now drops it.
            let claims = Claims {
                name: Some("Badword film".into()),
                ..Claims::default()
            };
            store
                .put_unverified(&[NewUnverified {
                    network: Network::Bt,
                    key: &[0x22; 20],
                    source: feed,
                    private_source: false,
                    claims: Some(&claims),
                    at: 100,
                }])
                .unwrap();
            let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
            w.filter.update_engine(|e| {
                let notes = e.load_rules(&lodestar_store::RuleSet {
                    rules: vec![lodestar_store::RuleRow {
                        id: 1,
                        query: "badword".into(),
                        action: Action::Drop.code(),
                        enabled: true,
                        shadow_until: None,
                        unless_query: None,
                    }],
                    ..Default::default()
                });
                assert!(notes.is_empty());
            });
            w.sweep = unverified::Sweep::default();
            let stats = w.stats.clone();
            let (results_tx, results) = mpsc::channel(1);
            let (touches_tx, touches) = mpsc::channel(1);
            let (queues_tx, queues) = mpsc::channel(1);
            let (swarm_tx, swarm) = mpsc::channel(1);
            let (retry, _retry_rx) = mpsc::channel(1);
            let (finish_tx, finish) = oneshot::channel();
            let (claims_tx, claims_rx) = mpsc::channel(4);
            let found = lodestar_sources::Found::key(InfoHash([0x33; 20]), 0).with_claims(
                lodestar_sources::Claims {
                    name: Some("Plain film".into()),
                    ..Default::default()
                },
            );
            let budget = Arc::new(tokio::sync::Semaphore::new(4));
            let permit = budget.clone().acquire_owned().await.unwrap();
            claims_tx
                .send((
                    Discovery {
                        source: feed,
                        found,
                        at: 100,
                        reply: false,
                    },
                    permit,
                ))
                .await
                .unwrap();
            let run = tokio::spawn(w.run(WriterIo {
                results,
                touches,
                queues,
                retry,
                finish,
                swarm,
                claims: claims_rx,
                cache: mpsc::channel(1).1,
            }));
            // The sweep runs between inputs; wait for it with a deadline.
            let deadline = Instant::now() + Duration::from_secs(10);
            while stats.unverified.sweeps.load(Ordering::Relaxed) == 0
                || stats.unverified.stored.load(Ordering::Relaxed) == 0
            {
                assert!(Instant::now() < deadline, "the sweep and the claims");
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            drop((results_tx, touches_tx, queues_tx, swarm_tx, claims_tx));
            finish_tx.send(Vec::new()).unwrap();
            let store = tokio::time::timeout(Duration::from_secs(10), run)
                .await
                .unwrap()
                .unwrap();
            assert!(
                store
                    .unverified(Network::Bt, &[0x22; 20])
                    .unwrap()
                    .is_none()
            );
            // Your rule drops the claims and holds nothing (runner, #383).
            assert!(!store.held_among(&[[0x22; 20]]).unwrap()[0]);
            assert!(
                store
                    .unverified(Network::Bt, &[0x33; 20])
                    .unwrap()
                    .is_some()
            );
            assert!(
                store
                    .unverified(Network::Bt, &[0x11; 20])
                    .unwrap()
                    .is_some()
            );
        }

        fn claimed(key: [u8; 20], name: &str) -> unverified::Claimed {
            let budget = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = budget.try_acquire_owned().unwrap();
            let found = lodestar_sources::Found::key(InfoHash(key), 0).with_claims(
                lodestar_sources::Claims {
                    name: Some(name.into()),
                    ..Default::default()
                },
            );
            let d = Discovery {
                source: SourceId::new(1).unwrap(),
                found,
                at: 100,
                reply: false,
            };
            (d, permit)
        }

        fn drop_rule(filter: &Handle, query: &str) {
            filter.update_engine(|e| {
                let notes = e.load_rules(&lodestar_store::RuleSet {
                    rules: vec![lodestar_store::RuleRow {
                        id: 1,
                        query: query.into(),
                        action: Action::Drop.code(),
                        enabled: true,
                        shadow_until: None,
                        unless_query: None,
                    }],
                    ..Default::default()
                });
                assert!(notes.is_empty(), "{notes:?}");
            });
        }

        /// Review B1: a claim holds a hash while its fetch is in flight.
        /// The verified metadata passes, but nothing is stored under the
        /// held hash.
        #[tokio::test]
        async fn a_hash_a_claim_held_after_judging_is_never_stored() {
            let funnel = Arc::new(FunnelCounts::default());
            let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
            let gate = Arc::new(GateStats::new(0));
            let (store, _) = with_item([0x11; 20]);
            let mut w = writer(store, coordinator.clone(), gate.clone());
            let r = leased(&coordinator, results());
            let key = r.key;
            w.ingest_claims(vec![claimed(key.0, "Rolea Lexa clip")])
                .await;
            assert!(w.held.contains(&key.0));
            let (retry, _rx) = mpsc::channel(4);
            w.commit(vec![r], vec![], vec![], vec![], &retry).await;
            let store = w.store.as_ref().unwrap();
            assert_eq!(store.find(&key).unwrap(), None, "not stored");
            assert_eq!(gate.excluded(Excluded::Filtered), 1);
            assert_eq!(w.stats.stored.load(Ordering::Relaxed), 0);
        }

        /// Review B2: a rule added after an item was stored, before the
        /// sweep reached it, still decides at promotion. The core holds
        /// the hash and nothing is stored; your rule only drops the claims
        /// (they never override verified metadata), so the torrent is
        /// stored under a new id without them.
        #[tokio::test]
        async fn promotion_judges_the_claims_under_the_current_rules() {
            for core in [true, false] {
                let funnel = Arc::new(FunnelCounts::default());
                let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
                let r = results();
                let key = r.key;
                let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
                store.register_source("dht", "1", 0).unwrap();
                let feed = store.register_source("feed", "1", 0).unwrap();
                let claims = Claims {
                    name: Some("Badword Codez clip".into()),
                    ..Claims::default()
                };
                let put = store
                    .put_unverified(&[NewUnverified {
                        network: Network::Bt,
                        key: &key.0,
                        source: feed,
                        private_source: false,
                        claims: Some(&claims),
                        at: 100,
                    }])
                    .unwrap();
                let Put::New(item) = put[0] else { panic!() };
                let mut w = writer(store, coordinator.clone(), Arc::new(GateStats::new(0)));
                if core {
                    w.filter.update(|c| {
                        let mut lists = c.lists().clone();
                        lists.codes = TermSet::of(&["codez"]);
                        c.set_lists(lists);
                    });
                } else {
                    drop_rule(&w.filter, "badword");
                }
                let (retry, _rx) = mpsc::channel(4);
                w.commit(
                    vec![leased(&coordinator, r)],
                    vec![],
                    vec![],
                    vec![],
                    &retry,
                )
                .await;
                let u = &w.stats.unverified;
                let store = w.store.as_ref().unwrap();
                assert!(store.unverified(Network::Bt, &key.0).unwrap().is_none());
                assert_eq!(u.promoted.load(Ordering::Relaxed), 0, "core {core}");
                if core {
                    assert_eq!(u.held.load(Ordering::Relaxed), 1);
                    assert!(store.held_among(&[key.0]).unwrap()[0]);
                    assert!(w.held.contains(&key.0));
                    assert_eq!(store.find(&key).unwrap(), None);
                    agrees_with_store(&w);
                } else {
                    assert_eq!(u.dropped.load(Ordering::Relaxed), 1);
                    let id = store.find(&key).unwrap().expect("stored");
                    assert_ne!(id, item, "a new id: the claims were dropped");
                    assert!(store.torrent_claims(id).unwrap().is_empty());
                    assert!(!store.held_among(&[key.0]).unwrap()[0]);
                }
            }
        }

        /// The pre-commit marks (`2026-10-31-B-held-precommit.md`) on the
        /// three paths SRC-2 adds that hold: a claim at ingest, the refile
        /// sweep, and a promotion. Each marks its hashes before its commit,
        /// and afterwards intake refuses exactly what the store holds.
        #[tokio::test]
        async fn claim_holds_are_refused_from_before_their_commit() {
            use std::sync::atomic::AtomicUsize;
            static HOOKS: AtomicUsize = AtomicUsize::new(0);
            fn marked(store: &Store, held: &HeldSet, _: &Handle, hs: &[[u8; 20]]) {
                HOOKS.fetch_add(1, Ordering::Relaxed);
                assert!(!hs.is_empty() && held.pending_len() > 0);
                assert!(
                    store.held_among(hs).unwrap().iter().all(|h| !h),
                    "marked before C"
                );
                for h in hs {
                    assert!(held.contains(h), "pending, so refused");
                }
            }
            fn between(store: &mut Store, held: &HeldSet, _: &Handle) {
                HOOKS.fetch_add(1, Ordering::Relaxed);
                for (h, _) in store.filter_state().unwrap().held {
                    assert!(held.contains(&h), "committed, so refused");
                }
            }
            for arm in 0..3 {
                HOOKS.store(0, Ordering::Relaxed);
                let funnel = Arc::new(FunnelCounts::default());
                let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
                let r = results();
                let key = r.key.0;
                let (store, _) = with_item(key);
                let mut w = writer(store, coordinator.clone(), Arc::new(GateStats::new(0)));
                w.marked = Some(marked);
                w.between = Some(between);
                let h = match arm {
                    0 => {
                        w.ingest_claims(vec![claimed([0x44; 20], "Rolea Lexa clip")])
                            .await;
                        [0x44; 20]
                    }
                    1 => {
                        // The item claims "Claimed name": the core now
                        // holds it.
                        w.filter.update(|c| {
                            let mut lists = c.lists().clone();
                            lists.codes = TermSet::of(&["claimed"]);
                            c.set_lists(lists);
                        });
                        w.sweep = unverified::Sweep::default();
                        while !w.sweep.done {
                            w.refile_unverified().await;
                        }
                        key
                    }
                    _ => {
                        w.filter.update(|c| {
                            let mut lists = c.lists().clone();
                            lists.codes = TermSet::of(&["claimed"]);
                            c.set_lists(lists);
                        });
                        let (retry, _rx) = mpsc::channel(4);
                        w.commit(
                            vec![leased(&coordinator, r)],
                            vec![],
                            vec![],
                            vec![],
                            &retry,
                        )
                        .await;
                        key
                    }
                };
                assert!(
                    HOOKS.load(Ordering::Relaxed) >= 2,
                    "arm {arm}: both hooks ran"
                );
                assert!(w.held.contains(&h), "arm {arm}: held");
                assert_eq!(w.held.pending_len(), 0, "arm {arm}: no mark left");
                agrees_with_store(&w);
            }
        }

        /// Claims already handed to the writer when the pipeline stops are
        /// judged and stored, not lost.
        #[tokio::test]
        async fn claims_waiting_at_shutdown_are_stored() {
            let funnel = Arc::new(FunnelCounts::default());
            let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
            let (store, _) = with_item([0x11; 20]);
            let w = writer(store, coordinator, Arc::new(GateStats::new(0)));
            let (results_tx, results) = mpsc::channel(1);
            let (touches_tx, touches) = mpsc::channel(1);
            let (queues_tx, queues) = mpsc::channel(1);
            let (swarm_tx, swarm) = mpsc::channel(1);
            let (retry, _retry_rx) = mpsc::channel(1);
            let (finish_tx, finish) = oneshot::channel();
            let (claims_tx, claims) = mpsc::channel(8);
            for k in 0x50..0x54u8 {
                claims_tx
                    .send(claimed([k; 20], "Plain film"))
                    .await
                    .unwrap();
            }
            drop((results_tx, touches_tx, queues_tx, swarm_tx, claims_tx));
            finish_tx.send(Vec::new()).unwrap();
            let store = tokio::time::timeout(
                Duration::from_secs(10),
                tokio::spawn(w.run(WriterIo {
                    results,
                    touches,
                    queues,
                    retry,
                    finish,
                    swarm,
                    claims,
                    cache: mpsc::channel(1).1,
                })),
            )
            .await
            .unwrap()
            .unwrap();
            for k in 0x50..0x54u8 {
                assert!(store.unverified(Network::Bt, &[k; 20]).unwrap().is_some());
            }
        }
    }

    // ---- SRC-13b: the crash window, and the lease-free entry ----

    fn stored_by(w: &mut Writer, name: &str, source: SourceId) -> (i64, InfoHash) {
        let r = results_named(name);
        let store = w.store.as_mut().unwrap();
        let written = store
            .write(&WriteBatch {
                prepared: &[PreparedTorrent {
                    torrent: &r.torrent,
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: 0,
                    source,
                    verified: true,
                }],
                ..WriteBatch::default()
            })
            .unwrap();
        (written.prepared[0].stored().unwrap(), r.key)
    }

    fn sighting(source: SourceId, key: InfoHash) -> Discovery {
        Discovery {
            source,
            found: lodestar_sources::Found::key(key, 0),
            at: unix_now(),
            reply: false,
        }
    }

    /// The #467 review's crash window: the commit that writes an always-
    /// adult source's bit marks the row for the backfill in the same
    /// transaction, so a failure before `adult_by_source` (simulated here)
    /// leaves it marked, and the backfill classifies it adult.
    #[tokio::test]
    async fn a_crash_before_the_adult_step_leaves_the_row_marked_for_the_backfill() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 1).unwrap();
        let adult = store.register_source("adult", "1", 1).unwrap();
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        w.adult = AdultSources(bit(adult));
        w.arm_adult_mark();
        let (retry, _rx) = mpsc::channel(4);
        let adult_code = Some(ContentType::Adult.code());
        let (crashed, crashed_key) = stored_by(&mut w, "Garden Tour 2022", dht);
        let (fine, fine_key) = stored_by(&mut w, "Garden Tour 2023", dht);
        let marked = |w: &Writer| {
            w.store
                .as_ref()
                .unwrap()
                .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
                .unwrap()
        };
        assert!(marked(&w).is_empty());

        // The always-adult source sees one; the step after the commit fails.
        w.crash_before_adult = true;
        w.commit(
            vec![],
            vec![sighting(adult, crashed_key)],
            vec![],
            vec![],
            &retry,
        )
        .await;
        let store = w.store.as_ref().unwrap();
        assert_ne!(store.content_type(crashed).unwrap(), adult_code);
        assert_eq!(marked(&w), [crashed], "marked by the commit itself");
        // The backfill finds the mark and applies the same correction.
        w.backfill_done = false;
        w.backfill().await;
        let store = w.store.as_ref().unwrap();
        assert_eq!(store.content_type(crashed).unwrap(), adult_code);
        assert!(marked(&w).is_empty());

        // With no failure, the step after the commit classifies it at once
        // and its write clears the mark.
        w.crash_before_adult = false;
        w.commit(
            vec![],
            vec![sighting(adult, fine_key)],
            vec![],
            vec![],
            &retry,
        )
        .await;
        let store = w.store.as_ref().unwrap();
        assert_eq!(store.content_type(fine).unwrap(), adult_code);
        assert!(marked(&w).is_empty());
    }

    fn drop_rule(filter: &Handle, query: &str) {
        filter.update_engine(|e| {
            let notes = e.load_rules(&lodestar_store::RuleSet {
                rules: vec![lodestar_store::RuleRow {
                    id: 1,
                    query: query.into(),
                    action: Action::Drop.code(),
                    enabled: true,
                    shadow_until: None,
                    unless_query: None,
                }],
                ..Default::default()
            });
            assert!(notes.is_empty(), "{notes:?}");
            e.version += 1;
        });
    }

    fn cached(w: &Writer, name: &str, source: SourceId) -> crate::torrent_cache::ToCommit {
        let raw = info_named(name);
        let key = lodestar_metainfo::Info::parse(&raw).unwrap().primary_hash();
        let mut preparer = w.store.as_ref().unwrap().preparer();
        let judged =
            crate::torrent_cache::judge(&w.filter.current(), &mut preparer, key, &raw, unix_now());
        crate::torrent_cache::ToCommit {
            source,
            judged,
            claims: Default::default(),
            provenance: Default::default(),
            at: unix_now(),
            info_raw: raw,
            key,
        }
    }

    /// A cache's file admitted under rules that changed before its commit
    /// is judged again by the writer: a drop rule added since holds it.
    /// One held under rules that changed since is judged again too: with
    /// the rule gone, it's stored; with it still there, it's held.
    #[tokio::test]
    async fn the_writer_judges_a_stale_cache_file_again() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let cache = store.register_source("torrent_cache", "1", 1).unwrap();
        let mut w = writer(store, coordinator, gate.clone());
        let (retry, _rx) = mpsc::channel(4);
        let admitted = cached(&w, "Big Buck Bunny", cache);
        assert!(matches!(
            admitted.judged,
            crate::torrent_cache::Judged::Store(_)
        ));
        let key = match &admitted.judged {
            crate::torrent_cache::Judged::Store(a) => a.key,
            _ => unreachable!(),
        };
        drop_rule(&w.filter, "bunny");
        w.commit_cache(vec![admitted], &retry).await;
        let cs = &w.stats.torrent_cache;
        assert_eq!(cs.held.load(Ordering::Relaxed), 1);
        assert_eq!(cs.stored.load(Ordering::Relaxed), 0);
        let store = w.store.as_ref().unwrap();
        assert!(store.find(&key).unwrap().is_none());
        assert_eq!(store.held_among(&[key.0]).unwrap(), [true]);
        // Never the crawl's: the gate saw nothing.
        assert_eq!(gate.excluded(Excluded::Filtered), 0);

        // Held under the drop rule, then the rules change with the rule
        // still there: judged again, and held.
        let held = cached(&w, "Bunny Returns", cache);
        let held_key = match &held.judged {
            crate::torrent_cache::Judged::Held { hashes, .. } => InfoHash(hashes[0]),
            j => panic!("not held: {j:?}"),
        };
        w.filter.update_engine(|e| e.version += 1);
        w.commit_cache(vec![held], &retry).await;
        let cs = &w.stats.torrent_cache;
        assert_eq!(cs.stale.load(Ordering::Relaxed), 1);
        assert_eq!(cs.held.load(Ordering::Relaxed), 2);
        let store = w.store.as_ref().unwrap();
        assert_eq!(store.held_among(&[held_key.0]).unwrap(), [true]);

        // Held under the drop rule, then the rule goes: judged again under
        // the rules as they are now, and stored.
        let held = cached(&w, "Bunny Again", cache);
        let again_key = match &held.judged {
            crate::torrent_cache::Judged::Held { hashes, .. } => InfoHash(hashes[0]),
            j => panic!("not held: {j:?}"),
        };
        w.filter.update_engine(|e| {
            e.load_rules(&lodestar_store::RuleSet::default());
            e.version += 1;
        });
        w.commit_cache(vec![held], &retry).await;
        let cs = &w.stats.torrent_cache;
        assert_eq!(cs.stale.load(Ordering::Relaxed), 2);
        assert_eq!(cs.held.load(Ordering::Relaxed), 2);
        assert_eq!(cs.stored.load(Ordering::Relaxed), 1);
        let store = w.store.as_ref().unwrap();
        assert!(store.find(&again_key).unwrap().is_some());
        assert_eq!(store.held_among(&[again_key.0]).unwrap(), [false]);
    }

    /// A clean cache file with no item under its key is stored as a new
    /// row from the plugin's source, outside the funnel and the gate.
    #[tokio::test]
    async fn a_cache_file_is_stored_from_the_plugins_source_without_a_lease() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let gate = Arc::new(GateStats::new(0));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let cache = store.register_source("torrent_cache", "1", 1).unwrap();
        let mut w = writer(store, coordinator.clone(), gate.clone());
        let (retry, _rx) = mpsc::channel(4);
        let c = cached(&w, "Sintel 2010", cache);
        w.commit_cache(vec![c], &retry).await;
        assert_eq!(w.stats.torrent_cache.stored.load(Ordering::Relaxed), 1);
        assert_eq!(w.stats.stored.load(Ordering::Relaxed), 0);
        assert_eq!(gate.new_verified(Origin::Network), 0);
        assert!(funnel.drain().is_empty(), "nothing in the funnel");
        let store = w.store.as_ref().unwrap();
        let key = lodestar_metainfo::Info::parse(&info_named("Sintel 2010"))
            .unwrap()
            .primary_hash();
        let id = store.find(&key).unwrap().unwrap();
        let row = store.torrent(id).unwrap().unwrap();
        assert_eq!(row.sources, bit(cache));
        // Kept out of the crawl's cohorts, live and in `rollup --repair`.
        assert_ne!(row.flags & flags::IMPORTED, 0);
        assert!(w.have.contains(&key));
    }

    /// The writer's own judge applies the always-adult mask to a cache's
    /// file inside the commit: an always-adult source's item is promoted
    /// Adult before the adult step after the commit runs (here it never
    /// does, as after a crash).
    #[tokio::test]
    async fn a_cache_file_for_an_always_adult_item_is_committed_adult() {
        use lodestar_store::unverified::NewUnverified;
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let adult = store.register_source("adult", "1", 1).unwrap();
        let cache = store.register_source("torrent_cache", "1", 1).unwrap();
        let key = lodestar_metainfo::Info::parse(&info_named("Garden Tour 2022"))
            .unwrap()
            .primary_hash();
        let claims = lodestar_store::claims::Claims {
            name: Some("Clips".into()),
            ..Default::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &key.0,
                source: adult,
                private_source: false,
                claims: Some(&claims),
                at: 1,
            }])
            .unwrap();
        let item = store.unverified(Network::Bt, &key.0).unwrap().unwrap().id;
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        w.adult = AdultSources(bit(adult));
        w.arm_adult_mark();
        w.crash_before_adult = true;
        let (retry, _rx) = mpsc::channel(4);
        let c = cached(&w, "Garden Tour 2022", cache);
        w.commit_cache(vec![c], &retry).await;
        let store = w.store.as_ref().unwrap();
        assert_eq!(store.find(&key).unwrap(), Some(item));
        assert_eq!(
            store.content_type(item).unwrap(),
            Some(ContentType::Adult.code())
        );
    }

    /// A held cache file for a stored partial import removes that row in
    /// the hold's transaction; a store error is counted as the cache's.
    #[tokio::test]
    async fn a_held_cache_file_removes_a_stored_import_and_errors_are_counted() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 1).unwrap();
        let cache = store.register_source("torrent_cache", "1", 1).unwrap();
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        let (id, key) = stored_by(&mut w, "Bunny Partial", dht);
        let (retry, _rx) = mpsc::channel(4);
        drop_rule(&w.filter, "bunny");
        let held = cached(&w, "Bunny Partial", cache);
        assert!(matches!(
            held.judged,
            crate::torrent_cache::Judged::Held { .. }
        ));
        w.commit_cache(vec![held], &retry).await;
        let store = w.store.as_ref().unwrap();
        assert!(store.torrent(id).unwrap().is_none(), "removed");
        assert_eq!(store.held_among(&[key.0]).unwrap(), [true]);
        let cs = &w.stats.torrent_cache;
        assert_eq!(cs.held.load(Ordering::Relaxed), 1);

        // A store that can't be written: the cache's file is lost, counted
        // as its error and not as the gate's.
        let clean = cached(&w, "Sintel 2010", cache);
        w.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        w.commit_cache(vec![clean], &retry).await;
        let cs = &w.stats.torrent_cache;
        assert_eq!(cs.errors.load(Ordering::Relaxed), 1);
        assert_eq!(cs.stored.load(Ordering::Relaxed), 0);
        assert_eq!(w.gate.excluded(Excluded::StoreFailed), 0);
    }

    /// The crash window on the claims path (runner's review of #479): an
    /// always-adult source's claims on a stored torrent set its bit in
    /// `put_unverified`'s transaction, which marks the row there, so a
    /// failure before `adult_by_source` (simulated) leaves it for the
    /// backfill.
    #[tokio::test]
    async fn a_crash_after_always_adult_claims_leaves_the_row_marked() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 1).unwrap();
        let adult = store.register_source("adult", "1", 1).unwrap();
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        w.adult = AdultSources(bit(adult));
        w.arm_adult_mark();
        let (id, key) = stored_by(&mut w, "Garden Tour 2024", dht);
        w.crash_before_adult = true;
        let claims = lodestar_sources::Claims {
            name: Some("Clips".into()),
            ..Default::default()
        };
        let d = Discovery {
            source: adult,
            found: lodestar_sources::Found::key(key, 0).with_claims(claims),
            at: unix_now(),
            reply: false,
        };
        w.ingest_found(vec![d], true).await;
        assert_eq!(w.stats.unverified.on_torrent.load(Ordering::Relaxed), 1);
        let adult_code = Some(ContentType::Adult.code());
        let unenriched = |w: &Writer| {
            w.store
                .as_ref()
                .unwrap()
                .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
                .unwrap()
        };
        assert_ne!(
            w.store.as_ref().unwrap().content_type(id).unwrap(),
            adult_code
        );
        assert_eq!(unenriched(&w), [id], "marked by the claims' transaction");
        w.backfill_done = false;
        w.backfill().await;
        assert_eq!(
            w.store.as_ref().unwrap().content_type(id).unwrap(),
            adult_code
        );
        assert!(unenriched(&w).is_empty());
    }

    /// A key a private source stored after the cache's lookup stays
    /// private: the commit reads the store's mark, never the lookup's.
    #[tokio::test]
    async fn a_cache_file_never_clears_a_private_mark_set_since_its_lookup() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let feed = store.register_source("private_feed", "1", 1).unwrap();
        let cache = store.register_source("torrent_cache", "1", 1).unwrap();
        let mut w = writer(store, coordinator, Arc::new(GateStats::new(0)));
        let (retry, _rx) = mpsc::channel(4);
        let mut c = cached(&w, "Sintel 2011", cache);
        // A file with the fields outside its info dict, so its claims are
        // stored too (runner's delta review of #479): they mustn't make it
        // public either.
        c.claims.trackers = vec!["udp://tracker.example:1337/announce".into()];
        c.claims.description = Some("Encoded by someone".into());
        c.provenance.published_at = Some(1_600_000_000);
        // Stored by a private source after the lookup, before the commit.
        let r = results_named("Sintel 2011");
        let id = w
            .store
            .as_mut()
            .unwrap()
            .write(&WriteBatch {
                prepared: &[PreparedTorrent {
                    torrent: &r.torrent,
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: flags::PRIVATE_SOURCE,
                    source: feed,
                    verified: true,
                }],
                ..WriteBatch::default()
            })
            .unwrap()
            .prepared[0]
            .stored()
            .unwrap();
        w.commit_cache(vec![c], &retry).await;
        assert_eq!(w.stats.torrent_cache.existing.load(Ordering::Relaxed), 1);
        let row = w.store.as_ref().unwrap().torrent(id).unwrap().unwrap();
        assert_ne!(row.flags & flags::PRIVATE_SOURCE, 0, "still private");
        assert_ne!(row.sources & bit(cache), 0);
        // The claims were stored, as a private source's are.
        assert_eq!(w.stats.unverified.on_torrent.load(Ordering::Relaxed), 1);
        let claims = w.store.as_ref().unwrap().torrent_claims(id).unwrap();
        let mine = claims.iter().find(|c| c.source == cache).expect("claims");
        assert!(mine.private_source);
    }

    // -----------------------------------------------------------------
    // SP-3 (RP-4): flooding, counted and judged at commit.

    /// An engine with every pack on and every rule applied, SP-3
    /// included, as on a fresh store.
    fn sp3_engine() -> Engine {
        use lodestar_filter::packs::manifest::Manifest;
        use lodestar_filter::packs::upgrade::{self, PackVersions, Rows};
        let mut e = engine();
        e.packs = lodestar_filter::PackSwitches::all();
        let record = upgrade::current_record(&Manifest::builtin()).encode();
        let rows = Rows {
            applied: [Some(Ok(record.clone())), Some(Ok(record))],
            marker: Some(Ok("1".into())),
        };
        e.pack_versions = PackVersions::resolve(&Manifest::builtin(), &rows);
        assert!(e.pack_versions.state(lodestar_filter::PackRule::Sp3).plain);
        // SP-3's fixtures share one payload (`results_named`), so once one
        // is demoted the rest are its clones: SP-4 is off here and has
        // tests of its own ([`sp4_writer`]).
        e.packs.rules_off.insert(lodestar_filter::PackRule::Sp4);
        e
    }

    /// The `n`th near-duplicate: episode, quality and group differ.
    fn episode(n: usize) -> String {
        let q = ["720p", "1080p", "480p"][n % 3];
        format!("Example Show S01E{:02} {q} HDTV x264-G{n}", n % 90 + 1)
    }

    fn committing(r: Results, source: SourceId) -> Committing {
        Committing {
            results: r,
            lease: Lease {
                holder: WorkerId::LOCAL,
                token: Token(0),
                cohort: 0,
                source,
                sources: bit(source),
                strategy: 0,
                discovered_at: 0,
            },
            leased: true,
            verdict: None,
            shadow: Vec::new(),
            sealed: None,
            publisher: None,
        }
    }

    /// The SP-3 decision each of `cs` gets, committed one by one.
    fn sp3_of(
        store: &Store,
        e: &Engine,
        flood: &mut Flooding,
        cs: Vec<Committing>,
    ) -> Vec<Option<Action>> {
        cs.into_iter()
            .map(|c| {
                let have = Have::new(1024);
                let mut out = judge(
                    store,
                    e,
                    AdultSources::default(),
                    None,
                    flood,
                    &have,
                    vec![c],
                );
                let c = out.pop().unwrap();
                c.verdict
                    .filter(|d| d.rule == RuleRef::Pack(lodestar_filter::PackRule::Sp3))
                    .map(|d| d.action)
            })
            .collect()
    }

    /// A feed's finds count against the feed; the DHT's never do; a
    /// publisher its claims name counts on its own, whichever source.
    #[test]
    fn sp3_counts_at_commit_per_publisher_else_per_non_dht_source() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let other = store.register_source("other", "1", 0).unwrap();
        let e = sp3_engine();
        let mut flood = Flooding {
            claims_from: bit(feed) | bit(other),
            per_source: bit(feed) | bit(other),
            clock: Some(10_000),
            ..Default::default()
        };
        let batch = |source, n: usize| -> Vec<Committing> {
            (0..n)
                .map(|i| committing(results_named(&episode(i)), source))
                .collect()
        };
        // The DHT: never.
        let got = sp3_of(&store, &e, &mut flood, batch(dht, 120));
        assert!(got.iter().all(Option::is_none));
        // A feed with no publisher: flagged from the 20th, demoted from
        // the 100th.
        let got = sp3_of(&store, &e, &mut flood, batch(feed, 120));
        assert!(got[..19].iter().all(Option::is_none), "{got:?}");
        assert!(got[19..99].iter().all(|a| *a == Some(Action::Flag)));
        assert!(got[99..].iter().all(|a| *a == Some(Action::Demote)));
        // Another source, its own count.
        let got = sp3_of(&store, &e, &mut flood, batch(other, 19));
        assert!(got.iter().all(Option::is_none));
        // A cache's publisher counts against the publisher, not the
        // source: the same source's 20th with another publisher passes.
        let named = |p: &str, n: usize| -> Vec<Committing> {
            (0..n)
                .map(|i| {
                    // A cache's lease-free file, naming its publisher.
                    let mut c = committing(results_named(&episode(i)), other);
                    c.leased = false;
                    c.publisher = Some(p.into());
                    c
                })
                .collect()
        };
        let got = sp3_of(&store, &e, &mut flood, named("uploader", 20));
        assert_eq!(got[19], Some(Action::Flag));
        // A cache's file naming no publisher counts against no one, even
        // from a source that counts.
        let cache: Vec<Committing> = (0..120)
            .map(|i| {
                let mut c = committing(results_named(&episode(i)), feed);
                c.leased = false;
                c
            })
            .collect();
        let before = flood.map.len();
        assert!(
            sp3_of(&store, &e, &mut flood, cache)
                .iter()
                .all(Option::is_none)
        );
        assert_eq!(flood.map.len(), before);
        assert_eq!(
            sp3_of(&store, &e, &mut flood, named("someone", 1)),
            [None],
            "another publisher"
        );
        // An hour on (the injected clock): forgotten.
        flood.clock = Some(10_000 + lodestar_filter::flood::WINDOW_SECS);
        assert_eq!(sp3_of(&store, &e, &mut flood, batch(feed, 1)), [None]);
    }

    /// A fetched torrent whose unverified item holds a feed's claims
    /// counts against the publisher those claims name.
    #[test]
    fn sp3_reads_the_publisher_from_the_unverified_item() {
        use lodestar_store::unverified::NewUnverified;
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let e = sp3_engine();
        let mut flood = Flooding {
            claims_from: bit(feed),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        let claims = lodestar_store::claims::Claims {
            name: Some("x".into()),
            publisher: Some("uploader".into()),
            ..Default::default()
        };
        let rs: Vec<Results> = (0..25).map(|i| results_named(&episode(i))).collect();
        let items: Vec<NewUnverified<'_>> = rs
            .iter()
            .map(|r| NewUnverified {
                network: Network::Bt,
                key: &r.key.0,
                source: feed,
                private_source: false,
                claims: Some(&claims),
                at: 0,
            })
            .collect();
        store.put_unverified(&items).unwrap();
        let cs = rs.into_iter().map(|r| committing(r, feed)).collect();
        let got = sp3_of(&store, &e, &mut flood, cs);
        assert!(got[..19].iter().all(Option::is_none));
        assert!(got[19..].iter().all(|a| *a == Some(Action::Flag)));
        // Counted against the publisher, not the feed.
        let sig = lodestar_filter::flood::signature("Example Show").unwrap();
        use lodestar_filter::flood::Origin;
        assert_eq!(
            flood
                .map
                .count(Origin::Publisher(feed.get(), "uploader"), sig, 10_000),
            25
        );
        assert_eq!(flood.map.count(Origin::Source(feed.get()), sig, 10_000), 0);
    }

    /// A non-live source's claim without a note is never read, so it
    /// never counts (runner, #495); the same items found by a live feed
    /// too are read and count against the publisher.
    #[test]
    fn sp3_reads_a_non_live_claim_only_with_a_live_feed() {
        use lodestar_filter::flood::Origin;
        use lodestar_store::unverified::NewUnverified;
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let scrape = store.register_source("full_scrape", "1", 0).unwrap();
        let e = sp3_engine();
        let flooding = || Flooding {
            claims_from: bit(feed) | bit(scrape),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        let claims = lodestar_store::claims::Claims {
            name: Some("x".into()),
            publisher: Some("uploader".into()),
            ..Default::default()
        };
        let rs: Vec<Results> = (0..25).map(|i| results_named(&episode(i))).collect();
        let items: Vec<NewUnverified<'_>> = rs
            .iter()
            .map(|r| NewUnverified {
                network: Network::Bt,
                key: &r.key.0,
                source: scrape,
                private_source: false,
                claims: Some(&claims),
                at: 0,
            })
            .collect();
        store.put_unverified(&items).unwrap();
        let sig = lodestar_filter::flood::signature("Example Show").unwrap();
        let commits = |live: bool| -> Vec<Committing> {
            (0..25)
                .map(|i| {
                    let mut c = committing(results_named(&episode(i)), scrape);
                    // Only the full scrape saw it (`results_named` names
                    // source 0, the feed, here).
                    c.results.sources = bit(scrape);
                    if live {
                        c.lease.sources |= bit(feed);
                    }
                    c
                })
                .collect()
        };
        // Found by the full scrape alone, with no note: never read.
        let mut flood = flooding();
        let got = sp3_of(&store, &e, &mut flood, commits(false));
        assert!(got.iter().all(Option::is_none), "{got:?}");
        assert_eq!(
            flood
                .map
                .count(Origin::Publisher(scrape.get(), "uploader"), sig, 10_000),
            0
        );
        assert!(flood.map.is_empty());
        // A live feed among the finders: the item is read and counts.
        let mut flood = flooding();
        let got = sp3_of(&store, &e, &mut flood, commits(true));
        assert!(got[..19].iter().all(Option::is_none));
        assert!(
            got[19..].iter().all(|a| *a == Some(Action::Flag)),
            "{got:?}"
        );
        assert_eq!(
            flood
                .map
                .count(Origin::Publisher(scrape.get(), "uploader"), sig, 10_000),
            25
        );
    }

    /// Only finds of torrents not stored yet count: a key already stored
    /// (a refetch, a cache's file offered again) is the same name, not a
    /// new one.
    #[test]
    fn sp3_counts_a_stored_key_once() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let mut flood = Flooding {
            claims_from: bit(feed),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        let have = Have::new(1024);
        let r = results_named(&episode(0));
        have.insert(&r.key);
        for _ in 0..30 {
            let c = committing(results_named(&episode(0)), feed);
            assert_eq!(flood.count(&store, &have, &c, &|| false, 10_000), None);
        }
        assert!(flood.map.is_empty());
    }

    /// Which sources count (runner, #495): a live feed with or without a
    /// publisher; any other source but the DHT and enrichment only with
    /// one; the DHT and enrichment never.
    #[test]
    fn sp3_counts_only_live_feeds_per_source() {
        use lodestar_sources::Kind;
        let m = |id: &'static str, kinds: &'static [Kind]| lodestar_sources::Manifest {
            id,
            name: "m",
            version: "1",
            kinds,
            networks: &[],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::None,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
            always_adult: false,
            finds_private: false,
        };
        let id = |n| SourceId::new(n).unwrap();
        let nyaa = m("nyaa", &[Kind::Discovery]);
        let sukebei = m("sukebei", &[Kind::Discovery]);
        let scrape = m("full_scrape", &[Kind::Discovery]);
        let clients = m("download_clients", &[Kind::Discovery]);
        let import = m("bitmagnet", &[Kind::Importer]);
        let enrich = m("tpdb", &[Kind::Enrichment]);
        let f = Flooding::for_sources([
            (id(0), &crate::dht::MANIFEST),
            (id(1), &nyaa),
            (id(2), &sukebei),
            (id(3), &scrape),
            (id(4), &clients),
            (id(5), &import),
            (id(7), &enrich),
        ]);
        assert_eq!(f.per_source, (1 << 1) | (1 << 2), "the live feeds");
        assert_eq!(
            f.claims_from,
            (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5),
            "with a publisher: all but the DHT and enrichment"
        );
        // A claim counts against its publisher, or (a live feed's) its
        // source; enrichment's and the DHT's are never noted.
        let mut f = f;
        let claim = |source, publisher: Option<&str>, n: u8| Discovery {
            source,
            found: lodestar_sources::Found::key(InfoHash([n; 20]), 0).with_claims(
                lodestar_sources::claims::Claims {
                    name: Some("x".into()),
                    publisher: publisher.map(str::to_owned),
                    ..Default::default()
                },
            ),
            at: 0,
            reply: false,
        };
        let have = Have::new(1024);
        f.note_claims(
            &[
                claim(id(7), Some("studio"), 1),
                claim(id(0), Some("x"), 2),
                claim(id(3), None, 3),
                claim(id(3), Some("up"), 4),
                claim(id(1), None, 5),
            ],
            &have,
        );
        assert_eq!(f.notes.by_key.len(), 2, "{:?}", f.notes.by_key);
        assert!(f.notes.by_key.contains_key(&[4; 20]));
        assert_eq!(f.notes.by_key.get(&[5; 20]), Some(&Who::Source(id(1))));
        // A key already stored is never noted.
        have.insert(&InfoHash([6; 20]));
        f.note_claims(&[claim(id(1), None, 6)], &have);
        assert!(!f.notes.by_key.contains_key(&[6; 20]));
    }

    /// A full scrape's backlog of one series, with no publisher named, is
    /// never flagged or demoted: only live feeds count per source.
    #[test]
    fn sp3_never_holds_a_full_scrape_backlog() {
        use lodestar_sources::Kind;
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let scrape = store.register_source("full_scrape", "1", 0).unwrap();
        let manifest = lodestar_sources::Manifest {
            id: "full_scrape",
            name: "Full scrape",
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::None,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
            always_adult: false,
            finds_private: false,
        };
        let mut flood = Flooding::for_sources([(scrape, &manifest)]);
        flood.clock = Some(10_000);
        let e = sp3_engine();
        let cs = (0..200)
            .map(|i| committing(results_named(&episode(i)), scrape))
            .collect();
        assert!(
            sp3_of(&store, &e, &mut flood, cs)
                .iter()
                .all(Option::is_none)
        );
        assert!(flood.map.is_empty());
    }

    /// The same publisher name on two sources is two publishers.
    #[test]
    fn sp3_keys_a_publisher_by_its_source() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let a = store.register_source("a", "1", 0).unwrap();
        let b = store.register_source("b", "1", 0).unwrap();
        let e = sp3_engine();
        let mut flood = Flooding {
            claims_from: bit(a) | bit(b),
            clock: Some(10_000),
            ..Default::default()
        };
        let named = |source, n: usize| -> Vec<Committing> {
            (0..n)
                .map(|i| {
                    let mut c = committing(results_named(&episode(i)), source);
                    c.leased = false;
                    c.publisher = Some("P".into());
                    c
                })
                .collect()
        };
        assert!(
            sp3_of(&store, &e, &mut flood, named(a, 19))
                .iter()
                .all(Option::is_none)
        );
        assert!(
            sp3_of(&store, &e, &mut flood, named(b, 19))
                .iter()
                .all(Option::is_none)
        );
        assert_eq!(
            sp3_of(&store, &e, &mut flood, named(a, 1)),
            [Some(Action::Flag)]
        );
    }

    /// With SP-3 off, a flood is still counted, so switching it on finds
    /// the rate as it is, but nothing is judged again.
    #[test]
    fn sp3_off_counts_without_judging() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        // Each name is also a site advert, which SP-1 demotes whether or
        // not SP-3 is on. The prepared verdict, trusted as is, is empty,
        // so SP-1's decision shows only when the commit is judged again.
        let mut e = sp3_engine();
        let name = |i| format!("www.example-site.com - {}", episode(i));
        let judged_again =
            |e: &Engine, flood: &mut Flooding| -> usize {
                let have = Have::new(1024);
                (0..30)
                    .map(|i| {
                        let mut r = results_named(&name(i));
                        r.worker = WorkerId::LOCAL;
                        r.version = e.version;
                        r.verdict = Verdict::default();
                        let c = committing(r, feed);
                        let mut out = judge(
                            &store,
                            e,
                            AdultSources::default(),
                            None,
                            flood,
                            &have,
                            vec![c],
                        );
                        let c = out.pop().unwrap();
                        usize::from(c.verdict.is_some_and(|d| {
                            d.rule == RuleRef::Pack(lodestar_filter::PackRule::Sp1)
                        }))
                    })
                    .sum()
            };
        let flooding = || Flooding {
            claims_from: bit(feed),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        // On: the 20th to 30th are judged again (SP-1's demote outweighs
        // SP-3's flag).
        assert_eq!(judged_again(&e, &mut flooding()), 11);
        // Off: counted all the same, never judged again.
        e.packs.rules_off.insert(lodestar_filter::PackRule::Sp3);
        let mut off = flooding();
        assert_eq!(judged_again(&e, &mut off), 0);
        let title = results_named(&name(0)).class.title.release.title;
        let sig = lodestar_filter::flood::signature(&title).unwrap();
        assert_eq!(
            off.map.count(
                lodestar_filter::flood::Origin::Source(feed.get()),
                sig,
                10_000
            ),
            30
        );
    }

    /// A CSX-5 entry in shadow mode is logged once after the second
    /// judgment, with the action of the decision that stands.
    #[test]
    fn sp3_logs_a_shadow_override_once() {
        use lodestar_filter::packs::manifest::Manifest;
        use lodestar_filter::packs::overrides::PackOverride;
        use lodestar_filter::packs::upgrade;
        use lodestar_filter::{Match, Override, Strength};
        // SP-1 overridden to flag: the first verdict is SP-1's flag, the
        // flooded one SP-3's demote, so the two shadow entries differ.
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0).unwrap();
        let row = PackOverride {
            base: 1,
            action: Action::Flag,
            unless: None,
        }
        .encode();
        s.set_pack_override("SP-1", Some(&row)).unwrap();
        let mut e = engine();
        assert!(e.load_rules(&s.rule_set().unwrap()).is_empty());
        e.packs = lodestar_filter::PackSwitches::all();
        e.overrides.push(Override {
            id: 3,
            what: Match::Keyword("example".into()),
            strength: Strength::Override,
            scope: None,
            expires_at: None,
            shadow_until: Some(i64::MAX),
        });
        let feed = s.register_source("feed", "1", 0).unwrap();
        let mut flood = Flooding {
            claims_from: bit(feed),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        let have = Have::new(1024);
        let mut last = None;
        for i in 0..DEMOTE_N {
            let name = format!("www.example-site.com - {}", episode(i));
            let c = committing(results_named(&name), feed);
            let mut out = judge(
                &s,
                &e,
                AdultSources::default(),
                None,
                &mut flood,
                &have,
                vec![c],
            );
            last = out.pop();
        }
        let c = last.unwrap();
        let d = c.verdict.expect("decided");
        assert_eq!(
            (d.rule, d.action),
            (
                RuleRef::Pack(lodestar_filter::PackRule::Sp3),
                Action::Demote
            )
        );
        let overrides: Vec<_> = c
            .shadow
            .iter()
            .filter(|x| x.rule == RuleRef::Override(3))
            .collect();
        assert_eq!(overrides.len(), 1, "{:?}", c.shadow);
        assert_eq!(overrides[0].action, Action::Demote);
    }

    const DEMOTE_N: usize = lodestar_filter::flood::DEMOTE_AT as usize;

    /// Claims the writer stored this start name their publisher without
    /// a store read; a find of another key isn't counted against them.
    #[test]
    fn sp3_notes_the_claims_the_writer_stores() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let e = sp3_engine();
        let mut flood = Flooding {
            claims_from: bit(feed),
            per_source: bit(feed),
            clock: Some(10_000),
            ..Default::default()
        };
        let rs: Vec<Results> = (0..20).map(|i| results_named(&episode(i))).collect();
        let found = |source, publisher: Option<&str>| -> Vec<Discovery> {
            rs.iter()
                .map(|r| Discovery {
                    source,
                    found: lodestar_sources::Found::key(r.key, 0).with_claims(
                        lodestar_sources::claims::Claims {
                            publisher: publisher.map(str::to_owned),
                            ..Default::default()
                        },
                    ),
                    at: 0,
                    reply: false,
                })
                .collect()
        };
        // The DHT's are never noted.
        flood.note_claims(&found(dht, Some("x")), &Have::new(1024));
        assert!(flood.notes.by_key.is_empty());
        flood.note_claims(&found(feed, Some("uploader")), &Have::new(1024));
        // A store with an unverified item (here another key's), so the
        // commit looks; the note answers without reading.
        use lodestar_store::unverified::NewUnverified;
        let claims = lodestar_store::claims::Claims {
            name: Some("x".into()),
            ..Default::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &[9; 20],
                source: feed,
                private_source: false,
                claims: Some(&claims),
                at: 0,
            }])
            .unwrap();
        let cs = rs.into_iter().map(|r| committing(r, feed)).collect();
        let got = sp3_of(&store, &e, &mut flood, cs);
        assert_eq!(got[19], Some(Action::Flag));
        let sig = lodestar_filter::flood::signature("Example Show").unwrap();
        use lodestar_filter::flood::Origin;
        assert_eq!(
            flood
                .map
                .count(Origin::Publisher(feed.get(), "uploader"), sig, 10_000),
            20
        );
        assert_eq!(flood.map.count(Origin::Source(feed.get()), sig, 10_000), 0);
        // Bounded.
        for i in 0..ClaimNotes::CAP as u32 + 10 {
            let mut k = [0; 20];
            k[..4].copy_from_slice(&i.to_le_bytes());
            flood.notes.insert(k, Who::Source(feed));
        }
        assert_eq!(flood.notes.by_key.len(), ClaimNotes::CAP);
        assert_eq!(flood.notes.order.len(), ClaimNotes::CAP);
    }

    // -----------------------------------------------------------------
    // SP-4 (RP-5): clones of hidden spam, judged and added at commit.

    /// A keyword-stuffed name SP-2 hides.
    fn stuffed() -> String {
        "free movie download ".repeat(20).trim_end().to_owned()
    }

    /// A writer whose engine has every pack on and every rule applied.
    fn sp4_writer() -> (Writer, Arc<Coordinator>) {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel));
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut w = writer(store, coordinator.clone(), Arc::new(GateStats::new(0)));
        let mut e = sp3_engine();
        e.packs.rules_off.remove(&lodestar_filter::PackRule::Sp4);
        w.filter = Arc::new(Handle::new(e));
        (w, coordinator)
    }

    /// `name`'s stored flags and the rules its filter hits name.
    fn stored_as(w: &Writer, name: &str) -> (i64, Vec<String>) {
        let s = w.store.as_ref().unwrap();
        let key = results_named(name).key;
        let id = s.find(&key).unwrap().expect("stored");
        let hits = s
            .filter_hits(0)
            .unwrap()
            .into_iter()
            .filter(|h| h.hash == key.0)
            .map(|h| h.rule)
            .collect();
        (s.torrent(id).unwrap().unwrap().flags, hits)
    }

    fn sig_rows(w: &Writer) -> Vec<lodestar_store::RawSetting> {
        w.store
            .as_ref()
            .unwrap()
            .settings_with_prefix_raw(lodestar_filter::spam_sigs::PREFIX)
            .unwrap()
    }

    /// An original SP-2 hides and its clone (another name, the same
    /// files) in one batch: the clone is hidden by SP-4, and the batch
    /// writes one row of the set. With SP-4 off a clone passes, and the
    /// original's signature is still added.
    #[tokio::test]
    async fn sp4_hides_a_clone_later_in_the_same_batch() {
        let (mut w, coordinator) = sp4_writer();
        let (retry, _rx) = mpsc::channel(4);
        let batch = vec![
            leased(&coordinator, results_named(&stuffed())),
            leased(&coordinator, results_named("Holiday Video 2024")),
        ];
        w.commit(batch, vec![], vec![], vec![], &retry).await;
        let (flags, hits) = stored_as(&w, &stuffed());
        assert!(flags & flags::HIDDEN != 0 && hits == ["SP-2"], "{hits:?}");
        let (flags, hits) = stored_as(&w, "Holiday Video 2024");
        assert!(flags & flags::HIDDEN != 0 && hits == ["SP-4"], "{hits:?}");
        assert_eq!(w.filter.current().spam_sigs.len(), 1, "the clone adds none");
        assert_eq!(sig_rows(&w).len(), 1, "one row");
        // A later clone, with SP-4 off: it passes.
        w.filter
            .update_engine(|e| e.packs.rules_off.insert(lodestar_filter::PackRule::Sp4));
        let later = leased(&coordinator, results_named("Garden Video 2024"));
        w.commit(vec![later], vec![], vec![], vec![], &retry).await;
        let (flags, hits) = stored_as(&w, "Garden Video 2024");
        assert!(flags & flags::HIDDEN == 0 && hits.is_empty(), "{hits:?}");
        // An original hidden while SP-4 is off is still added, and the
        // write is one row again.
        let before = sig_rows(&w);
        let other = format!("{} again", stuffed());
        let mut r = results_named(&other);
        r.files[0].size += 1;
        r.torrent.size += 1;
        w.commit(
            vec![leased(&coordinator, r)],
            vec![],
            vec![],
            vec![],
            &retry,
        )
        .await;
        assert_eq!(w.filter.current().spam_sigs.len(), 2);
        let after = sig_rows(&w);
        assert_eq!(after.len(), 1);
        assert_ne!(after, before);
    }

    /// Only stored torrents' hides feed the set: claims (unverified items,
    /// the torrent cache's lookups and enrichment's matches among them)
    /// are judged apart from the commit and never add a signature.
    #[tokio::test]
    async fn sp4_is_never_fed_by_claims() {
        let (mut w, _) = sp4_writer();
        let budget = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = budget.try_acquire_owned().unwrap();
        let found = lodestar_sources::Found::key(InfoHash([0x42; 20]), 0).with_claims(
            lodestar_sources::Claims {
                name: Some(stuffed()),
                size: Some(900 << 20),
                ..Default::default()
            },
        );
        let d = Discovery {
            source: SourceId::new(1).unwrap(),
            found,
            at: 100,
            reply: false,
        };
        w.ingest_claims(vec![(d, permit)]).await;
        assert!(w.filter.current().spam_sigs.is_empty());
        assert!(sig_rows(&w).is_empty());
    }

    /// SP-4's added cost per commit in the writer (RP-5, the same 5% gate
    /// as SP-3's): a crawl commit while the set is empty, the lookup with
    /// the set at its cap, and a hide's add and one-row write. Run:
    /// `cargo test --release -p lodestar-pipeline --lib sp4_writer_cost --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn sp4_writer_cost() {
        use lodestar_filter::spam_sigs::{CAP, Original, usable};
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        const N: usize = 10_240;
        // Torrents of three files, sizes of their own.
        let rs: Vec<Results> = (0..N)
            .map(|i| {
                let mut r = results_named(&format!("Crawled Title {i} 1080p"));
                r.files[0].size += i as u64;
                for k in 0..2u64 {
                    let mut f = r.files[0].clone();
                    f.path = vec![format!("extra{k}.nfo")];
                    f.size = 4_001 + k + i as u64;
                    r.files.push(f);
                }
                r
            })
            .collect();
        let t = Instant::now();
        for batch in rs.chunks(256) {
            let pts: Vec<PreparedTorrent<'_>> = batch
                .iter()
                .map(|r| PreparedTorrent {
                    torrent: &r.torrent,
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: 0,
                    source: dht,
                    verified: true,
                })
                .collect();
            store
                .write(&WriteBatch {
                    prepared: &pts,
                    ..WriteBatch::default()
                })
                .unwrap();
        }
        let insert = t.elapsed().as_nanos() as f64 / N as f64;
        let e = sp3_engine();
        let sigs = &e.spam_sigs;
        // The writer's step, as `judge` runs it.
        let step = |r: &Results| {
            if !sigs.is_empty() {
                std::hint::black_box(
                    usable(lodestar_filter::payload_sig(&r.files)).and_then(|s| sigs.get(s)),
                );
            }
        };
        let time = |f: &dyn Fn(&Results)| {
            let t = Instant::now();
            for r in &rs {
                f(r);
            }
            t.elapsed().as_nanos() as f64 / N as f64
        };
        let empty_ns = time(&step);
        let hid = Original {
            rule: lodestar_filter::PackRule::Sp2,
            action: Action::Hide,
        };
        for i in 0..CAP as u64 {
            sigs.add(
                lodestar_filter::payload_sig(&[lodestar_classify::FileInfo::new(
                    vec!["x".into()],
                    (2 << 40) + i,
                )])
                .unwrap()
                .sig,
                hid,
            );
        }
        sigs.flush(&mut store).unwrap();
        let full_ns = time(&step);
        // Hides: their adds, and their batch's one-row write, 8 hides a
        // batch.
        let t = Instant::now();
        let mut flushes = 0;
        for (i, r) in rs.iter().enumerate().take(2_048) {
            if let Some(s) = usable(lodestar_filter::payload_sig(&r.files)) {
                sigs.add(s, hid);
            }
            if i % 8 == 7 {
                sigs.flush(&mut store).unwrap();
                flushes += 1;
            }
        }
        let batch_ns = t.elapsed().as_nanos() as f64 / f64::from(flushes);
        // A flood of one payload (each hide adds it again) after a release
        // left a removal in the open row: compaction stays within a row.
        let old = usable(lodestar_filter::payload_sig(&rs[0].files)).unwrap();
        sigs.release(&mut store, &[old], None).unwrap();
        let flood = usable(lodestar_filter::payload_sig(&rs[1].files)).unwrap();
        let t = Instant::now();
        for i in 0..2_048 {
            sigs.add(flood, hid);
            if i % 8 == 7 {
                sigs.flush(&mut store).unwrap();
            }
        }
        let flood_ns = t.elapsed().as_nanos() as f64 / 256.0;
        println!(
            "per insert: {:.2} µs; SP-4: a crawl commit with the set empty {:.3} µs ({:.2}%), \
             with the set at its cap {:.2} µs ({:.1}%); a batch with hides: {:.1} µs for their \
             adds and its one row, {:.2} µs a commit of a 256-commit batch ({:.1}%); a flood \
             of one payload after a release, {:.1} µs a batch",
            insert / 1000.0,
            empty_ns / 1000.0,
            100.0 * empty_ns / insert,
            full_ns / 1000.0,
            100.0 * full_ns / insert,
            batch_ns / 1000.0,
            batch_ns / 256.0 / 1000.0,
            100.0 * batch_ns / 256.0 / insert,
            flood_ns / 1000.0,
        );
        assert!(
            flood_ns < 4.0 * batch_ns,
            "a flood after a release: within a row"
        );
        assert!(empty_ns <= 0.05 * insert, "a crawl commit, the set empty");
    }

    /// SP-3's added cost per insert in the writer, measured apart from
    /// the engine's budget (RP-4 gate: at most 5% of the writer's
    /// per-insert cost). Run: `cargo test --release -p lodestar-pipeline
    /// --lib sp3_writer_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn sp3_writer_cost() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let feed = store.register_source("feed", "1", 0).unwrap();
        let scrape = store.register_source("full_scrape", "1", 0).unwrap();
        let e = sp3_engine();
        const N: usize = 10_240;
        let mut cs: Vec<Committing> = (0..N)
            .map(|i| {
                // A dozen titles in turn, fewer than a source's signature
                // slots: their counts reach the levels.
                let name = format!("Crawled Title {} 1080p", i % 12);
                committing(results_named(&name), dht)
            })
            .collect();
        // The insert SP-3 is added to: batches of 256.
        let t = Instant::now();
        for batch in cs.chunks(256) {
            let pts: Vec<PreparedTorrent<'_>> = batch
                .iter()
                .map(|c| PreparedTorrent {
                    torrent: &c.results.torrent,
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: 0,
                    source: dht,
                    verified: true,
                })
                .collect();
            store
                .write(&WriteBatch {
                    prepared: &pts,
                    ..WriteBatch::default()
                })
                .unwrap();
        }
        let insert = t.elapsed().as_nanos() as f64 / N as f64;
        // A feed's find promotes the unverified item its claims made: the
        // item is read and its claims judged again (`promote`), on top of
        // the insert.
        let claims: Vec<lodestar_store::claims::Claims> = cs
            .iter()
            .map(|c| lodestar_store::claims::Claims {
                name: Some(c.results.torrent.name.clone()),
                size: Some(900 << 20),
                ..Default::default()
            })
            .collect();
        // Under keys of their own: the store refuses an item for a key
        // it holds a torrent under.
        let other = |k: [u8; 20]| {
            let mut k = k;
            k[0] ^= 0xff;
            k
        };
        let keys: Vec<[u8; 20]> = cs.iter().map(|c| other(c.results.key.0)).collect();
        let items: Vec<lodestar_store::unverified::NewUnverified<'_>> = keys
            .iter()
            .zip(&claims)
            .map(|(key, claims)| lodestar_store::unverified::NewUnverified {
                network: Network::Bt,
                key,
                source: feed,
                private_source: false,
                claims: Some(claims),
                at: 0,
            })
            .collect();
        store.put_unverified(&items).unwrap();
        let t = Instant::now();
        for key in &keys {
            let item = store.unverified(Network::Bt, key).unwrap().unwrap();
            std::hint::black_box(unverified::judge(
                &e,
                *key,
                item.claims.as_ref().unwrap(),
                None,
                false,
                10_000,
            ));
        }
        let promotion = t.elapsed().as_nanos() as f64 / N as f64;
        let have = Have::new(1024);
        // `noted`: the feed's claims came through this start's writer.
        let mut time = |source: SourceId, publisher: Option<&str>, noted: bool| {
            let mut flood = Flooding {
                claims_from: bit(feed) | bit(scrape),
                per_source: bit(feed),
                clock: Some(10_000),
                ..Default::default()
            };
            if noted {
                let found: Vec<Discovery> = cs
                    .iter()
                    .map(|c| Discovery {
                        source: feed,
                        found: lodestar_sources::Found::key(c.results.key, 0).with_claims(
                            lodestar_sources::claims::Claims {
                                name: Some("claimed".into()),
                                ..Default::default()
                            },
                        ),
                        at: 0,
                        reply: false,
                    })
                    .collect();
                flood.note_claims(&found, &Have::new(1024));
            }
            for c in &mut cs {
                c.lease.source = source;
                c.lease.sources = bit(source);
                c.publisher = publisher.map(str::to_owned);
                // A publisher on the commit itself is a cache's file's.
                c.leased = publisher.is_none();
            }
            let t = Instant::now();
            let mut levels = 0;
            for c in &cs {
                levels += usize::from(flood.count(&store, &have, c, &|| true, 10_000).is_some());
            }
            (t.elapsed().as_nanos() as f64 / N as f64, levels)
        };
        let (dht_ns, dht_levels) = time(dht, None, false);
        let (feed_ns, feed_levels) = time(feed, None, false);
        let (noted_ns, noted_levels) = time(feed, None, true);
        let (pub_ns, _) = time(feed, Some("uploader"), false);
        // A non-live source's claimed find (a full scrape's), with no note:
        // no store read (runner, #495).
        let (scrape_ns, scrape_levels) = time(scrape, None, false);
        assert_eq!(scrape_levels, 0);
        assert_eq!(noted_levels, feed_levels);
        assert_eq!(dht_levels, 0);
        assert!(feed_levels > 0);
        // A flooded commit's second judgment, as the writer makes it.
        let flooded: Vec<&Results> = cs.iter().take(2_000).map(|c| &c.results).collect();
        let t = Instant::now();
        for r in &flooded {
            std::hint::black_box(judge_at_commit(
                &e,
                r,
                0,
                AtCommit {
                    flood: Some(lodestar_filter::flood::Level::Flag),
                    clone_of: None,
                },
                10_000,
            ));
        }
        let rejudge_ns = t.elapsed().as_nanos() as f64 / flooded.len() as f64;
        let promoted = insert + promotion;
        println!(
            "per insert: {:.2} µs (a feed's promoted find: {:.2} µs); SP-3: DHT find {:.3} µs \
             ({:.2}% of an insert), feed find {:.2} µs ({:.1}% of its commit; {:.2} µs, {:.1}%, \
             when its claims came this start), named publisher {:.2} µs ({:.1}%), a full \
             scrape's claimed find {:.3} µs ({:.2}% of an insert); a flooded commit's second \
             judgment {:.2} µs; \
             {feed_levels} of {N} feed finds reached a level",
            insert / 1000.0,
            promoted / 1000.0,
            dht_ns / 1000.0,
            100.0 * dht_ns / insert,
            feed_ns / 1000.0,
            100.0 * feed_ns / promoted,
            noted_ns / 1000.0,
            100.0 * noted_ns / promoted,
            pub_ns / 1000.0,
            100.0 * pub_ns / promoted,
            scrape_ns / 1000.0,
            100.0 * scrape_ns / insert,
            rejudge_ns / 1000.0,
        );
        assert!(dht_ns <= 0.05 * insert, "the crawl's commits");
        assert!(noted_ns <= 0.05 * promoted, "a feed's commits");
        assert!(scrape_ns <= 0.05 * insert, "a full scrape's commits");
    }
}
