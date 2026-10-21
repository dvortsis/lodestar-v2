//! The single writer (architecture §7), the coordinator's commit step
//! (§19.4): the one task that changes the store. It commits prepared
//! torrents under current leases, applies rediscoveries, and flushes the
//! Insights counters with them, or on their own every few seconds when
//! nothing else is committing.
//!
//! SQLite calls block, so each transaction runs on the blocking pool with
//! the store moved in and back out; the async side never waits on anything
//! that waits on it.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_classify::ENRICHMENT_VERSION;
use lodestar_cluster::Lease;
use lodestar_metainfo::InfoHash;
use lodestar_sources::{Discovery, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{
    CohortCount, Counter, Event, Inserted, PreparedTorrent, QueuedKey, SourceId, Store, Touch,
    WriteBatch, flags,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, interval_at};

use crate::coordinator::Coordinator;
use crate::enrich::enrich;
use crate::have::{Bloom, Have, capacity_for};
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
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct WriterStats {
    pub stored: AtomicU64,
    /// Fetched, but already stored (a race with the have-filter).
    pub existing: AtomicU64,
    pub invalid: AtomicU64,
    pub blocked: AtomicU64,
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
}

pub(crate) struct Writer {
    pub store: Option<Store>,
    /// The background enrichment pass: the last id it reached, and whether
    /// it has found nothing left to do.
    pub backfill_after: i64,
    pub backfill_done: bool,
    pub cfg: WriterConfig,
    pub have: Arc<Have>,
    pub funnel: Arc<FunnelCounts>,
    pub rollups: Arc<Rollups>,
    pub blocklist: Arc<HashSet<[u8; 20]>>,
    pub coordinator: Arc<Coordinator>,
    pub stats: Arc<WriterStats>,
}

/// A result under a current lease, ready to commit.
struct Committing {
    results: Results,
    lease: Lease,
}

/// What a transaction did, for the async side to account.
struct Outcome {
    /// Per stored or already-stored result: (index, new?, swarm hashes).
    stored: Vec<(usize, bool, Vec<InfoHash>)>,
    /// Results the authoritative blocklist rejected.
    blocked: Vec<usize>,
    /// Per touch: whether the store had it.
    touched: Vec<bool>,
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
                    self.commit(batch, Vec::new(), Vec::new(), &io.retry).await;
                }
                Some(first) = io.touches.recv() => {
                    let mut touches = vec![first];
                    while touches.len() < 4 * self.cfg.batch_max {
                        match io.touches.try_recv() {
                            Ok(d) => touches.push(d),
                            Err(_) => break,
                        }
                    }
                    self.commit(Vec::new(), touches, Vec::new(), &io.retry).await;
                }
                Some(queue) = io.queues.recv() => self.save_queue(queue).await,
                // Always ready until done; the unbiased select gives it a
                // fair share between ingest batches.
                () = std::future::ready(()), if !self.backfill_done => self.backfill().await,
                _ = flush.tick() => {
                    self.commit(Vec::new(), Vec::new(), Vec::new(), &io.retry).await;
                }
            }
        }
        let queue = io.finish.await.unwrap_or_default();
        let stop = Event {
            at: unix_now(),
            kind: event::STOP,
            detail: format!("{} keys queued", queue.len()),
        };
        self.commit(Vec::new(), Vec::new(), vec![stop], &io.retry)
            .await;
        self.save_queue(queue).await;
        let store = self.store.take().expect("store");
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.checkpoint() {
                tracing::warn!("WAL checkpoint at shutdown failed: {e}");
            }
            store
        })
        .await
        .expect("checkpoint task")
    }

    async fn commit(
        &mut self,
        results: Vec<Results>,
        touches: Vec<Discovery>,
        events: Vec<Event>,
        retry: &mpsc::Sender<Discovery>,
    ) {
        // Fencing (§19.3): only results under a current lease count. A
        // stale one comes from a holder whose lease expired; the key's new
        // holder will deliver it.
        let committing: Vec<Committing> = results
            .into_iter()
            .filter_map(|r| {
                let lease = self
                    .coordinator
                    .current(&r.key, r.token)
                    .filter(|l| l.holder == r.worker)?;
                Some(Committing { results: r, lease })
            })
            .collect();
        let cohorts = self.funnel.drain();
        let counters = self.rollups.drain();
        if committing.is_empty()
            && touches.is_empty()
            && events.is_empty()
            && cohorts.is_empty()
            && counters.is_empty()
        {
            return;
        }
        let mut store = self.store.take().expect("store");
        let blocklist = self.blocklist.clone();
        let backfill_until = self.cfg.backfill_until;
        let (store, committing, touches, result) = tokio::task::spawn_blocking(move || {
            let result = transaction(
                &mut store,
                &committing,
                &touches,
                &cohorts,
                &counters,
                &events,
                &blocklist,
                backfill_until,
            );
            (store, committing, touches, result)
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
                tracing::error!("store write failed: {e}");
                return;
            }
        };
        self.stats.transactions.fetch_add(1, Ordering::Relaxed);
        self.stats
            .last_commit
            .store(unix_now() as u64, Ordering::Relaxed);
        let now = unix_now();
        // The have-filter learns the keys before their leases end, so a
        // rediscovery in between is held, never claimed and fetched again.
        for (_, _, hashes) in &outcome.stored {
            for h in hashes {
                self.have.insert(h);
            }
        }
        for (i, new, _) in outcome.stored {
            let c = &committing[i];
            let Some(lease) = self.coordinator.complete(&c.results.key, c.results.token) else {
                continue;
            };
            self.funnel
                .record(lease.cohort, lease.source, Stage::Stored, None);
            if new {
                self.stats.stored.fetch_add(1, Ordering::Relaxed);
                self.rollups.add(now, metric::STORED, lease.source, 0, 1);
            } else {
                self.stats.existing.fetch_add(1, Ordering::Relaxed);
            }
        }
        for i in outcome.blocked {
            let c = &committing[i];
            self.stats.blocked.fetch_add(1, Ordering::Relaxed);
            self.coordinator
                .lost(&c.results.key, c.results.token, Loss::Blocked);
        }
        for (d, had) in touches.into_iter().zip(outcome.touched) {
            if had {
                self.stats.rediscovered.fetch_add(1, Ordering::Relaxed);
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
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let result = backfill_batch(&mut store, after);
            (store, result)
        })
        .await
        .expect("backfill task");
        self.store = Some(store);
        match result {
            Ok(Some((last, n))) => {
                self.backfill_after = last;
                self.stats.enriched.fetch_add(n as u64, Ordering::Relaxed);
            }
            Ok(None) => self.backfill_done = true,
            Err(e) => {
                // Tried again at the next start.
                self.backfill_done = true;
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!("background enrichment failed: {e}");
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
            Err(e) => tracing::error!("rebuilding the have-filter failed: {e}"),
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
            tracing::error!("saving the fetch queue failed: {e}");
        }
    }
}

