//! The worker side of the boundary (architecture §19.4): what a worker
//! does between finding a key and handing a finished torrent to the
//! coordinator. M3 runs one built-in worker ([`WorkerId::LOCAL`]) in this
//! process; remote workers will run the same stages behind the network.
//!
//! - Intake claims each new key from the coordinator (see `intake`) and
//!   keeps the lease's token here.
//! - The fetch pool reports its losses through [`WorkerFunnel`], which
//!   hands them to the coordinator under that token.
//! - The prepare stage parses each fetched info dict, builds the row and
//!   blob, enriches it (release parse, classification) and runs the
//!   content filter (§20.4) into [`Results`], so the writer only commits.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{ContentType, FileInfo};
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::{Decision, Engine, Handle, Item};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_sources::{FetchedKey, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{Prepared, Preparer, SourceId};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

use crate::coordinator::Coordinator;
use crate::unix_now;
use crate::writer::WriterStats;

/// The tokens of the leases this worker holds.
#[derive(Debug, Default)]
pub(crate) struct WorkerLeases(Mutex<HashMap<InfoHash, Token>>);

impl WorkerLeases {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<InfoHash, Token>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn insert(&self, key: InfoHash, token: Token) {
        self.map().insert(key, token);
    }

    /// The key's token, removed: the lease ends with this report.
    pub fn take(&self, key: &InfoHash) -> Option<Token> {
        self.map().remove(key)
    }
}

/// The pool's funnel on a worker: progress stages count directly, the
/// harvest was counted at grant, and losses go to the coordinator under
/// the lease's token.
pub(crate) struct WorkerFunnel {
    pub counts: Arc<FunnelCounts>,
    pub coordinator: Arc<Coordinator>,
    pub leases: Arc<WorkerLeases>,
}

impl Funnel for WorkerFunnel {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) {
        self.counts.record(cohort, source, stage, loss);
    }

    fn admitted(&self, _key: &InfoHash, _cohort: i64, _source: SourceId) {}

    fn lost(&self, key: &InfoHash, _cohort: i64, _source: SourceId, loss: Loss) {
        match self.leases.take(key) {
            Some(token) => self.coordinator.lost(key, token, loss),
            // Every key enters the pool under a lease; one without is a
            // bug, and counting it would unbalance the funnel.
            None => tracing::warn!(key = %key, "a pool loss without a lease"),
        }
    }
}

/// A prepared torrent on its way to the coordinator (the `Results`
/// record, §19.4). Provenance beyond what the worker saw comes from the
/// coordinator's lease.
#[derive(Debug)]
pub(crate) struct Results {
    pub key: InfoHash,
    pub token: Token,
    pub worker: WorkerId,
    pub torrent: Prepared,
    /// Sources the worker saw for the key (peer finders included).
    pub sources: u64,
    pub metadata_at: i64,
    /// What the filter needs to judge again at commit.
    pub files: Vec<FileInfo>,
    pub content_type: ContentType,
    /// The worker's filter verdict, and the rule-set version it used.
    pub verdict: Option<Decision>,
    pub version: u64,
}

pub(crate) struct PrepareStage {
    pub preparer: Preparer,
    pub filter: Arc<Handle>,
    pub coordinator: Arc<Coordinator>,
    pub leases: Arc<WorkerLeases>,
    pub stats: Arc<WriterStats>,
    pub batch_max: usize,
    pub batch_wait: Duration,
}

impl PrepareStage {
    /// Runs until the pool's output closes. Waiting on `out` passes the
    /// writer's backpressure on to the pool.
    pub async fn run(
        mut self,
        mut fetched: mpsc::Receiver<FetchedKey>,
        out: mpsc::Sender<Results>,
    ) {
        while let Some(first) = fetched.recv().await {
            let batch = collect(first, &mut fetched, self.batch_max, self.batch_wait).await;
            let preparer = self.preparer;
            let engine = self.filter.current();
            let (preparer, done) = tokio::task::spawn_blocking(move || {
                let mut preparer = preparer;
                let done: Vec<_> = batch
                    .into_iter()
                    .map(|fk| {
                        let r = prepare_one(&mut preparer, &engine, &fk);
                        (fk, r)
                    })
                    .collect();
                (preparer, done)
            })
            .await
            .expect("prepare task");
            self.preparer = preparer;
            for (fk, result) in done {
                let Some(token) = self.leases.take(&fk.key) else {
                    tracing::warn!(key = %fk.key, "a fetched key without a lease");
                    continue;
                };
                match result {
                    Ok(done) => {
                        let r = Results {
                            key: fk.key,
                            token,
                            worker: WorkerId::LOCAL,
                            torrent: done.torrent,
                            sources: fk.sources,
                            metadata_at: unix_now(),
                            files: done.files,
                            content_type: done.content_type,
                            verdict: done.verdict,
                            version: done.version,
                        };
                        if out.send(r).await.is_err() {
                            return;
                        }
                    }
                    Err(loss) => {
                        let counter = match loss {
                            Loss::Blocked => &self.stats.blocked,
                            _ => &self.stats.invalid,
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        self.coordinator.lost(&fk.key, token, loss);
                    }
                }
            }
        }
    }
}

/// A prepared torrent with what the filter made of it.
struct Done {
    torrent: Prepared,
    files: Vec<FileInfo>,
    content_type: ContentType,
    verdict: Option<Decision>,
    version: u64,
}

/// Parses, prepares, enriches and judges one fetched info dict. A key in
/// the floor's hash list is refused before parsing; every other verdict
/// goes to the writer, which records it and re-checks it (§19.4).
fn prepare_one(preparer: &mut Preparer, engine: &Engine, fk: &FetchedKey) -> Result<Done, Loss> {
    if engine.blocked_hash(&[fk.key.0]).is_some() {
        return Err(Loss::Blocked);
    }
    // Hashes to the key but isn't usable metainfo: invalid.
    let info = Info::parse(&fk.fetched.info).map_err(|_| Loss::Invalid)?;
    let mut prepared = preparer
        .prepare(&info, Some(&fk.fetched.info))
        .map_err(|e| {
            tracing::warn!(key = %fk.key, "preparing failed: {e}");
            Loss::Invalid
        })?;
    // Enrichment runs here, on the worker side (§19.4).
    let files = crate::enrich::file_infos(&info.files, info.path_encoding);
    let (enriched, content_type) = crate::enrich::enrich_infos(&prepared.name, &files);
    prepared.enrichment = Some(enriched);
    let hashes: Vec<[u8; 20]> = prepared.swarm_hashes().map(|h| h.0).collect();
    let verdict = engine.judge(&Item {
        hashes: &hashes,
        name: &prepared.name,
        files: &files,
        content_type,
    });
    Ok(Done {
        torrent: prepared,
        files,
        content_type,
        verdict,
        version: engine.version,
    })
}

/// Gathers more items for the same batch, briefly.
pub(crate) async fn collect<T>(
    first: T,
    rx: &mut mpsc::Receiver<T>,
    max: usize,
    wait: Duration,
) -> Vec<T> {
    let mut batch = vec![first];
    let deadline = Instant::now() + wait;
    while batch.len() < max {
        match rx.try_recv() {
            Ok(x) => batch.push(x),
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => match timeout_at(deadline, rx.recv()).await {
                Ok(Some(x)) => batch.push(x),
                Ok(None) | Err(_) => break,
            },
        }
    }
    batch
}
