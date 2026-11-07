//! The coordinator's intake of what remote workers send (architecture
//! §19.3, §19.4, §19.5, §18.7; workers plan WK-9).
//!
//! **Results.** A `Results` payload is decoded with WK-6's codec, all or
//! nothing. Each record is then fenced against the lease table: a record
//! whose token isn't the current lease of the worker that sent it is
//! dropped and counted as stale (§19.3). A survivor is rebuilt before it
//! goes to the writer, which re-judges it like every remote result
//! (`writer::rejudge`), so nothing that is stored or judged rests on the
//! worker's word:
//! - the file list is decoded from the blob, under a cap derived from the
//!   row's own `info_size` and metainfo's limits, and a batch-wide budget;
//! - `size` and `file_count` must match it, and the blob must name a
//!   dictionary the coordinator has;
//! - the classification and the enrichment are rebuilt from the name and
//!   that file list (`enrich_infos`): **the worker's enrichment record is
//!   never stored**, of any version, since search builds its adult term
//!   and tags from the stored record bytes;
//! - `sources` and `private_source` come from the lease, `metadata_at` is
//!   corrected by the worker's clock offset and kept between the key's
//!   discovery and now, and the worker's verdict, rule fix and
//!   always-adult mark are dropped (the writer judges again, and finds an
//!   always-adult source from the lease's sources);
//! - a raw dict is kept only while the coordinator's `archive_raw` is on.
//!
//! A record refused for its content ends its lease as `Invalid`, once.
//!
//! **Late results (plan Q7).** A record under a token from an earlier
//! epoch (a coordinator restart since) is committed only when its key is
//! neither stored nor in flight now, under a lease rebuilt from the
//! coordinator's own record of the key (the fetch queue it started from),
//! never the worker's. It never counts "harvested" again; the key's
//! earlier terminal outcome gets a `LATE_RESULTS` correction beside it, so
//! the funnel still reconciles. A late result from a quarantined, revoked
//! or purged worker is refused outright, and so is any result older than
//! that worker's last purge ([`Standing`], which WK-13 fills).
//!
//! **The queue (§19.5).** Results wait in a small queue in front of the
//! writer. While it's full, a batch is refused for now and the worker sends
//! it again; the sink never blocks the session.
//!
//! **Still to come (WK-9b):** counters into the rollups with a worker
//! dimension, grants held back while the writer is behind (WK-0's
//! `GrantAdmission`), and remote and waiting leases in the fetch queue.
//! Until then a `Counters` payload is refused (the worker's spool keeps
//! it) and no admission hook is set.
//!
//! **Peer hints** are relayed in memory only (§18.7), filtered with
//! `lodestar_network_policy::allows`; nothing here writes one anywhere.
//!
//! Every batch that was read is answered `accepted`, stale or refused
//! records included, so the worker calls `committed` and drops what it
//! held for it; only the queue being full answers "send it again later".

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use lodestar_cluster::proto::{HintItem, Outcome, Payload};
use lodestar_cluster::{
    ClusterServer, GrantAdmission, InboundSink, Lease, LeaseAuthority, RestoredLease, StoredKeys,
    Token, WorkerId,
};
use lodestar_filter::{Handle, HeldSet, Trail, Verdict};
use lodestar_metainfo::{InfoHash, Limits};
use lodestar_sources::{Discovery, Found, Loss};
use lodestar_store::{Preparer, QueuedKey, SourceId, flags};
use tokio::sync::mpsc;

use crate::rollups::{Admit, Rollups, metric};
use crate::wire::filter_snapshot::WireKeys;
use crate::wire::{CountersBatch, ResultRecord, ResultsBatch};
use crate::worker::{Results, WorkerLeases};

/// Payloads waiting in front of the writer, at most (each ≤ 512 KiB).
pub const INTAKE_QUEUE: usize = 16;

/// A blob may decompress to its info dict's size plus this much (the file
/// tree is never larger than the dict it was built from, plus its header).
pub const BLOB_SLACK: usize = 4 << 10;

/// What one blob may decompress to, at most: metainfo's info dict cap.
pub const MAX_BLOB: usize = Limits::DEFAULT.max_info_bytes;

/// What one batch's blobs may decompress to, in total. A record that would
/// pass it is refused alone.
pub const BATCH_BLOBS: usize = 64 << 20;

/// Path components one batch's file trees may hold in all, and so one
/// record's: metainfo's bencode item cap (`max_items`), which a single
/// largest torrent fits. Each costs a few dozen bytes once decoded, so a
/// batch's file lists stay within about 150 MB however its blobs are shaped
/// (`filetree::decode` copies a directory's path into each child's).
pub const BATCH_COMPONENTS: u64 = 2_000_000;

/// Bytes of paths one batch's file trees may hold in all (what decoding
/// copies, charged once a blob's shape passes, whether or not it then
/// decodes). Each record's are bounded by the largest info dict
/// ([`MAX_BLOB`], 16 MiB) and by what the batch has left
/// (`docs/decisions/2026-11-07-WK-9.md` has the arithmetic).
pub const BATCH_PATH_BYTES: u64 = 64 << 20;

/// What the rest of one batch may still decode.
#[derive(Debug, Clone, Copy)]
struct Budget {
    bytes: usize,
    components: u64,
    path_bytes: u64,
}

