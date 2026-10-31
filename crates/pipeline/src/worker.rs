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

use lodestar_classify::{Classification, FileInfo};
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::{Engine, Facts, Handle, Item, Verdict};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::FailureKind;
use lodestar_sources::{FetchedKey, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{Prepared, Preparer, SourceId};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

use crate::coordinator::Coordinator;
use crate::gate::{Excluded, GateStats};
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
    pub gate: Arc<GateStats>,
}

impl Funnel for WorkerFunnel {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) {
        if crate::in_funnel(cohort) {
            self.counts.record(cohort, source, stage, loss);
        }
    }

    fn admitted(&self, _key: &InfoHash, _cohort: i64, _source: SourceId) {}

    fn lost(
        &self,
        key: &InfoHash,
        _cohort: i64,
        _source: SourceId,
        loss: Loss,
        cause: Option<FailureKind>,
    ) {
        match self.leases.take(key) {
            Some(token) => {
                // Counted where the lease ends, once per key: the pool
                // gave it up on data that didn't hash to it. A protocol
                // violation is an `Invalid` loss too, but not this.
                if self.coordinator.lost(key, token, loss)
                    && loss == Loss::Invalid
                    && cause == Some(FailureKind::HashMismatch)
                {
                    self.gate.exclude(Excluded::FailedVerification, 1);
                }
            }
            // Every key enters the pool under a lease; one without is a
            // bug, and counting it would unbalance the funnel.
            None => lodestar_log::log_event!(
                crate::log_events::LEASE_MISSING,
                key = lodestar_log::Torrent(&key.0),
                stage = "lost"
            ),
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
    pub class: Classification,
    /// The worker's filter verdict, and the rule-set version it used.
    pub verdict: Verdict,
    pub version: u64,
    /// The classification rule applied to the stored enrichment (§20.9):
    /// Is adult or Not adult, where it differs from detection.
    pub fix: Option<bool>,
    /// Only private sources reported the key.
    pub private_source: bool,
}

pub(crate) struct PrepareStage {
    pub preparer: Preparer,
    pub filter: Arc<Handle>,
    pub coordinator: Arc<Coordinator>,
    pub leases: Arc<WorkerLeases>,
    pub stats: Arc<WriterStats>,
    pub gate: Arc<GateStats>,
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
                    lodestar_log::log_event!(
                        crate::log_events::LEASE_MISSING,
                        key = lodestar_log::Torrent(&fk.key.0),
                        stage = "fetched"
                    );
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
                            class: done.class,
                            verdict: done.verdict,
                            version: done.version,
                            fix: done.fix,
                            private_source: fk.private_source,
                        };
                        if out.send(r).await.is_err() {
                            return;
                        }
                    }
                    Err(loss) => {
                        let (counter, excluded) = match loss {
                            Loss::Blocked => (&self.stats.blocked, Excluded::Filtered),
                            _ => (&self.stats.invalid, Excluded::InvalidMetainfo),
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        if self.coordinator.lost(&fk.key, token, loss) {
                            self.gate.exclude(excluded, 1);
                        }
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
    class: Classification,
    verdict: Verdict,
    version: u64,
    fix: Option<bool>,
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
            lodestar_log::log_event!(
                crate::log_events::PREPARE_FAILED,
                key = lodestar_log::Torrent(&fk.key.0),
                error = lodestar_log::Error::new(&e)
            );
            Loss::Invalid
        })?;
    // Enrichment runs here, on the worker side (§19.4).
    let files = crate::enrich::file_infos(&info.files, info.path_encoding);
    let (enriched, class) = crate::enrich::enrich_infos(&prepared.name, &files);
    prepared.enrichment = Some(enriched);
    let hashes: Vec<[u8; 20]> = prepared.swarm_hashes().map(|h| h.0).collect();
    let now = unix_now();
    let facts = Facts {
        class: &class,
        size: prepared.size,
        piece_len: prepared.piece_len,
        flags: prepared.flags,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: &prepared.name,
        files: &files,
        content_type: class.content_type,
    };
    // Your classification rules change what's stored; an Is adult one
    // also makes the torrent meet the Adult-only core (§20.9).
    let (verdict, says) = engine.judge_classified(&item, Some(&facts), now);
    let fix = crate::enrich::rule_fix(says, &class);
    if fix.is_some() {
        prepared.enrichment = Some(crate::enrich::enrich_with_rule(&prepared.name, &files, fix));
    }
    Ok(Done {
        torrent: prepared,
        files,
        class,
        verdict,
        version: engine.version,
        fix,
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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_bencode::{OwnedValue, to_vec};
    use lodestar_cluster::{Claim, Claimed};
    use lodestar_peer_wire::Fetched;
    use lodestar_store::{Store, StoreConfig};

    use crate::gate::Origin;

    fn info(name: &str) -> Vec<u8> {
        to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"name".to_vec(), OwnedValue::from(name)),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 80])),
            ]
            .into_iter()
            .collect(),
        ))
    }

    /// The prepare stage's terminal losses reach the gate once each: the
    /// floor's hash list as filtered, unusable metainfo as invalid, and a
    /// usable one goes on to the writer uncounted.
    #[tokio::test]
    async fn prepare_losses_count_as_filtered_and_invalid_metainfo() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let leases = Arc::new(WorkerLeases::default());
        let gate = Arc::new(GateStats::new(0));
        let filter = Arc::new(crate::default_filter());
        let source = SourceId::new(1).unwrap();
        // A dictionary that hashes to its key but isn't metainfo.
        let raws = [info("Floor"), b"de".to_vec(), info("Fine")];
        let keys: Vec<InfoHash> = raws
            .iter()
            .map(|r| InfoHash(lodestar_peer_wire::testing::Seed::v1(r.clone()).info_hash))
            .collect();
        filter.update(|core| {
            core.cs
                .add_hash(keys[0].0, lodestar_filter::Origin::Imported)
        });
        for k in &keys {
            let claim = Claim {
                key: *k,
                source,
                strategy: 0,
                at: 0,
            };
            let Claimed::Granted(token) = coordinator.claim(WorkerId::LOCAL, claim, 0) else {
                panic!("granted")
            };
            leases.insert(*k, token);
        }
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let stage = PrepareStage {
            preparer: store.preparer(),
            filter,
            coordinator: coordinator.clone(),
            leases,
            stats: Arc::default(),
            gate: gate.clone(),
            batch_max: 8,
            batch_wait: Duration::from_millis(1),
        };
        let (in_tx, in_rx) = mpsc::channel(8);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        for (key, raw) in keys.iter().zip(raws) {
            in_tx
                .send(FetchedKey {
                    key: *key,
                    fetched: Fetched {
                        info: raw,
                        v2: false,
                        encrypted: false,
                        pex: Vec::new(),
                        client: None,
                    },
                    source,
                    sources: 1 << source.get(),
                    cohort: 0,
                    strategy: 0,
                    discovered_at: 0,
                    private_source: false,
                })
                .await
                .unwrap();
        }
        drop(in_tx);
        stage.run(in_rx, out_tx).await;
        let r = out_rx.recv().await.expect("the usable one");
        assert_eq!(r.key, keys[2]);
        assert_eq!(gate.excluded(Excluded::Filtered), 1);
        assert_eq!(gate.excluded(Excluded::InvalidMetainfo), 1);
        assert_eq!(gate.excluded(Excluded::FailedVerification), 0);
        assert_eq!(gate.new_verified(Origin::Network), 0);
    }

    /// The pool's terminal loss to data that didn't hash to the key counts
    /// once, under the current lease only. A protocol violation is the
    /// same funnel loss but not a failed verification, and other losses
    /// don't count.
    #[test]
    fn a_verification_loss_counts_once_under_its_lease() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let leases = Arc::new(WorkerLeases::default());
        let gate = Arc::new(GateStats::new(0));
        let wf = WorkerFunnel {
            counts: funnel.clone(),
            coordinator: coordinator.clone(),
            leases: leases.clone(),
            gate: gate.clone(),
        };
        let source = SourceId::new(1).unwrap();
        let cases = [
            (Loss::Invalid, Some(FailureKind::HashMismatch)),
            (Loss::Invalid, Some(FailureKind::Invalid)),
            (Loss::NoPeers, None),
            // Not a cause the pool passes, but the gate doesn't rely on it.
            (Loss::Evicted, Some(FailureKind::HashMismatch)),
        ];
        for (i, (loss, cause)) in cases.into_iter().enumerate() {
            let key = InfoHash([i as u8 + 1; 20]);
            let claim = Claim {
                key,
                source,
                strategy: 0,
                at: 0,
            };
            let Claimed::Granted(token) = coordinator.claim(WorkerId::LOCAL, claim, 0) else {
                panic!("granted")
            };
            leases.insert(key, token);
            wf.lost(&key, 0, source, loss, cause);
            // A second report has no lease left.
            wf.lost(&key, 0, source, loss, cause);
            // Only the hash mismatch, the first case, counts.
            assert_eq!(
                gate.excluded(Excluded::FailedVerification),
                1,
                "after {cause:?}"
            );
        }
        // Both invalid keys are still the funnel's `invalid` losses.
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::Invalid)), 2);
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::NoPeers)), 1);
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::Evicted)), 1);
    }
}
