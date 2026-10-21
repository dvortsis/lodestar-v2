//! The single writer (architecture §7): the one task that changes the
//! store. It batches verified info dictionaries into transactions, applies
//! rediscoveries, and flushes the Insights counters with them, or on their
//! own every few seconds when nothing else is committing.
//!
//! SQLite calls block, so each transaction runs on the blocking pool with
//! the store moved in and back out; the async side never waits on anything
//! that waits on it.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_metainfo::{Info, InfoHash};
use lodestar_sources::{Discovery, FetchedKey, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{
    CohortCount, Counter, Event, Inserted, NewTorrent, QueuedKey, SourceId, Store, Touch,
    WriteBatch, flags,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, interval_at, timeout_at};

use crate::have::{Bloom, Have, capacity_for};
use crate::rollups::{Rollups, metric};
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
}

pub(crate) struct WriterIo {
    pub fetched: mpsc::Receiver<FetchedKey>,
    pub touches: mpsc::Receiver<Discovery>,
    /// Periodic snapshots of the fetch queue, to persist.
    pub queues: mpsc::Receiver<Vec<QueuedKey>>,
    /// Have-filter false positives go to the fetch pool.
    pub to_pool: mpsc::Sender<Discovery>,
    /// The pool's queue at shutdown.
    pub finish: oneshot::Receiver<Vec<QueuedKey>>,
}

pub(crate) struct Writer {
    pub store: Option<Store>,
    pub cfg: WriterConfig,
    pub have: Arc<Have>,
    pub funnel: Arc<FunnelCounts>,
    pub rollups: Arc<Rollups>,
    pub blocklist: Arc<HashSet<[u8; 20]>>,
    pub stats: Arc<WriterStats>,
}