/// A remote worker's timestamps may be ahead of the coordinator's by this
/// much and still be clamped at the claim (`ServerConfig::max_skew_ms`):
/// the margin a purge cutoff allows for (seconds).
const SKEW_SECS: i64 = 60;

/// Parked keys handed to the built-in worker per heartbeat, at most.
pub const PARKED_PER_BEAT: usize = 512;

/// A late result is possible only while a spool can hold one (24 h,
/// owner §2.17); the start's records are dropped after this.
pub const LATE_FOR_SECS: i64 = 25 * 3600;

/// Whether a worker's results may still be taken (plan Q7). WK-13 fills it
/// from quarantine and purge-by-worker; until then [`NoStanding`].
pub trait Standing: Send + Sync + 'static {
    /// Every late result from `worker` is refused: it is quarantined,
    /// revoked or purged.
    fn refuses_late(&self, worker: WorkerId) -> bool;
    /// Unix seconds of `worker`'s last purge: any result fetched at or
    /// before it is refused, late or not.
    fn last_purge(&self, worker: WorkerId) -> Option<i64>;
}

/// No quarantine or purge yet (before WK-13).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStanding;

impl Standing for NoStanding {
    fn refuses_late(&self, _: WorkerId) -> bool {
        false
    }
    fn last_purge(&self, _: WorkerId) -> Option<i64> {
        None
    }
}

/// Each worker's clock offset as the coordinator measured it (§19.3):
/// worker clock − coordinator clock, in milliseconds.
pub trait Clocks: Send + Sync + 'static {
    fn offset_ms(&self, worker: WorkerId) -> i64;
}

