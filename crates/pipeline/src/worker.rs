//! The worker side of the boundary (architecture §19.4): what a worker
//! does between finding a key and handing a finished torrent to the
//! coordinator. M3 runs one built-in worker ([`WorkerId::LOCAL`]) in this
//! process; remote workers run the same stages behind the network, built
//! without a store by [`crate::stages::WorkerStages`].
//!
//! - Intake claims each new key from the coordinator (see `intake`) and
//!   keeps the lease's token here.
//! - The fetch pool reports its losses through [`WorkerFunnel`], which
//!   hands them to the coordinator under that token ([`Reports`]).
//! - The prepare stage parses each fetched info dict, builds the row and
//!   blob, enriches it (release parse, classification) and runs the
//!   content filter (§20.4) into [`Results`], so the writer only commits.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{Classification, FileInfo};

use crate::source_adult::AdultSources;
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::{Engine, Facts, Handle, Item, Trail, Verdict};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::FailureKind;
use lodestar_sources::{FetchedKey, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{Prepared, Preparer, SourceId};
use tokio::time::{Instant, timeout_at};

use crate::gate::{Excluded, GateStats};
use crate::stages::{Inbox, Outbox, Reports};
use crate::unix_now;
use crate::writer::WriterStats;

/// The tokens of the leases this worker holds.
#[derive(Debug, Default)]
pub struct WorkerLeases(Mutex<HashMap<InfoHash, Token>>);

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

    /// Leases held.
    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The pool's funnel on a worker: progress stages count directly, the
/// harvest was counted at grant, and losses go to the coordinator under
/// the lease's token.
pub(crate) struct WorkerFunnel {
    pub counts: Arc<FunnelCounts>,
    pub reports: Arc<dyn Reports>,
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
                if self.reports.lost(key, token, loss)
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

    /// Intake granted the sighting a new lease (a reply would have been
    /// held), and the pool fetched the key lately but didn't store it.
    /// Without this nothing ends the lease, and the lease tables grow by
    /// one entry per such sighting (R-mem-triage).
    ///
    /// The lease ends as `Invalid` whatever the first fetch's outcome was.
    /// Prepare rejecting the dictionary is the common case, but a key also
    /// gets here after the writer refused it for a stopped source
    /// (`Evicted`), after a Drop rule at commit (`Blocked`, which puts it
    /// in neither the held set nor the blocklist), or through the writer's
    /// retry channel, which skips the have-filter for a key committed in
    /// the meantime. Insights' loss-reason share therefore overcounts
    /// `invalid` slightly; that's accepted, since the lease has to end and
    /// tracking the first loss isn't worth it. Each such sighting within
    /// the pool's recent window is its own harvest and loss, possibly more
    /// than once per key.
    fn duplicate(&self, key: &InfoHash, _cohort: i64, _source: SourceId) {
        if let Some(token) = self.leases.take(key) {
            self.reports.lost(key, token, Loss::Invalid);
        }
    }
}

/// A prepared torrent on its way to the coordinator (the `Results`
/// record, §19.4). Provenance beyond what the worker saw comes from the
/// coordinator's lease.
#[derive(Debug)]
pub struct Results {
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
    /// What the verdict's judgment found above the pack layer, for the
    /// writer's second judgment with SP-3 or SP-4 matching
    /// ([`lodestar_filter::Engine::judge_at_commit_reusing`]).
    pub trail: Trail,
    /// The classification rule applied to the stored enrichment (§20.9):
    /// Is adult or Not adult, where it differs from detection.
    pub fix: Option<bool>,
    /// Only private sources reported the key.
    pub private_source: bool,
    /// An always-adult source found it (X-source-adult): stored adult and
    /// judged as adult too, whatever the classification rules say.
    pub source_adult: bool,
}

pub(crate) struct PrepareStage {
    /// Who the results come from.
    pub worker: WorkerId,
    pub preparer: Preparer,
    pub filter: Arc<Handle>,
    pub reports: Arc<dyn Reports>,
    pub leases: Arc<WorkerLeases>,
    pub stats: Arc<WriterStats>,
    pub gate: Arc<GateStats>,
    /// The sources whose every find is adult.
    pub adult: AdultSources,
    pub batch_max: usize,
    pub batch_wait: Duration,
}

impl PrepareStage {
    /// Runs until the pool's output closes. Waiting on `out` passes the
    /// writer's (or the link's) backpressure on to the pool. An item from
    /// a byte-capped queue keeps its bytes until its result is sent.
    pub async fn run<I, O>(mut self, mut fetched: I, out: O)
    where
        I: Inbox,
        I::Item: Borrow<FetchedKey>,
        O: Outbox<Results>,
    {
        while let Some(first) = fetched.recv().await {
            let batch = collect(first, &mut fetched, self.batch_max, self.batch_wait).await;
            let preparer = self.preparer;
            let engine = self.filter.current();
            let adult = self.adult;
            let (preparer, done) = tokio::task::spawn_blocking(move || {
                let mut preparer = preparer;
                let done: Vec<_> = batch
                    .into_iter()
                    .map(|item| {
                        let fk: &FetchedKey = item.borrow();
                        let source_adult = adult.any(fk.sources);
                        let r = prepare_one(&mut preparer, &engine, fk, source_adult);
                        (item, r)
                    })
                    .collect();
                (preparer, done)
            })
            .await
            .expect("prepare task");
            self.preparer = preparer;
            for (item, result) in done {
                let fk: &FetchedKey = item.borrow();
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
                            worker: self.worker,
                            torrent: done.torrent,
                            sources: fk.sources,
                            metadata_at: unix_now(),
                            files: done.files,
                            class: done.class,
                            verdict: done.verdict,
                            version: done.version,
                            trail: done.trail,
                            fix: done.fix,
                            private_source: fk.private_source,
                            source_adult: done.source_adult,
                        };
                        let sent = out.send(r).await;
                        drop(item);
                        if sent.is_err() {
                            return;
                        }
                    }
                    Err(loss) => {
                        let (counter, excluded) = match loss {
                            Loss::Blocked => (&self.stats.blocked, Excluded::Filtered),
                            _ => (&self.stats.invalid, Excluded::InvalidMetainfo),
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        if self.reports.lost(&fk.key, token, loss) {
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
    trail: Trail,
    fix: Option<bool>,
    source_adult: bool,
}

/// Parses, prepares, enriches and judges one fetched info dict. A key in
/// the floor's hash list is refused before parsing; every other verdict
/// goes to the writer, which records it and re-checks it (§19.4).
/// `source_adult`: an always-adult source found it, so it's stored adult
/// and judged as adult too ([`crate::source_adult`]).
fn prepare_one(
    preparer: &mut Preparer,
    engine: &Engine,
    fk: &FetchedKey,
    source_adult: bool,
) -> Result<Done, Loss> {
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
    // also makes the torrent meet the Adult-only core (§20.9). So does an
    // always-adult source, which no Not adult rule undoes.
    let mut trail = Trail::default();
    let (verdict, fix) =
        crate::source_adult::judge_kept(engine, &item, &facts, source_adult, now, Some(&mut trail));
    if fix.is_some() {
        prepared.enrichment = Some(crate::enrich::enrich_with_rule(&prepared.name, &files, fix));
    }
    Ok(Done {
        torrent: prepared,
        files,
        class,
        verdict,
        version: engine.version,
        trail,
        fix,
        source_adult,
    })
}

/// Gathers more items for the same batch, briefly: at most `max`, for at
/// most `wait` after the first. Not cancel-safe: dropped while it waits,
/// the items gathered so far are lost.
pub(crate) async fn collect<I: Inbox>(
    first: I::Item,
    rx: &mut I,
    max: usize,
    wait: Duration,
) -> Vec<I::Item> {
    let mut batch = vec![first];
    let deadline = Instant::now() + wait;
    while batch.len() < max {
        match rx.try_recv() {
            Ok(x) => batch.push(x),
            Err(true) => break,
            Err(false) => match timeout_at(deadline, rx.recv()).await {
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
    use tokio::sync::mpsc;

    use crate::coordinator::Coordinator;
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
            worker: WorkerId::LOCAL,
            preparer: store.preparer(),
            filter,
            reports: coordinator.clone(),
            leases,
            stats: Arc::default(),
            gate: gate.clone(),
            adult: AdultSources::default(),
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

    /// A fetch an always-adult source found is stored adult, as an Is
    /// adult correction makes it, and says so to the writer; the same
    /// torrent from another source is detection's.
    #[test]
    fn a_fetch_an_always_adult_source_found_is_prepared_adult() {
        use lodestar_classify::{ContentType, Record};
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut preparer = store.preparer();
        let engine = crate::default_filter().current();
        let raw = info("Garden.Tour.2022.1080p.mkv");
        let key = InfoHash(lodestar_peer_wire::testing::Seed::v1(raw.clone()).info_hash);
        let source = SourceId::new(2).unwrap();
        let fk = FetchedKey {
            key,
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
        };
        let stored = |d: &Done| {
            let e = d.torrent.enrichment.as_ref().unwrap();
            let r = Record::decode(&e.data).unwrap();
            (ContentType::from_code(e.content_type), r.content_type)
        };
        let plain = prepare_one(&mut preparer, &engine, &fk, false).unwrap();
        assert_ne!(plain.class.content_type, ContentType::Adult);
        assert_ne!(stored(&plain).0, Some(ContentType::Adult));
        assert!(!plain.source_adult && plain.fix.is_none());
        let adult = prepare_one(&mut preparer, &engine, &fk, true).unwrap();
        assert_eq!(
            stored(&adult),
            (Some(ContentType::Adult), ContentType::Adult)
        );
        assert!(adult.source_adult);
        assert_eq!(adult.fix, Some(true));
        // Detection's own reading is kept for filtering beside it.
        assert_eq!(adult.class.content_type, plain.class.content_type);
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
            reports: coordinator.clone(),
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