/// What a transaction did, for the async side to account.
struct Outcome {
    /// Per stored or already-stored fetched key: (index, new?, swarm hashes).
    stored: Vec<(usize, bool, Vec<InfoHash>)>,
    rejected: Vec<(usize, Loss)>,
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
                first = io.fetched.recv() => {
                    let Some(first) = first else { break };
                    let batch =
                        collect(first, &mut io.fetched, self.cfg.batch_max, self.cfg.batch_wait).await;
                    self.commit(batch, Vec::new(), Vec::new(), &io.to_pool).await;
                }
                Some(first) = io.touches.recv() => {
                    let mut touches = vec![first];
                    while touches.len() < 4 * self.cfg.batch_max {
                        match io.touches.try_recv() {
                            Ok(d) => touches.push(d),
                            Err(_) => break,
                        }
                    }
                    self.commit(Vec::new(), touches, Vec::new(), &io.to_pool).await;
                }
                Some(queue) = io.queues.recv() => self.save_queue(queue).await,
                _ = flush.tick() => {
                    self.commit(Vec::new(), Vec::new(), Vec::new(), &io.to_pool).await;
                }
            }
        }
        let queue = io.finish.await.unwrap_or_default();
        let stop = Event {
            at: unix_now(),
            kind: event::STOP,
            detail: format!("{} keys queued", queue.len()),
        };
        self.commit(Vec::new(), Vec::new(), vec![stop], &io.to_pool)
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
        fetched: Vec<FetchedKey>,
        touches: Vec<Discovery>,
        events: Vec<Event>,
        to_pool: &mpsc::Sender<Discovery>,
    ) {
        let cohorts = self.funnel.drain();
        let counters = self.rollups.drain();
        if fetched.is_empty()
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
        let (store, fetched, touches, result) = tokio::task::spawn_blocking(move || {
            let result = transaction(
                &mut store,
                &fetched,
                &touches,
                &cohorts,
                &counters,
                &events,
                &blocklist,
                backfill_until,
            );
            (store, fetched, touches, result)
        })
        .await
        .expect("writer transaction task");
        self.store = Some(store);
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                // The batch and its counters are lost; keep going.
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
        for (i, new, hashes) in outcome.stored {
            let fk = &fetched[i];
            for h in &hashes {
                self.have.insert(h);
            }
            self.funnel
                .record(fk.cohort, fk.source, Stage::Stored, None);
            if new {
                self.stats.stored.fetch_add(1, Ordering::Relaxed);
                self.rollups.add(now, metric::STORED, fk.source, 0, 1);
            } else {
                self.stats.existing.fetch_add(1, Ordering::Relaxed);
            }
        }
        for (i, loss) in outcome.rejected {
            let fk = &fetched[i];
            let counter = match loss {
                Loss::Blocked => &self.stats.blocked,
                _ => &self.stats.invalid,
            };
            counter.fetch_add(1, Ordering::Relaxed);
            self.funnel
                .record(fk.cohort, fk.source, Stage::Lost, Some(loss));
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
                if to_pool.try_send(d.clone()).is_err() {
                    self.rollups.add(d.at, metric::DROPPED, d.source, 0, 1);
                }
            }
        }
        if self.have.is_full() {
            self.rebuild_have().await;
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

/// Gathers more fetched keys for the same transaction, briefly. (A free
/// function: a `&Writer` held across an await would make the task !Send,
/// since the store isn't `Sync`.)
async fn collect(
    first: FetchedKey,
    rx: &mut mpsc::Receiver<FetchedKey>,
    max: usize,
    wait: Duration,
) -> Vec<FetchedKey> {
    let mut batch = vec![first];
    let deadline = Instant::now() + wait;
    while batch.len() < max {
        match rx.try_recv() {
            Ok(fk) => batch.push(fk),
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => match timeout_at(deadline, rx.recv()).await {
                Ok(Some(fk)) => batch.push(fk),
                Ok(None) | Err(_) => break,
            },
        }
    }
    batch
}

/// One store transaction (runs on the blocking pool).
#[allow(clippy::too_many_arguments)]
fn transaction(
    store: &mut Store,
    fetched: &[FetchedKey],
    touches: &[Discovery],
    cohorts: &[CohortCount],
    counters: &[Counter],
    events: &[Event],
    blocklist: &HashSet<[u8; 20]>,
    backfill_until: i64,
) -> Result<Outcome, lodestar_store::Error> {
    let now = unix_now();
    let mut parsed: Vec<(usize, Info)> = Vec::with_capacity(fetched.len());
    let mut rejected = Vec::new();
    for (i, fk) in fetched.iter().enumerate() {
        // The universal blocklist runs before anything is stored (§18.7).
        if blocklist.contains(&fk.key.0) {
            rejected.push((i, Loss::Blocked));
            continue;
        }
        match Info::parse(&fk.fetched.info) {
            Ok(info) if info.swarm_hashes().any(|h| blocklist.contains(&h.0)) => {
                rejected.push((i, Loss::Blocked));
            }
            Ok(info) => parsed.push((i, info)),
            // Hashes to the key but isn't usable metainfo.
            Err(_) => rejected.push((i, Loss::Invalid)),
        }
    }
    let torrents: Vec<NewTorrent<'_>> = parsed
        .iter()
        .map(|(i, info)| {
            let fk = &fetched[*i];
            NewTorrent {
                info,
                raw_info: Some(&fk.fetched.info),
                discovered_at: fk.discovered_at,
                metadata_at: now,
                extra_flags: if fk.discovered_at < backfill_until {
                    flags::BACKFILL
                } else {
                    0
                },
                source: fk.source,
            }
        })
        .collect();
    // Other sources that reported a key while it was queued, then the
    // rediscoveries.
    let mut all_touches: Vec<Touch> = parsed
        .iter()
        .map(|(i, _)| &fetched[*i])
        .filter(|fk| fk.sources != bit(fk.source))
        .map(|fk| Touch {
            hash: fk.key,
            sources: fk.sources,
        })
        .collect();
    let extra = all_touches.len();
    all_touches.extend(touches.iter().map(|d| Touch {
        hash: d.found.key,
        sources: bit(d.source),
    }));
    let written = store.write(&WriteBatch {
        torrents: &torrents,
        touches: &all_touches,
        counters,
        cohorts,
        events,
    })?;
    let stored = parsed
        .iter()
        .zip(&written.torrents)
        .map(|((i, info), ins)| {
            (
                *i,
                matches!(ins, Inserted::New(_)),
                info.swarm_hashes().collect(),
            )
        })
        .collect();
    Ok(Outcome {
        stored,
        rejected,
        touched: written.touched[extra..]
            .iter()
            .map(Option::is_some)
            .collect(),
    })
}

fn bit(source: SourceId) -> u64 {
    1u64 << source.get()
}