/// Counters for metrics.
#[derive(Debug, Default)]
pub struct RemoteStats {
    /// `Results` payloads decoded, and records in them (counted once the
    /// whole batch has been taken).
    pub batches: AtomicU64,
    pub records: AtomicU64,
    /// Records handed to the writer (late ones included). A resend read
    /// before the first copy's commit ends the lease is handed over too;
    /// the writer's own fence stores it once.
    pub committed: AtomicU64,
    /// Under a token that isn't the sender's current lease (current epoch),
    /// or a late result whose key is stored or in flight.
    pub stale: AtomicU64,
    /// Late results handed to the writer, and refused outright (Q7).
    pub late: AtomicU64,
    pub late_refused: AtomicU64,
    /// Results fetched before the worker's last purge.
    pub purged: AtomicU64,
    /// Records refused because they came in clear: every remote record is
    /// sealed (WK-8), so one in clear is a misbehaving worker.
    pub clear: AtomicU64,
    /// Sealed records refused because they didn't open: an unknown or
    /// expired wire key, a seal that fails under the session's worker id,
    /// or an inside that isn't its header's one record.
    pub unopened: AtomicU64,
    /// Records refused for their content (counts, blob, dictionary, a
    /// hybrid hash naming another stored torrent).
    pub refused: AtomicU64,
    /// Payloads that didn't decode (answered, and dropped).
    pub undecodable: AtomicU64,
    /// Batches refused for now because the queue was full.
    pub backpressure: AtomicU64,
    /// Records refused because the batch's decode budget (bytes, path
    /// bytes, components) was spent before them (also in `refused`).
    pub over_budget: AtomicU64,
    /// Peer hints relayed to a holder, and dropped by the network policy.
    pub hints: AtomicU64,
    pub hints_dropped: AtomicU64,
    /// Counters accepted and refused (out of domain, out of the window,
    /// over a row or day cap).
    pub counters: AtomicU64,
    pub counters_refused: AtomicU64,
    /// New leases not granted while the writer was past its high-water
    /// mark or the intake's queue was full (per key).
    pub held_back: AtomicU64,
    /// Waiting keys handed to the built-in worker while no server ran, and
    /// those it refused (held or blocked), whose leases ended.
    pub parked: AtomicU64,
    pub parked_refused: AtomicU64,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

fn epoch_of(token: Token) -> u32 {
    (token.0 >> 32) as u32
}

/// The fetch queue the coordinator started from: its own record of keys
/// in flight before the restart, for late results (Q7).
///
/// Kept only when remote workers are allowed at the start; empty
/// otherwise, so a single machine holds nothing. Bounded by the start
/// queue, which the pool caps (`PoolConfig::max_pending`, 200,000 keys),
/// at about 30 bytes a key (the key, its cohort hour, source and
/// strategy: what the restart restored, so nothing is lost by keeping no
/// more). It never grows after the start: each key is removed when its
/// late result is committed, and the whole map is dropped
/// [`LATE_FOR_SECS`] after the start, when no spool can hold a result
/// from before it.
#[derive(Debug, Default)]
pub struct Prior {
    keys: HashMap<[u8; 20], (i32, u8, u8)>,
    until: i64,
    started_at: i64,
}

impl Prior {
    /// `queue`: the fetch queue as the start read it, restored as leases.
    pub fn new(
        queue: impl IntoIterator<Item = (InfoHash, RestoredLease)>,
        started_at: i64,
    ) -> Prior {
        let keys = queue
            .into_iter()
            .filter_map(|(k, l)| {
                Some((
                    k.0,
                    (i32::try_from(l.cohort).ok()?, l.source.get(), l.strategy),
                ))
            })
            .collect();
        Prior {
            keys,
            until: started_at.saturating_add(LATE_FOR_SECS),
            started_at,
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The key's lease as the start restored it (`lib.rs`: a refetch's
    /// discovery is the start's hour).
    fn get(&mut self, key: &InfoHash, now: i64) -> Option<RestoredLease> {
        if now > self.until && !self.keys.is_empty() {
            self.keys = HashMap::new();
        }
        let &(cohort, source, strategy) = self.keys.get(&key.0)?;
        let source = SourceId::new(source)?;
        let cohort = i64::from(cohort);
        let discovered_at = if crate::in_funnel(cohort) {
            cohort * 3600
        } else {
            self.started_at.div_euclid(3600) * 3600
        };
        Some(RestoredLease {
            cohort,
            source,
            sources: 1u64 << source.get(),
            strategy,
            discovered_at,
        })
    }

    /// Takes the key's record: `false` if there was none (already used).
    fn remove(&mut self, key: &InfoHash) -> bool {
        self.keys.remove(&key.0).is_some()
    }
}

/// What the pipeline gives the intake when it starts.
pub struct RemoteParts {
    pub authority: Arc<LeaseAuthority>,
    /// The writer's input, shared with the built-in worker.
    pub writer: mpsc::Sender<Results>,
    /// The built-in worker's pool, for hints a remote worker found.
    pub pool: mpsc::Sender<Discovery>,
    pub rollups: Arc<Rollups>,
    /// This run's first epoch (its start time): a token from below it is
    /// a late result.
    pub epoch: u32,
    pub prior: Prior,
    /// Sources whose every find is private (`finds_private`), as a bitset.
    pub private_sources: u64,
    /// The built-in worker's leases, for parked keys handed to it.
    pub leases: Arc<WorkerLeases>,
    /// The held keys and the content filter, so a parked key the built-in
    /// worker would refuse isn't handed to it (`None`: nothing refused).
    pub refusals: Option<Refusals>,
}

/// What the built-in worker refuses before fetching (`lib.rs`'s intake):
/// tombstoned or quarantined keys, and the filter's blocked hashes.
#[derive(Clone)]
pub struct Refusals {
    pub held: Arc<HeldSet>,
    pub filter: Arc<Handle>,
}

impl std::fmt::Debug for Refusals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Refusals")
            .field("held", &self.held.len())
            .finish_non_exhaustive()
    }
}

impl Refusals {
    fn refuses(&self, key: &InfoHash) -> bool {
        self.held.contains(&key.0) || self.filter.current().blocked_hash(&[key.0]).is_some()
    }
}

impl std::fmt::Debug for RemoteParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteParts")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// The coordinator's intake: one per pipeline, shared by every cluster
/// server built on it ([`RemoteIntake::sink`]).
pub struct RemoteIntake {
    authority: Arc<LeaseAuthority>,
    /// `None` once the pipeline is stopping, so the writer's input closes.
    writer: RwLock<Option<mpsc::Sender<Results>>>,
    pool: RwLock<Option<mpsc::Sender<Discovery>>>,
    rollups: Arc<Rollups>,
    epoch: u32,
    prior: Mutex<Prior>,
    private_sources: u64,
    leases: Arc<WorkerLeases>,
    refusals: Option<Refusals>,
    /// Writer-queue fill at or past which no new lease is granted.
    high_water: usize,
    admit: Mutex<Admit>,
    /// The server running now, for hints the built-in worker found.
    server: RwLock<Option<(Weak<ClusterServer>, bool)>>,
    pub stats: Arc<RemoteStats>,
}

impl std::fmt::Debug for RemoteIntake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteIntake")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the cluster port gives the intake when it opens.
pub struct SinkParts {
    /// The store's dictionaries and `archive_raw`, from the connection the
    /// port opened: built only once workers are allowed, so a single
    /// machine holds no second copy of the dictionaries.
    pub preparer: Mutex<Preparer>,
    pub standing: Arc<dyn Standing>,
    /// The wire keys sealed records open with (WK-8): today's and the
    /// day before's. Empty, nothing opens, and every record is refused.
    pub keys: Arc<WireKeys>,
    pub stored: Arc<dyn StoredKeys>,
    pub clocks: Arc<dyn Clocks>,
    /// `lodestar_network_policy::allows`' `allow_private` for hints.
    pub hints_allow_private: bool,
}

impl SinkParts {
    fn preparer_archive_raw(&self) -> bool {
        lock(&self.preparer).archive_raw()
    }
}

impl std::fmt::Debug for SinkParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SinkParts")
            .field("hints_allow_private", &self.hints_allow_private)
            .finish_non_exhaustive()
    }
}

/// One batch, as decoded and fenced.
#[derive(Debug, Default)]
pub struct Taken {
    /// For the writer.
    pub results: Vec<Results>,
}

/// Why a record was refused for its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    UnknownDictionary,
    Blob,
    Counts,
    OtherHashStored,
}

impl RemoteIntake {
    pub fn new(p: RemoteParts) -> RemoteIntake {
        let high_water = p.writer.max_capacity() * 3 / 4;
        RemoteIntake {
            authority: p.authority,
            writer: RwLock::new(Some(p.writer)),
            pool: RwLock::new(Some(p.pool)),
            rollups: p.rollups,
            epoch: p.epoch,
            prior: Mutex::new(p.prior),
            private_sources: p.private_sources,
            high_water,
            leases: p.leases,
            refusals: p.refusals,
            admit: Mutex::new(Admit::default()),
            server: RwLock::new(None),
            stats: Arc::new(RemoteStats::default()),
        }
    }