/// Torrents enriched per background step.
const BACKFILL_BATCH: usize = 256;

/// Enriches the stored torrents after `after` that need it. `None` when
/// there are none left.
fn backfill_batch(
    store: &mut Store,
    after: i64,
) -> Result<Option<(i64, usize)>, lodestar_store::Error> {
    let ids = store.unenriched(after, ENRICHMENT_VERSION, BACKFILL_BATCH)?;
    let Some(&last) = ids.last() else {
        return Ok(None);
    };
    let mut batch = Vec::with_capacity(ids.len());
    for id in ids {
        if let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) {
            batch.push((id, enrich(&row.name, &tree.files, tree.encoding)));
        }
    }
    let n = store.set_enrichments(&batch)?;
    Ok(Some((last, n)))
}

/// One store transaction (runs on the blocking pool).
#[allow(clippy::too_many_arguments)]
fn transaction(
    store: &mut Store,
    committing: &[Committing],
    touches: &[Discovery],
    cohorts: &[CohortCount],
    counters: &[Counter],
    events: &[Event],
    blocklist: &HashSet<[u8; 20]>,
    backfill_until: i64,
) -> Result<Outcome, lodestar_store::Error> {
    // The authoritative blocklist check, on the key and every swarm hash,
    // before anything is stored (§18.7, §19.4).
    let (accepted, blocked): (Vec<usize>, Vec<usize>) = (0..committing.len()).partition(|&i| {
        let r = &committing[i].results;
        !blocklist.contains(&r.key.0) && !r.torrent.swarm_hashes().any(|h| blocklist.contains(&h.0))
    });
    let prepared: Vec<PreparedTorrent<'_>> = accepted
        .iter()
        .map(|&i| {
            let Committing { results, lease } = &committing[i];
            PreparedTorrent {
                torrent: &results.torrent,
                discovered_at: lease.discovered_at,
                metadata_at: results.metadata_at,
                extra_flags: if lease.discovered_at < backfill_until {
                    flags::BACKFILL
                } else {
                    0
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
                c.results.key,
                c.lease.sources | c.results.sources,
                c.lease.source,
            )
        })
        .filter(|&(_, sources, first)| sources != bit(first))
        .map(|(hash, sources, _)| Touch { hash, sources })
        .collect();
    let extra = all_touches.len();
    all_touches.extend(touches.iter().map(|d| Touch {
        hash: d.found.key,
        sources: bit(d.source),
    }));
    let written = store.write(&WriteBatch {
        prepared: &prepared,
        touches: &all_touches,
        counters,
        cohorts,
        events,
        ..WriteBatch::default()
    })?;
    let stored = accepted
        .iter()
        .zip(&written.prepared)
        .map(|(&i, ins)| {
            (
                i,
                matches!(ins, Inserted::New(_)),
                committing[i].results.torrent.swarm_hashes().collect(),
            )
        })
        .collect();
    Ok(Outcome {
        stored,
        blocked,
        touched: written.touched[extra..]
            .iter()
            .map(Option::is_some)
            .collect(),
    })
}

fn bit(source: SourceId) -> u64 {
    1u64 << source.get()
}
