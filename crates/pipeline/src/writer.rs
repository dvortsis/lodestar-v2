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
use lodestar_filter::{Action, Decision, Engine, Facts, Handle, HeldSet, Item, Sealer, Verdict};
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
    /// Updated after each commit, so SQLite is always ahead of them.
    pub hot: Option<Arc<HotColumns>>,
    /// Told about rediscoveries, to scrape them sooner (§5).
    pub health: Option<HealthHandle>,
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
    stored: Vec<(usize, bool, Vec<InfoHash>)>,
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
                _ = flush.tick() => {
                    if self.reenrich.swap(false, Ordering::AcqRel) {
                        self.backfill_after = 0;
                        self.backfill_done = false;
                    }
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
                tracing::warn!("WAL checkpoint at shutdown failed: {e}");
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

    /// Removes expired quarantine (its keys become fetchable again) and
    /// filter hits past their retention.
    async fn purge(&mut self) {
        let now = unix_now();
        self.last_purge = now;
        let hits_before = now - self.cfg.hits_secs;
        let mut store = self.store.take().expect("store");
        let (store, result) = tokio::task::spawn_blocking(move || {
            let r = store.purge_filter(now, hits_before);
            (store, r)
        })
        .await
        .expect("purge task");
        self.store = Some(store);
        match result {
            Ok((expired, _)) => {
                for h in expired {
                    self.held.remove(&h);
                }
            }
            Err(e) => tracing::warn!("purging filter state failed: {e}"),
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
        // The classification rules' generation before the rules
        // themselves: a batch judged under rules older than the store's
        // generation is marked for enriching again (§20.9).
        let class_gen = self.store.as_ref().and_then(|s| s.class_gen().ok());
        let committing = self.judge(committing);
        let cohorts = self.funnel.drain();
        let counters = self.rollups.drain();
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
        let (store, committing, touches, swarm, result) = tokio::task::spawn_blocking(move || {
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
                tracing::error!("store write failed: {e}");
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
            let counter = match c.verdict.as_ref().map(|h| h.action) {
                Some(Action::Quarantine) => &self.stats.quarantined,
                _ => &self.stats.blocked,
            };
            counter.fetch_add(1, Ordering::Relaxed);
            for h in c.results.torrent.swarm_hashes() {
                self.held.insert(h.0);
            }
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
        let (store, result) = tokio::task::spawn_blocking(move || {
            let mut store = store;
            let ctx = crate::enrich::Backfill {
                filter: Some(&filter),
                sealer: sealer.as_deref(),
                quarantine_secs,
            };
            let result = crate::enrich::enrich_stored_with(&mut store, after, BACKFILL_BATCH, &ctx);
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
                for h in b.held {
                    self.held.insert(h);
                }
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
                } | marks(committing[i].verdict.as_ref()),
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
}