    /// Writer-queue entries waiting now (the built-in worker's included).
    pub fn writer_fill(&self) -> usize {
        self.writer().map_or(0, |w| w.max_capacity() - w.capacity())
    }

    /// Adds one `Counters` payload to the rollups, under [`Admit`]'s
    /// bounds: the fleet's own row and the worker's [`metric::BY_WORKER`]
    /// row, whose dim is `worker`, the authenticated session's. `false` if
    /// it didn't decode (it's answered and dropped all the same).
    pub fn counters(&self, worker: WorkerId, payload: &[u8], offset_ms: i64, now: i64) -> bool {
        let Ok(batch) = CountersBatch::decode(payload) else {
            bump(&self.stats.undecodable);
            return false;
        };
        let mut admit = lock(&self.admit);
        for c in batch.counters {
            let Some(c) = admit.admit(worker, c, offset_ms, now) else {
                bump(&self.stats.counters_refused);
                continue;
            };
            bump(&self.stats.counters);
            self.rollups
                .add_dim(c.hour * 3600, c.metric, c.dim, c.key, c.value);
            self.rollups.add_dim(
                c.hour * 3600,
                metric::BY_WORKER,
                worker.0,
                metric::by_worker_key(c.metric, c.dim, c.key),
                c.value,
            );
        }
        true
    }

    /// Remote and waiting leases as fetch-queue rows (§19.3: outstanding
    /// leases are saved with the coordinator's fetch queue). After a
    /// restart the built-in worker holds them, and a late result for one
    /// finds its record (plan Q7).
    pub fn queue_rows(&self) -> Vec<QueuedKey> {
        self.authority
            .outstanding_remote()
            .into_iter()
            .map(|(key, l)| QueuedKey {
                info_hash: key,
                source: l.source,
                cohort: l.cohort,
                strategy: l.strategy,
                priority: 0,
                attempts: 0,
                private_source: self.private(l.sources | (1u64 << l.source.get())),
            })
            .collect()
    }

    /// While no cluster server runs (workers off, WK-1), keys waiting for a
    /// re-grant go to the built-in worker instead of waiting for a remote
    /// one: not a harvest, with their cohort, sources and privacy kept
    /// (§18.7: a key every source of which is private never reaches public
    /// peer finding). A key the built-in worker refuses (held, or blocked
    /// by the filter) ends its lease as blocked, as the built-in worker's
    /// own intake does, and is never fetched. A key the pool has no room
    /// for goes back to wait.
    pub fn hand_parked(&self, now: i64) {
        let serving = self
            .server
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(s, _)| s.strong_count() > 0);
        if serving || self.authority.pending() == 0 {
            return;
        }
        let Some(pool) = self.pool() else { return };
        for (_, keys) in self
            .authority
            .regrant_pending(&[WorkerId::LOCAL], PARKED_PER_BEAT, now)
        {
            for (key, token) in keys {
                let Some(lease) = self.authority.current(&key, token) else {
                    continue;
                };
                if self.refusals.as_ref().is_some_and(|r| r.refuses(&key)) {
                    self.authority.lost(&key, token, Loss::Blocked as u8);
                    bump(&self.stats.parked_refused);
                    continue;
                }
                let mut found = Found::key(key, lease.strategy);
                found.private_source = self.private(lease.sources | (1u64 << lease.source.get()));
                let d = Discovery {
                    source: lease.source,
                    found,
                    at: lease.discovered_at,
                    reply: false,
                };
                self.leases.insert(key, token);
                if pool.try_send(d).is_err() {
                    self.leases.take(&key);
                    self.authority.requeue(&key, token);
                } else {
                    bump(&self.stats.parked);
                }
            }
        }
    }

    fn writer(&self) -> Option<mpsc::Sender<Results>> {
        self.writer
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn pool(&self) -> Option<mpsc::Sender<Discovery>> {
        self.pool
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The pipeline is stopping: lets go of the writer's and the pool's
    /// inputs, so they close when the built-in worker's do. Anything still
    /// arriving is refused (and stays in the worker's spool).
    pub fn close(&self) {
        *self.writer.write().unwrap_or_else(PoisonError::into_inner) = None;
        *self.pool.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Keys of the start's fetch queue still kept for late results (0 with
    /// workers off at the start, or 25 h after it).
    pub fn late_records(&self) -> usize {
        let mut prior = lock(&self.prior);
        let _ = prior.get(&InfoHash([0; 20]), crate::unix_now());
        prior.len()
    }

    /// Whether every source of a key is a private one (§18.7).
    fn private(&self, sources: u64) -> bool {
        self.private_sources != 0 && sources & !self.private_sources == 0
    }

    /// Takes one `Results` payload from `worker`: decodes it, fences every
    /// record and rebuilds the survivors. Blocking (decompression and, for
    /// late results, store reads). `None` if it didn't decode.
    pub fn take(
        &self,
        worker: WorkerId,
        payload: &[u8],
        parts: &SinkParts,
        now: i64,
    ) -> Option<Taken> {
        let Ok((clear, sealed)) = ResultsBatch::decode_with(payload) else {
            bump(&self.stats.undecodable);
            return None;
        };
        bump(&self.stats.batches);
        let n = (clear.records.len() + sealed.len()) as u64;
        // Every remote record is sealed (WK-8): only the built-in worker,
        // which never comes here, sends one in clear. One in clear is
        // refused and counted, a signal for WK-13; one that doesn't open
        // (an unknown or old key, a seal that fails under this session's
        // worker id, or an inside that isn't the clear header's record)
        // is refused alone and counted, never logged. Either ends its
        // lease once, as any refused record does, if the sender holds it.
        let refuse = |key: &InfoHash, token: Token, count: &AtomicU64| {
            bump(count);
            if epoch_of(token) == self.epoch {
                self.authority
                    .report(worker, key, token, Some(Loss::Invalid as u8));
            }
        };
        for rec in &clear.records {
            refuse(&rec.key, rec.token, &self.stats.clear);
        }
        drop(clear);
        // Each seal is dropped as it's opened: its ciphertext isn't kept
        // beside its plaintext.
        let mut records = Vec::with_capacity(sealed.len());
        for s in sealed {
            match parts.keys.open(&s, worker) {
                Ok(rec) => records.push(rec),
                Err(_) => refuse(&s.key, s.token, &self.stats.unopened),
            }
        }
        let offset = parts.clocks.offset_ms(worker);
        let purge = parts.standing.last_purge(worker);
        let mut budget = Budget {
            bytes: BATCH_BLOBS,
            components: BATCH_COMPONENTS,
            path_bytes: BATCH_PATH_BYTES,
        };
        let mut out = Taken::default();
        for rec in records {
            let fetched_at = corrected(rec.metadata_at, offset);
            let late = epoch_of(rec.token) < self.epoch;
            if late {
                if parts.standing.refuses_late(worker) {
                    bump(&self.stats.late_refused);
                    continue;
                }
                if let Some(r) = self.late(worker, rec, fetched_at, purge, parts, &mut budget, now)
                {
                    out.results.push(r);
                }
                continue;
            }
            let Some(lease) = self.authority.held(worker, &rec.key, rec.token) else {
                bump(&self.stats.stale);
                continue;
            };
            // Before the worker's last purge, by the coordinator's own time
            // for the lease (its discovery, clamped to the claim's clock),
            // never a time the worker chose.
            if purged(purge, lease.discovered_at) {
                bump(&self.stats.purged);
                // Ends once, so the key isn't stranded once the worker
                // forgets it.
                self.authority
                    .report(worker, &rec.key, rec.token, Some(Loss::Invalid as u8));
                continue;
            }
            let (key, token) = (rec.key, rec.token);
            match self.rebuild(worker, rec, &lease, fetched_at, parts, &mut budget, now) {
                Ok(r) => {
                    bump(&self.stats.committed);
                    out.results.push(r);
                }
                Err(_) => {
                    bump(&self.stats.refused);
                    // Ends once: the lease is the sender's, under `token`.
                    self.authority
                        .report(worker, &key, token, Some(Loss::Invalid as u8));
                }
            }
        }
        // Counted once each has its outcome, so a reader of the counters
        // sees every record's outcome with its count.
        // Release: a reader that sees the count sees every outcome above.
        self.stats.records.fetch_add(n, Ordering::Release);
        Some(out)
    }

    /// A late result (Q7): committed under the coordinator's own record of
    /// the key, only when it's neither stored nor in flight now.
    #[allow(clippy::too_many_arguments)]
    fn late(
        &self,
        worker: WorkerId,
        rec: ResultRecord,
        fetched_at: i64,
        purge: Option<i64>,
        parts: &SinkParts,
        budget: &mut Budget,
        now: i64,
    ) -> Option<Results> {
        let key = rec.key;
        let Some(record) = lock(&self.prior).get(&key, now) else {
            // No record of ours: nothing to attribute it to.
            bump(&self.stats.stale);
            return None;
        };
        // Discovered (by our own record) before the worker's last purge.
        if purged(purge, record.discovered_at) {
            bump(&self.stats.purged);
            return None;
        }
        // Taken before the store check, so the lease below knows whether
        // every commit since is still remembered.
        let checked_at = self.authority.commit_seq();
        if self.authority.in_flight(&key) || parts.stored.stored(&[key])[0] {
            bump(&self.stats.stale);
            return None;
        }
        let lease = Lease {
            holder: worker,
            token: Token(0),
            cohort: record.cohort,
            source: record.source,
            sources: record.sources,
            strategy: record.strategy,
            discovered_at: record.discovered_at,
        };
        // Its record is used up here, before the costly rebuild, and taking
        // it is the check: of any number of copies, however they arrive,
        // one finds the record; the rest are stale and never rebuilt.
        // Rebuilt before any lease exists, so a refusal counts nothing.
        if !lock(&self.prior).remove(&key) {
            bump(&self.stats.stale);
            return None;
        }
        let Ok(mut r) = self.rebuild(worker, rec, &lease, fetched_at, parts, budget, now) else {
            bump(&self.stats.refused);
            return None;
        };
        // Not a harvest. Leased only if it's still neither held nor waiting,
        // in one step under the authority's lock: a claim, an expiry or
        // another feed in between makes this record stale, never a second
        // lease or a second correction.
        let Some(token) = self
            .authority
            .restore_if_free(worker, key, record, now, checked_at)
        else {
            bump(&self.stats.stale);
            return None;
        };
        r.token = token;
        // Its first lease ended with a terminal outcome (it isn't stored or
        // in flight), and this one will end with another: one correction
        // beside them keeps harvested = Σ terminal − late + in flight.
        if crate::in_funnel(record.cohort) {
            self.rollups.add(
                record.cohort * 3600,
                metric::LATE_RESULTS,
                record.source,
                0,
                1,
            );
        }
        bump(&self.stats.late);
        bump(&self.stats.committed);
        Some(r)
    }

    /// The record as the coordinator rebuilds it (module docs).
    #[allow(clippy::too_many_arguments)]
    fn rebuild(
        &self,
        worker: WorkerId,
        rec: ResultRecord,
        lease: &Lease,
        fetched_at: i64,
        parts: &SinkParts,
        budget: &mut Budget,
        now: i64,
    ) -> Result<Results, Refusal> {
        let mut torrent = rec.torrent;
        let (files, derived) = {
            let preparer = lock(&parts.preparer);
            if let Some(id) = torrent.files.dict_id
                && !preparer.has_dict(id)
            {
                return Err(Refusal::UnknownDictionary);
            }
            let own = usize::try_from(torrent.info_size)
                .unwrap_or(usize::MAX)
                .saturating_add(BLOB_SLACK)
                .min(MAX_BLOB);
            let cap = own.min(budget.bytes);
            // A refused blob may have taken its whole cap: charged as such,
            // so a batch of bombs costs the budget, not 1,024 caps. Its
            // shape is read before any path is built. Directory paths are
            // copied into each child's, which the info dict doesn't spell
            // out: an honest deep tree can outweigh it, so a record's paths
            // are bounded by the largest dict (`MAX_BLOB`, 16 MiB) and the
            // batch's rest.
            let paths = budget.path_bytes.min(MAX_BLOB as u64);
            let measured = preparer.measure_files(&torrent.files, cap, budget.components, paths);
            let (raw, shape) = match measured {
                Ok(m) => m,
                Err(e) => {
                    budget.bytes = budget.bytes.saturating_sub(cap);
                    // Over what the batch has left: refused before decoding.
                    let over = match &e {
                        lodestar_store::Error::Corrupt(why) => *why == lodestar_store::OVER_BUDGET,
                        // Over a cap the batch's remaining bytes set. A bomb
                        // over its own cap too is counted here then: only
                        // this statistic, never what's refused.
                        lodestar_store::Error::Compression(io) => {
                            io.kind() == std::io::ErrorKind::InvalidData && cap < own
                        }
                        _ => false,
                    };
                    if over {
                        bump(&self.stats.over_budget);
                    }
                    return Err(Refusal::Blob);
                }
            };
            // Charged in full once the shape passes: decoding builds every
            // directory path before it can fail on what `shape` doesn't
            // read (names, sizes, trailing bytes).
            budget.bytes = budget.bytes.saturating_sub(raw.len());
            budget.components = budget.components.saturating_sub(shape.components);
            budget.path_bytes = budget.path_bytes.saturating_sub(shape.path_bytes);
            let tree = lodestar_store::filetree::decode(&raw).map_err(|_| Refusal::Blob)?;
            drop(raw);
            let derived = derived_flags(&tree.files);
            let infos = crate::enrich::file_infos(&tree.files, tree.encoding);
            // The tree goes here: only the file list is kept.
            drop(tree);
            (infos, derived)
        };
        let content = files.iter().filter(|f| !f.padding);
        let size = content
            .clone()
            .try_fold(0u64, |s, f| s.checked_add(f.size))
            .ok_or(Refusal::Counts)?;
        if size != torrent.size || content.count() as u64 != torrent.file_count {
            return Err(Refusal::Counts);
        }
        check_pieces(&torrent, &files)?;
        // The file-list flags are rebuilt, never taken as sent.
        if !derived.single_file && torrent.flags & flags::SINGLE_FILE != 0 {
            return Err(Refusal::Counts);
        }
        torrent.flags = (torrent.flags & !(flags::PADDING | flags::SYMLINKS)) | derived.bits;
        // `PRIVATE` is the worker's word: every privacy gate treats the row
        // as private until WK-13 re-verifies it (fail closed).
        torrent.flags |= flags::REMOTE_UNVERIFIED;
        // A hybrid's other hash is the worker's word: a record that would
        // merge into another stored torrent through it is refused.
        let other: Vec<InfoHash> = torrent.swarm_hashes().filter(|h| *h != rec.key).collect();
        if !other.is_empty() && parts.stored.stored(&other).iter().any(|s| *s) {
            return Err(Refusal::OtherHashStored);
        }
        if !parts.preparer_archive_raw() {
            torrent.raw = None;
        }
        // Content safety: the coordinator's own enrichment, never the
        // worker's bytes (search reads `Enriched::data`).
        let (enriched, class) = crate::enrich::enrich_infos(&torrent.name, &files);
        torrent.enrichment = Some(enriched);
        let sources = lease.sources | (1u64 << lease.source.get());
        let private_source = self.private(sources);
        Ok(Results {
            key: rec.key,
            token: lease.token,
            worker,
            torrent,
            sources,
            metadata_at: fetched_at.clamp(lease.discovered_at.min(now), now),
            files,
            class,
            // Judged again at commit, as every remote result is.
            verdict: Verdict::default(),
            version: 0,
            trail: Trail::default(),
            fix: None,
            private_source,
            source_adult: false,
        })
    }

    /// Hints the built-in worker found for a key a remote worker holds:
    /// relayed to it in memory, through the server running now.
    pub fn relay_local(&self, key: InfoHash, token: Token, peers: &[SocketAddr]) -> bool {
        let server = self
            .server
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|(s, private)| s.upgrade().map(|s| (s, *private)));
        let Some((server, allow_private)) = server else {
            return false;
        };
        let hints = self.allowed(peers, allow_private);
        if hints.is_empty() {
            return false;
        }
        let sent = server.relay_hints(key, token, hints);
        if sent {
            bump(&self.stats.hints);
        }
        sent
    }

    fn allowed(&self, peers: &[SocketAddr], allow_private: bool) -> Vec<SocketAddr> {
        let kept: Vec<SocketAddr> = peers
            .iter()
            .copied()
            .filter(|a| lodestar_network_policy::allows(*a, allow_private))
            .collect();
        let dropped = (peers.len() - kept.len()) as u64;
        self.stats
            .hints_dropped
            .fetch_add(dropped, Ordering::Relaxed);
        kept
    }

    /// The sink and admission hook for a cluster server about to be built
    /// on this pipeline. Spawns the task that feeds the writer; it ends
    /// when the server (and so the sink) is dropped.
    pub fn sink(self: &Arc<Self>, parts: SinkParts) -> Arc<RemoteSink> {
        let (tx, rx) = mpsc::channel(INTAKE_QUEUE);
        let parts = Arc::new(parts);
        tokio::spawn(feed(Arc::clone(self), Arc::clone(&parts), rx));
        Arc::new(RemoteSink {
            intake: Arc::clone(self),
            parts,
            queue: tx,
        })
    }

    /// The server now serving, for [`relay_local`](Self::relay_local).
    pub fn serving(&self, server: Option<&Arc<ClusterServer>>, hints_allow_private: bool) {
        *self.server.write().unwrap_or_else(PoisonError::into_inner) =
            server.map(|s| (Arc::downgrade(s), hints_allow_private));
    }
}

/// Whether a lease discovered at `at` (the coordinator's own time) comes
/// from before the worker's last purge, allowing for the skew a claim may
/// carry.
fn purged(purge: Option<i64>, at: i64) -> bool {
    purge.is_some_and(|p| at <= p.saturating_add(SKEW_SECS))
}

/// The file-list flags as `Preparer` derives them, from the decoded list.
struct Derived {
    bits: i64,
    /// The list can be a single-file torrent's (one file whose path is a
    /// single name), as `SINGLE_FILE` needs.
    single_file: bool,
}

fn derived_flags(files: &[lodestar_metainfo::File]) -> Derived {
    let mut bits = 0;
    if files.iter().any(lodestar_metainfo::File::is_padding) {
        bits |= flags::PADDING;
    }
    if files
        .iter()
        .any(|f| f.attrs.contains(lodestar_metainfo::FileAttrs::SYMLINK))
    {
        bits |= flags::SYMLINKS;
    }
    Derived {
        bits,
        single_file: files.len() == 1 && files[0].path.len() == 1,
    }
}

/// `piece_len` as metainfo accepts it, and `piece_count` as it follows
/// from it and the file list: exactly for v2 (per file), and for v1 unless
/// metainfo flagged a mismatch in the dict (`WARNINGS`), as it keeps the
/// dict's own count then.
fn check_pieces(
    t: &lodestar_store::Prepared,
    files: &[lodestar_classify::FileInfo],
) -> Result<(), Refusal> {
    let v2 = t.flags & flags::V2 != 0;
    let hybrid = t.flags & flags::HYBRID != 0;
    if t.piece_len == 0 || (v2 && (t.piece_len < 16 << 10 || !t.piece_len.is_power_of_two())) {
        return Err(Refusal::Counts);
    }
    let expected = if v2 && !hybrid {
        files
            .iter()
            .filter(|f| !f.padding)
            .map(|f| f.size.div_ceil(t.piece_len))
            .try_fold(0u64, u64::checked_add)
    } else {
        files
            .iter()
            .try_fold(0u64, |s, f| s.checked_add(f.size))
            .map(|all| all.div_ceil(t.piece_len))
    }
    .ok_or(Refusal::Counts)?;
    if t.piece_count != expected && (t.flags & flags::WARNINGS == 0 || v2 && !hybrid) {
        return Err(Refusal::Counts);
    }
    Ok(())
}

/// A worker's timestamp on the coordinator's clock (§19.3).
fn corrected(at: i64, offset_ms: i64) -> i64 {
    at.saturating_sub(offset_ms.div_euclid(1000))
}

/// Takes batches off the queue in order, one at a time, and hands their
/// records to the writer (waiting on it: the queue then fills, and new
/// batches are refused for now).
async fn feed(
    intake: Arc<RemoteIntake>,
    parts: Arc<SinkParts>,
    mut rx: mpsc::Receiver<(WorkerId, Payload)>,
) {
    while let Some((worker, payload)) = rx.recv().await {
        let (i, p) = (Arc::clone(&intake), Arc::clone(&parts));
        let taken = tokio::task::spawn_blocking(move || {
            i.take(worker, &payload.bytes, &p, crate::unix_now())
        })
        .await;
        // `take` doesn't panic on any input; were it to, that batch's leases
        // would last while the worker heartbeats, as an undecodable one's do.
        let Ok(Some(taken)) = taken else { continue };
        // Stopping: what's taken isn't committed, and its leases (and a late
        // result's correction) are left as a crash would leave them.
        let Some(writer) = intake.writer() else {
            return;
        };
        for r in taken.results {
            if writer.send(r).await.is_err() {
                return;
            }
        }
    }
}

/// The cluster server's [`InboundSink`] and [`GrantAdmission`].
pub struct RemoteSink {
    intake: Arc<RemoteIntake>,
    parts: Arc<SinkParts>,
    queue: mpsc::Sender<(WorkerId, Payload)>,
}

impl std::fmt::Debug for RemoteSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSink").finish_non_exhaustive()
    }
}

impl RemoteSink {
    /// Batches waiting for the writer.
    pub fn queued(&self) -> usize {
        self.queue.max_capacity() - self.queue.capacity()
    }
}

impl GrantAdmission for RemoteSink {
    /// No new lease while the writer is past its high-water mark or the
    /// intake's queue is full (§19.5): workers harvest less, and nothing
    /// piles up. Keys already held aren't asked about, and late results
    /// never come through a grant.
    fn admit(&self, _worker: WorkerId, wanted: usize) -> usize {
        if self.intake.writer_fill() >= self.intake.high_water || self.queue.capacity() == 0 {
            self.intake
                .stats
                .held_back
                .fetch_add(wanted as u64, Ordering::Relaxed);
            0
        } else {
            wanted
        }
    }
}

impl InboundSink for RemoteSink {
    fn results(&self, worker: WorkerId, batch: Payload) -> bool {
        match self.queue.try_send((worker, batch)) {
            Ok(()) => true,
            // Full: "send it again later". Closed can't happen while the
            // sink lives; refusing keeps the batch in the worker's spool.
            Err(_) => {
                bump(&self.intake.stats.backpressure);
                false
            }
        }
    }

    fn counters(&self, worker: WorkerId, batch: Payload) -> bool {
        let offset = self.parts.clocks.offset_ms(worker);
        self.intake
            .counters(worker, &batch.bytes, offset, crate::unix_now());
        // Answered even when refused or undecodable: nothing in it is kept
        // for later, so the worker's spool can let it go.
        true
    }

    /// The server ended their leases (losses) or kept them (`Fetched`,
    /// which the commit ends); nothing is stored for them here.
    fn outcomes(&self, _worker: WorkerId, _accepted: Vec<(InfoHash, Lease, Outcome)>) {}

    /// Hints for keys the built-in worker holds: into its pool as a reply,
    /// so they're merged into a queued key and never admit one.
    fn local_hints(&self, hints: Vec<HintItem>) {
        for item in hints {
            let key = InfoHash(item.key);
            let Some(lease) = self.intake.authority.current(&key, Token(item.token)) else {
                continue;
            };
            if lease.holder != WorkerId::LOCAL {
                continue;
            }
            let peers = self
                .intake
                .allowed(&item.hints, self.parts.hints_allow_private);
            if peers.is_empty() {
                continue;
            }
            let mut found = Found::key(key, lease.strategy);
            found.peers = peers;
            // A reply only adds peers to the pool's entry. Merged, its flag
            // can only make the entry public (`private &= private_source`,
            // `pool.rs`), so it's set: a worker's hints never make a
            // private key public (§18.7).
            found.private_source = true;
            let d = Discovery {
                source: lease.source,
                found,
                at: crate::unix_now(),
                reply: true,
            };
            if self.intake.pool().is_some_and(|p| p.try_send(d).is_ok()) {
                bump(&self.intake.stats.hints);
            }
        }
    }
}

/// The private-sources bitset from sources' manifests.
pub fn private_sources<'a>(sources: impl IntoIterator<Item = (SourceId, bool)> + 'a) -> u64 {
    sources
        .into_iter()
        .filter(|(_, private)| *private)
        .fold(0, |m, (id, _)| m | (1u64 << id.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_name_their_epoch() {
        assert_eq!(epoch_of(Token((7u64 << 32) | 9)), 7);
    }

    #[test]
    fn timestamps_are_corrected_by_the_offset() {
        // The worker's clock is 90 s ahead.
        assert_eq!(corrected(1_000, 90_000), 910);
        assert_eq!(corrected(1_000, -1_500), 1_002);
        assert_eq!(corrected(i64::MIN, 1), i64::MIN);
    }

    #[test]
    fn the_start_queue_is_kept_only_while_a_spool_can_hold_a_result() {
        let key = InfoHash([1; 20]);
        let lease = RestoredLease {
            cohort: 5,
            source: SourceId::new(1).unwrap(),
            sources: 2,
            strategy: 4,
            discovered_at: 18_000,
        };
        let mut p = Prior::new([(key, lease)], 100);
        // What the start restored: the cohort's hour, its one source.
        assert_eq!(p.get(&key, 100 + LATE_FOR_SECS), Some(lease));
        assert_eq!(p.len(), 1);
        // 25 h after the start, on the clock it's given: gone, all of it.
        assert_eq!(p.get(&key, 101 + LATE_FOR_SECS), None);
        assert!(p.is_empty());
        assert_eq!(LATE_FOR_SECS, 25 * 3600);
    }

    #[test]
    fn private_sources_make_a_bitset() {
        let s = |n| SourceId::new(n).unwrap();
        assert_eq!(
            private_sources([(s(1), true), (s(2), false), (s(5), true)]),
            0b10_0010
        );
    }
}
