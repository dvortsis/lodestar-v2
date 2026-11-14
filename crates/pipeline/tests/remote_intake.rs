//! The coordinator's intake of remote workers' results and counters (WK-9;
//! architecture §19.3–§19.5, §18.7; plan Q7), on the real pipeline and
//! store. Each of WK-9's hard requirements has a test here or beside the
//! code it changed (`docs/decisions/2026-11-07-WK-9.md` lists them).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{ContentType, ENRICHMENT_VERSION, Record};
use lodestar_cluster::proto::{HintItem, Payload};
use lodestar_cluster::{
    BatchAnswer, Claim, Claimed, InboundSink, LeaseAuthority, StoredKeys, Token, WorkerId,
};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::metric;
use lodestar_pipeline::remote::{Clocks, RemoteIntake, SinkParts, Standing};
use lodestar_pipeline::wire::filter_snapshot::{SealedRecord, WireKey, WireKeys};
use lodestar_pipeline::wire::{ResultRecord, ResultsBatch};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{PoolConfig, Stage};
use lodestar_store::{Enriched, Prepared, QueuedKey, SourceId, Store, StoreConfig};

const WAIT: Duration = Duration::from_secs(15);
const W: WorkerId = WorkerId(7);
const GIB: u64 = 1 << 30;

/// Polls with a deadline (standards §2: no fixed sleeps).
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn src(n: u8) -> SourceId {
    SourceId::new(n).unwrap()
}

fn bytes(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// A single-file info dict (its name is the file's).
fn info(name: &str, size: u64) -> Vec<u8> {
    format!(
        "d6:lengthi{size}e4:name{}12:piece lengthi1048576e6:pieces20:{}e",
        bytes(name),
        "\0".repeat(20)
    )
    .into_bytes()
}

/// A prepared torrent as a worker's `Preparer` makes it, with the
/// worker's own enrichment (honest, unless the test changes it).
fn prepared(store: &Store, raw: &[u8]) -> Prepared {
    let info = Info::parse(raw).unwrap();
    let mut p = store.preparer().prepare(&info, Some(raw)).unwrap();
    let files = lodestar_classify::file_infos(&info.files, info.path_encoding);
    p.enrichment = Some(lodestar_pipeline::enrich::enrich_infos(&p.name, &files).0);
    p
}

fn record(torrent: Prepared, token: Token) -> ResultRecord {
    ResultRecord {
        key: torrent.info_hash,
        token,
        torrent,
        sources: 0,
        metadata_at: now(),
        action: None,
        rule_version: 0,
        fix: None,
        private_source: false,
        source_adult: false,
    }
}

/// A batch from worker 7, every record sealed as a worker seals it.
fn payload(batch: u64, records: Vec<ResultRecord>) -> Payload {
    payload_as(W, batch, records)
}

/// The same from worker `w`: each record is sealed for its id.
fn payload_as(w: WorkerId, batch: u64, records: Vec<ResultRecord>) -> Payload {
    Payload {
        batch,
        bytes: sealed_bytes(w, batch, records),
    }
}

/// The wire key the tests' workers seal with.
fn wire_key() -> WireKey {
    WireKey::new(1, [7; 32])
}

/// The coordinator's keys: the tests' one.
fn test_keys() -> Arc<WireKeys> {
    let keys = Arc::new(WireKeys::default());
    keys.set(vec![wire_key()]);
    keys
}

/// A `Results` payload from `w` with every record sealed (WK-8: every
/// remote record is).
fn sealed_bytes(w: WorkerId, batch: u64, records: Vec<ResultRecord>) -> Vec<u8> {
    sealed_with(&wire_key(), w, batch, records)
}

/// The same, sealed under `key`.
fn sealed_with(key: &WireKey, w: WorkerId, batch: u64, records: Vec<ResultRecord>) -> Vec<u8> {
    let sealed: Vec<SealedRecord> = records
        .iter()
        .map(|r| SealedRecord::seal(r, key, w).unwrap())
        .collect();
    ResultsBatch {
        batch,
        records: Vec::new(),
    }
    .encode_with(&sealed)
    .unwrap()
}

/// A worker's clock offset, fixed.
struct Offset(i64);
impl Clocks for Offset {
    fn offset_ms(&self, _: WorkerId) -> i64 {
        self.0
    }
}

/// WK-13's quarantine and purge, faked.
#[derive(Default)]
struct Fake {
    refused: Mutex<Vec<WorkerId>>,
    purged: Mutex<HashMap<WorkerId, i64>>,
}
impl Standing for Fake {
    fn refuses_late(&self, w: WorkerId) -> bool {
        self.refused.lock().unwrap().contains(&w)
    }
    fn last_purge(&self, w: WorkerId) -> Option<i64> {
        self.purged.lock().unwrap().get(&w).copied()
    }
}

/// The store check, on a reader of its own.
struct Reader(Mutex<Store>);
impl StoredKeys for Reader {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        let r = self.0.lock().unwrap();
        keys.iter().map(|k| r.find(k).unwrap().is_some()).collect()
    }
}

fn reader(db: &Path) -> Store {
    Store::open_reader(db, StoreConfig::default()).unwrap()
}

fn config() -> PipelineConfig {
    PipelineConfig {
        remote_workers: true,
        pool: PoolConfig {
            lookup_wait: Duration::from_secs(600),
            ..PoolConfig::default()
        },
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
        ..PipelineConfig::default()
    }
}

/// A sink on `pipeline`'s intake, as `serve` attaches one.
fn sink_for(
    pipeline: &Pipeline,
    db: &Path,
    standing: Arc<Fake>,
    offset_ms: i64,
) -> Arc<lodestar_pipeline::remote::RemoteSink> {
    pipeline.monitor().remote().sink(SinkParts {
        preparer: Mutex::new(reader(db).preparer()),
        standing,
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(reader(db)))),
        clocks: Arc::new(Offset(offset_ms)),
        hints_allow_private: false,
    })
}

/// A pipeline with no plugins, its store's path, a worker-side store for
/// preparing records, and a sink for worker 7.
struct Rig {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
    pipeline: Pipeline,
    worker_store: Store,
    standing: Arc<Fake>,
    sink: Arc<lodestar_pipeline::remote::RemoteSink>,
}

impl Rig {
    async fn new(queue: &[QueuedKey], offset_ms: i64) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        store.save_fetch_queue(queue).unwrap();
        let pipeline = Pipeline::start(config(), store, vec![]).await.unwrap();
        let standing = Arc::new(Fake::default());
        let sink = sink_for(&pipeline, &db, standing.clone(), offset_ms);
        let worker_store =
            Store::open(dir.path().join("worker.db"), StoreConfig::default()).unwrap();
        Rig {
            _dir: dir,
            db,
            pipeline,
            worker_store,
            standing,
            sink,
        }
    }

    fn authority(&self) -> Arc<LeaseAuthority> {
        self.pipeline.monitor().lease_authority().clone()
    }

    fn remote(&self) -> Arc<RemoteIntake> {
        self.pipeline.monitor().remote().clone()
    }

    /// Leases `key` to worker `w` as a remote claim would, from source `s`.
    fn lease(&self, w: WorkerId, key: InfoHash, s: u8) -> Token {
        let c = Claim {
            key,
            source: src(s),
            strategy: 3,
            at: now() - 30,
        };
        match self.authority().claim_batch(w, &[c], now(), None, None)[0] {
            BatchAnswer::Claimed {
                claimed: Claimed::Granted(t),
                ..
            } => t,
            other => panic!("{other:?}"),
        }
    }

    fn stored(&self, key: &InfoHash) -> Option<i64> {
        reader(&self.db).find(key).unwrap()
    }

    async fn wait_stored(&self, key: &InfoHash) -> i64 {
        until("the record is stored", || self.stored(key).is_some()).await;
        self.stored(key).unwrap()
    }

    /// Waits until the intake has dealt with `n` records.
    async fn wait_records(&self, n: u64) {
        let stats = self.remote().stats.clone();
        until("the intake read the records", || {
            stats.records.load(Ordering::Acquire) >= n
        })
        .await;
    }
}

fn stat(
    r: &RemoteIntake,
    f: impl Fn(&lodestar_pipeline::remote::RemoteStats) -> &std::sync::atomic::AtomicU64,
) -> u64 {
    f(&r.stats).load(Ordering::Relaxed)
}

/// Requirement 1 (content safety): a worker that labels adult content
/// non-adult, with a record that passes the codec (this build's version,
/// hot columns matching), has the coordinator's own verdict stored, in the
/// record bytes search builds its `type:adult` term from (`Enriched::data`,
/// decoded by `doc_enrichment`) and in the hot column. A record of another
/// version is never kept as sent either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_cannot_store_adult_content_as_non_adult() {
    let rig = Rig::new(&[], 0).await;
    let adult = "Brazzers.24.01.15.Some.Title.1080p.mp4";
    let mut lying = prepared(&rig.worker_store, &info(adult, GIB));
    // The worker's record: what an honest worker computes for a movie.
    let movie = lodestar_pipeline::enrich::enrich_infos(
        "Lantern.Bay.2019.1080p.WEB-DL",
        &[lodestar_classify::FileInfo::new(
            vec!["Lantern.Bay.2019.1080p.WEB-DL.mkv".into()],
            GIB,
        )],
    )
    .0;
    assert_eq!(movie.content_type, ContentType::Movie.code());
    lying.enrichment = Some(movie);
    // Another version's record: the codec checks only its hot columns.
    let mut other = prepared(
        &rig.worker_store,
        &info("Brazzers.24.02.01.Other.Title.720p.mp4", GIB),
    );
    other.enrichment = Some(Enriched {
        version: ENRICHMENT_VERSION - 1,
        content_type: ContentType::Movie.code(),
        junk_score: 0,
        comp: vec![],
        data: b"not a record".to_vec(),
    });
    let (k1, k2) = (lying.info_hash, other.info_hash);
    let (t1, t2) = (rig.lease(W, k1, 2), rig.lease(W, k2, 2));
    assert!(
        rig.sink
            .results(W, payload(1, vec![record(lying, t1), record(other, t2)]))
    );
    for key in [k1, k2] {
        let id = rig.wait_stored(&key).await;
        let e = reader(&rig.db).enrichment(id).unwrap().unwrap();
        assert_eq!(e.version, ENRICHMENT_VERSION, "rebuilt, not kept");
        let r = Record::decode(&e.data).expect("the stored record decodes");
        assert_eq!(r.content_type, ContentType::Adult, "search's adult term");
        assert!(r.adult_kind.is_some());
        assert_eq!(e.content_type, ContentType::Adult.code(), "the hot column");
    }
    rig.pipeline.shutdown().await.unwrap();
}

/// Requirement 3: sources and `private_source` come from the lease,
/// `metadata_at` is offset-corrected and kept between discovery and now,
/// and `size` and `file_count` must match the rebuilt file list (a record
/// that doesn't is refused and its lease ends once, as invalid).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provenance_comes_from_the_lease_and_counts_from_the_blob() {
    // The worker's clock runs an hour ahead.
    let rig = Rig::new(&[], 3_600_000).await;
    let good = prepared(&rig.worker_store, &info("Sintel.2010.1080p", GIB));
    let mut size = prepared(&rig.worker_store, &info("Sintel.2010.720p", GIB));
    size.size += 1;
    let mut count = prepared(&rig.worker_store, &info("Sintel.2010.480p", GIB));
    count.file_count = 3;
    let keys = [good.info_hash, size.info_hash, count.info_hash];
    let tokens: Vec<Token> = keys.iter().map(|k| rig.lease(W, *k, 4)).collect();
    let fetched = now();
    let mut records: Vec<ResultRecord> = [good, size, count]
        .into_iter()
        .zip(&tokens)
        .map(|(t, tok)| record(t, *tok))
        .collect();
    for r in &mut records {
        // Every source bit, private, and the worker's (fast) clock.
        r.sources = u64::MAX >> 1;
        r.private_source = true;
        r.metadata_at = fetched + 3_600;
    }
    assert!(rig.sink.results(W, payload(2, records)));
    let id = rig.wait_stored(&keys[0]).await;
    let row = reader(&rig.db).torrent(id).unwrap().unwrap();
    assert_eq!(
        row.sources,
        1 << 4,
        "the lease's source, not the worker's bits"
    );
    assert_eq!(row.flags & lodestar_store::flags::PRIVATE_SOURCE, 0);
    // Its BEP 27 bit is the worker's word (cleared here): every privacy
    // gate treats the row as private until it's re-verified (WK-9, fail
    // closed). The display bit stays as sent.
    assert_ne!(row.flags & lodestar_store::flags::REMOTE_UNVERIFIED, 0);
    assert_eq!(row.flags & lodestar_store::flags::PRIVATE, 0);
    assert!(
        (fetched..=now()).contains(&row.metadata_at),
        "corrected by the offset: {} vs {fetched}",
        row.metadata_at
    );
    let authority = rig.authority();
    let remote = rig.remote();
    until("the bad records' leases end", || {
        !authority.in_flight(&keys[1]) && !authority.in_flight(&keys[2])
    })
    .await;
    assert_eq!(stat(&remote, |s| &s.refused), 2);
    assert!(rig.stored(&keys[1]).is_none() && rig.stored(&keys[2]).is_none());
    // Counted once each, as invalid: a report under the old token is stale.
    assert!(authority.report(W, &keys[1], tokens[1], Some(6)).is_none());
    rig.pipeline.shutdown().await.unwrap();
}

/// Fencing: a current-epoch token that isn't the sender's current lease is
/// ignored, results and outcomes alike, and nothing is counted twice.
/// Requirement 4: the commit is the holder's; another worker that names the
/// holder's (predictable) token commits nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_and_guessed_tokens_commit_nothing() {
    let rig = Rig::new(&[], 0).await;
    let a = prepared(&rig.worker_store, &info("Tears.of.Steel.2012", GIB));
    let b = prepared(&rig.worker_store, &info("Elephants.Dream.2006", GIB));
    let (ka, kb) = (a.info_hash, b.info_hash);
    let ta = rig.lease(W, ka, 1);
    let tb = rig.lease(W, kb, 1);
    // Worker 8 names worker 7's tokens, in a record it sealed itself.
    let other = WorkerId(8);
    assert!(
        rig.sink
            .results(other, payload_as(other, 3, vec![record(a.clone(), ta)]))
    );
    // Worker 7's lease on b moves on (expired, granted again elsewhere).
    let authority = rig.authority();
    assert_eq!(authority.evict(W), 2);
    let tb2 = rig.lease(WorkerId(9), kb, 1);
    assert_ne!(tb, tb2);
    assert!(rig.sink.results(W, payload(4, vec![record(b.clone(), tb)])));
    rig.wait_records(2).await;
    let remote = rig.remote();
    assert_eq!(stat(&remote, |s| &s.stale), 2);
    // A late outcome under the old token is ignored too.
    assert!(authority.report(W, &kb, tb, Some(1)).is_none());
    assert_eq!(authority.get(&kb).unwrap().token, tb2);
    // The real holder's result is stored, once, however often it's sent.
    for batch in [5, 6] {
        assert!(rig.sink.results(
            WorkerId(9),
            payload_as(WorkerId(9), batch, vec![record(b.clone(), tb2)])
        ));
    }
    rig.wait_stored(&kb).await;
    rig.wait_records(4).await;
    let monitor = rig.pipeline.monitor();
    until("one stored", || {
        monitor.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    assert!(rig.stored(&ka).is_none());
    // A resend read before the first commit ended the lease reaches the
    // writer too, which stores the key once and ends the lease once.
    assert!(stat(&remote, |s| &s.stale) >= 2);
    let id = rig.stored(&kb).unwrap();
    assert_eq!(
        reader(&rig.db).torrent(id).unwrap().unwrap().sources,
        1 << 1
    );
    assert!(!authority.in_flight(&kb));
    let granted = rig
        .pipeline
        .monitor()
        .lease_stats()
        .granted
        .load(Ordering::Relaxed);
    assert_eq!(granted, 2, "a and b harvested once each");
    rig.pipeline.shutdown().await.unwrap();
}

/// Requirement 7: every batch that was read is answered `accepted` (so the
/// worker calls `committed` and drops its held entries), including one
/// that didn't decode and one whose records were all stale; only a full
/// queue answers "send it again later".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_batch_read_is_accepted() {
    let rig = Rig::new(&[], 0).await;
    let junk = Payload {
        batch: 9,
        bytes: vec![1, 2, 3],
    };
    assert!(rig.sink.results(W, junk));
    let t = prepared(&rig.worker_store, &info("Big.Buck.Bunny.2008", GIB));
    assert!(rig.sink.results(W, payload(10, vec![record(t, Token(1))])));
    let remote = rig.remote();
    until("read", || stat(&remote, |s| &s.undecodable) == 1).await;
    rig.wait_records(1).await;
    assert_eq!(stat(&remote, |s| &s.stale), 1);
    rig.pipeline.shutdown().await.unwrap();
}

/// Plan Q7: late results (a token from an earlier epoch).
/// - refused outright from a quarantined, revoked or purged worker, and
///   when fetched before the worker's last purge;
/// - stale while the key is in flight (the built-in worker holds it after
///   the restart) or stored;
/// - otherwise committed under the coordinator's own record of the key
///   (cohort, source), never harvested again, with a `LATE_RESULTS`
///   correction so harvested = Σ terminal − late + in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_results_follow_q7() {
    let dir_store = tempfile::tempdir().unwrap();
    let worker_store = Store::open(dir_store.path().join("w.db"), StoreConfig::default()).unwrap();
    let names = [
        "Cosmos.Laundromat.2015.mkv",
        "Spring.2019.mkv",
        "Agent.327.2017.mkv",
        "Hero.2018.mkv",
        "Coffee.Run.2020.mkv",
    ];
    let torrents: Vec<Prepared> = names
        .iter()
        .map(|n| prepared(&worker_store, &info(n, GIB)))
        .collect();
    let cohort = now().div_euclid(3600) - 2;
    let queue: Vec<QueuedKey> = torrents
        .iter()
        .map(|t| QueuedKey {
            info_hash: t.info_hash,
            source: src(5),
            cohort,
            strategy: 3,
            priority: 0,
            attempts: 0,
            private_source: false,
        })
        .collect();
    let rig = Rig::new(&queue, 0).await;
    let authority = rig.authority();
    let remote = rig.remote();
    let monitor = rig.pipeline.monitor();
    let granted = || monitor.lease_stats().granted.load(Ordering::Relaxed);
    // Restored locally at the start: in flight, not harvested.
    for t in &torrents {
        assert_eq!(authority.get(&t.info_hash).unwrap().holder, WorkerId::LOCAL);
    }
    assert_eq!(granted(), 0);
    let earlier = Token(((now() as u64 - 3_600) << 32) | 77);
    let late = |t: &Prepared| record(t.clone(), earlier);

    // In flight: stale.
    assert!(rig.sink.results(W, payload(20, vec![late(&torrents[0])])));
    rig.wait_records(1).await;
    assert_eq!(stat(&remote, |s| &s.stale), 1);

    // The built-in worker loses every key ("lost" is their terminal row).
    for t in &torrents {
        let l = authority.get(&t.info_hash).unwrap();
        assert!(authority.lost(&t.info_hash, l.token, 1).is_some());
    }
    // Found again since and leased to worker 9 (a new harvest in this run):
    // key 0 waits for a re-grant after worker 9 is revoked, and key 4 is
    // stored by worker 9. A late result for either is stale.
    let w9 = WorkerId(9);
    let t0 = rig.lease(w9, torrents[0].info_hash, 5);
    let t4 = rig.lease(w9, torrents[4].info_hash, 5);
    let _ = t0;
    assert!(rig.sink.results(
        w9,
        payload_as(w9, 30, vec![record(torrents[4].clone(), t4)])
    ));
    rig.wait_stored(&torrents[4].info_hash).await;
    rig.wait_records(2).await;
    until("worker 9's commit ends its lease", || {
        !authority.in_flight(&torrents[4].info_hash)
    })
    .await;
    assert_eq!(authority.evict(w9), 1);
    assert!(authority.in_flight(&torrents[0].info_hash), "waiting");
    assert!(
        rig.sink
            .results(W, payload(31, vec![late(&torrents[0]), late(&torrents[4])]))
    );
    rig.wait_records(4).await;
    assert_eq!(stat(&remote, |s| &s.stale), 3);
    assert_eq!(stat(&remote, |s| &s.late), 0);
    assert_eq!(granted(), 2);
    // Quarantined (or revoked, or purged): refused outright.
    rig.standing.refused.lock().unwrap().push(W);
    assert!(rig.sink.results(W, payload(21, vec![late(&torrents[1])])));
    rig.wait_records(5).await;
    assert_eq!(stat(&remote, |s| &s.late_refused), 1);
    rig.standing.refused.lock().unwrap().clear();
    // Fetched before the worker's last purge: refused, late or not.
    rig.standing.purged.lock().unwrap().insert(W, now() + 10);
    assert!(rig.sink.results(W, payload(22, vec![late(&torrents[1])])));
    rig.wait_records(6).await;
    assert_eq!(stat(&remote, |s| &s.purged), 1);
    rig.standing.purged.lock().unwrap().clear();

    // Neither stored nor in flight: committed once, from our record.
    assert!(
        rig.sink
            .results(W, payload(23, vec![late(&torrents[2]), late(&torrents[3])]))
    );
    let id = rig.wait_stored(&torrents[2].info_hash).await;
    rig.wait_stored(&torrents[3].info_hash).await;
    let row = reader(&rig.db).torrent(id).unwrap().unwrap();
    assert_eq!(row.sources, 1 << 5, "the coordinator's record");
    assert_eq!(row.discovered_at.div_euclid(3600), cohort);
    assert_eq!(stat(&remote, |s| &s.late), 2);
    // Replayed again: stored now, so stale; nothing counted twice.
    assert!(rig.sink.results(W, payload(23, vec![late(&torrents[2])])));
    rig.wait_records(9).await;
    assert_eq!(stat(&remote, |s| &s.late), 2);
    assert_eq!(granted(), 2, "a late result is never harvested again");
    until("the late results' leases ended", || {
        !authority.in_flight(&torrents[2].info_hash) && !authority.in_flight(&torrents[3].info_hash)
    })
    .await;
    assert_eq!(authority.len(), 0);
    assert_eq!(authority.pending(), 1, "key 0 still waits");

    // The funnel: five keys harvested in the earlier run and two in this
    // one; five lost and three stored terminal rows, two corrections, and
    // one key in flight: 5 + 2 = 5 + 3 − 2 + 1.
    let store = rig.pipeline.shutdown().await.unwrap();
    let late_rows: i64 = store
        .rollup(metric::LATE_RESULTS, cohort, cohort + 1)
        .unwrap()
        .iter()
        .map(|c| c.value)
        .sum();
    assert_eq!(late_rows, 2);
    // Worker 9's leases are in this hour's cohort.
    let funnel = store.cohorts(cohort, now().div_euclid(3600) + 1).unwrap();
    let count = |stage: Stage| -> i64 {
        funnel
            .iter()
            .filter(|c| c.stage == stage as u8)
            .map(|c| c.count)
            .sum()
    };
    assert_eq!(count(Stage::Harvested), 2, "only worker 9's");
    assert_eq!((count(Stage::Lost), count(Stage::Stored)), (5, 3));
    assert_eq!(
        5 + count(Stage::Harvested),
        count(Stage::Lost) + count(Stage::Stored) - late_rows + 1
    );
}

/// Requirement 5 and §18.7: hints for a key the built-in worker holds pass
/// the network policy, and none of them reach the store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hints_are_filtered_and_never_stored() {
    let rig = Rig::new(&[], 0).await;
    let key = InfoHash([4; 20]);
    let authority = rig.authority();
    let Claimed::Granted(token) = authority.claim(
        WorkerId::LOCAL,
        Claim {
            key,
            source: src(1),
            strategy: 0,
            at: now(),
        },
        now(),
    ) else {
        panic!()
    };
    let public: std::net::SocketAddr = "203.0.113.77:6881".parse().unwrap();
    // A public address the policy passes. Nothing dials it: the key isn't
    // in the built-in worker's pool, so the reply has no entry to join.
    let real: std::net::SocketAddr = "93.184.216.34:51413".parse().unwrap();
    rig.sink.local_hints(vec![HintItem {
        key: key.0,
        token: token.0,
        hints: vec![
            "127.0.0.1:6881".parse().unwrap(),
            "10.1.2.3:6881".parse().unwrap(),
            // The documentation address WK-0's test relayed: now dropped.
            "192.0.2.1:6881".parse().unwrap(),
            public,
            real,
        ],
    }]);
    let remote = rig.remote();
    // Loopback, private and documentation addresses are dropped.
    assert_eq!(stat(&remote, |s| &s.hints_dropped), 4);
    assert_eq!(stat(&remote, |s| &s.hints), 1);
    let dir = rig.db.parent().unwrap().to_path_buf();
    rig.pipeline.shutdown().await.unwrap();
    // Nothing on disk holds the address, in text or as bytes.
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let data = std::fs::read(&path).unwrap();
            for needle in [b"93.184.216.34".as_slice(), &[93, 184, 216, 34]] {
                assert!(
                    !data.windows(needle.len()).any(|w| w == needle),
                    "{} holds a hint",
                    path.display()
                );
            }
        }
    }
}

/// A multi-file info dict of `files` files, each with one path segment
/// `path_len` bytes long.
/// An info dictionary of about `files * path_len` bytes: every file in one
/// folder `path_len` bytes long, which the dictionary spells out in each
/// file's path and the batch's path budget counts for each, while the
/// core reads it once, so the torrent is judged whole.
fn big_info(name: &str, files: usize, path_len: usize) -> Vec<u8> {
    let folder = "a".repeat(path_len.saturating_sub(6).max(1));
    let mut out = b"d5:filesl".to_vec();
    for f in 0..files {
        let seg = format!("{f:06}");
        out.extend(
            format!(
                "d6:lengthi1048576e4:pathl{}:{folder}{}:{seg}ee",
                folder.len(),
                seg.len()
            )
            .into_bytes(),
        );
    }
    out.extend(
        format!(
            "e4:name{}12:piece lengthi1048576e6:pieces{}:{}e",
            bytes(name),
            files * 20,
            "\0".repeat(files * 20)
        )
        .into_bytes(),
    );
    out
}

/// A blob that names a dictionary the coordinator doesn't have, or that
/// decompresses past its cap (a zstd bomb), is refused alone: its lease
/// ends once as invalid, and the rest of the batch is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_blob_is_refused_alone() {
    let rig = Rig::new(&[], 0).await;
    let good = prepared(&rig.worker_store, &info("Good.Record.2021.mkv", GIB));
    let mut unknown = prepared(&rig.worker_store, &info("Unknown.Dict.2021.mkv", GIB));
    unknown.files.dict_id = Some(999);
    unknown.files.codec = lodestar_store::Codec::Zstd;
    // 1 MiB of zeros in a few hundred bytes, against a cap of the row's
    // `info_size` (a hundred bytes) plus 4 KiB.
    let mut bomb = prepared(&rig.worker_store, &info("Bomb.2021.mkv", GIB));
    bomb.files = lodestar_store::BlobCodec::new(19)
        .compress(&vec![0u8; 1 << 20])
        .unwrap();
    assert!(bomb.files.data.len() < 1 << 10 && bomb.info_size < 1 << 10);
    let keys = [good.info_hash, unknown.info_hash, bomb.info_hash];
    let tokens: Vec<Token> = keys.iter().map(|k| rig.lease(W, *k, 1)).collect();
    let records = [good, unknown, bomb]
        .into_iter()
        .zip(&tokens)
        .map(|(t, tok)| record(t, *tok))
        .collect();
    assert!(rig.sink.results(W, payload(40, records)));
    rig.wait_stored(&keys[0]).await;
    let authority = rig.authority();
    until("the refused records' leases end", || {
        !authority.in_flight(&keys[1]) && !authority.in_flight(&keys[2])
    })
    .await;
    assert_eq!(stat(&rig.remote(), |s| &s.refused), 2);
    // Over its own cap, with the batch's bytes to spare: not the budget.
    assert_eq!(stat(&rig.remote(), |s| &s.over_budget), 0);
    assert!(rig.stored(&keys[1]).is_none() && rig.stored(&keys[2]).is_none());
    rig.pipeline.shutdown().await.unwrap();
}

/// A batch's blobs decompress to at most `BATCH_BLOBS` (64 MiB) in total:
/// four file trees of about 15 MB fit, the fifth is refused alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_decompresses_at_most_its_budget() {
    let rig = Rig::new(&[], 0).await;
    let mut records = Vec::new();
    let mut keys = Vec::new();
    for n in 0..5 {
        let raw = big_info(&format!("Dataset.{n}"), 3_840, 4_000);
        let t = prepared(&rig.worker_store, &raw);
        assert!(t.info_size > 15_000_000 && t.info_size < 16 << 20);
        keys.push(t.info_hash);
        let token = rig.lease(W, t.info_hash, 1);
        records.push(record(t, token));
    }
    let remote = rig.remote();
    assert!(rig.sink.results(W, payload(41, records)));
    rig.wait_records(5).await;
    for k in &keys[..4] {
        rig.wait_stored(k).await;
    }
    assert_eq!(stat(&remote, |s| &s.refused), 1);
    // Over the cap the batch's bytes left set (WK-9b): the budget's.
    assert_eq!(stat(&remote, |s| &s.over_budget), 1);
    assert!(rig.stored(&keys[4]).is_none());
    assert_eq!(lodestar_pipeline::remote::BATCH_BLOBS, 64 << 20);
    rig.pipeline.shutdown().await.unwrap();
}

/// Plan Q7's map of the start's fetch queue is kept only when workers are
/// allowed at the start: a single machine holds nothing extra. (Its 25 h
/// drop is `remote.rs`'s `the_start_queue_is_kept_only_while_a_spool_can_hold_a_result`,
/// on an injected clock.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_start_queue_is_kept_only_with_workers_allowed() {
    let queue: Vec<QueuedKey> = (1..=3u8)
        .map(|n| QueuedKey {
            info_hash: InfoHash([n; 20]),
            source: src(1),
            cohort: now().div_euclid(3600),
            strategy: 0,
            priority: 0,
            attempts: 0,
            private_source: false,
        })
        .collect();
    for allowed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("l.db"), StoreConfig::default()).unwrap();
        store.save_fetch_queue(&queue).unwrap();
        let config = PipelineConfig {
            remote_workers: allowed,
            ..config()
        };
        let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
        let kept = pipeline.monitor().remote().late_records();
        assert_eq!(kept, if allowed { 3 } else { 0 }, "allowed: {allowed}");
        pipeline.shutdown().await.unwrap();
    }
}

/// A file tree nested deeper than metainfo allows (cheap as a blob, costly
/// to decode: each path copies its parent's) is refused alone, before any
/// path is built (`filetree::shape`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deep_file_tree_is_refused_before_it_is_built() {
    let rig = Rig::new(&[], 0).await;
    let mut deep = prepared(&rig.worker_store, &info("Deep.2022.mkv", GIB));
    let file = lodestar_metainfo::File {
        path: (0..300).map(|i| format!("d{i}").into_bytes()).collect(),
        size: GIB,
        attrs: lodestar_metainfo::FileAttrs::empty(),
        symlink_target: None,
        pieces_root: None,
        hashes: lodestar_metainfo::FileHashes::default(),
    };
    let raw =
        lodestar_store::filetree::encode(&[file], lodestar_metainfo::TextEncoding::UTF8).unwrap();
    deep.info_size = raw.len() as u64;
    deep.files = lodestar_store::Compressed {
        codec: lodestar_store::Codec::Raw,
        dict_id: None,
        data: raw,
    };
    let key = deep.info_hash;
    let token = rig.lease(W, key, 1);
    assert!(rig.sink.results(W, payload(50, vec![record(deep, token)])));
    let authority = rig.authority();
    until("its lease ends", || !authority.in_flight(&key)).await;
    assert_eq!(stat(&rig.remote(), |s| &s.refused), 1);
    assert!(rig.stored(&key).is_none());
    rig.pipeline.shutdown().await.unwrap();
}

/// A current-epoch record fetched before its worker's last purge is
/// refused, and its lease ends once, so the key isn't stranded after the
/// worker forgets it (the batch is still answered `accepted`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purged_record_ends_its_lease() {
    let rig = Rig::new(&[], 0).await;
    let t = prepared(&rig.worker_store, &info("Purged.2023.mkv", GIB));
    let key = t.info_hash;
    let token = rig.lease(W, key, 1);
    rig.standing.purged.lock().unwrap().insert(W, now() + 10);
    // The worker claims a fetch far after the purge: the cutoff is the
    // coordinator's own time for the lease, so it doesn't help.
    let mut r = record(t, token);
    r.metadata_at = now() + 1_000_000;
    assert!(rig.sink.results(W, payload(51, vec![r])));
    let authority = rig.authority();
    until("its lease ends", || !authority.in_flight(&key)).await;
    assert_eq!(stat(&rig.remote(), |s| &s.purged), 1);
    assert!(rig.stored(&key).is_none());
    assert!(authority.report(W, &key, token, Some(6)).is_none(), "once");
    rig.pipeline.shutdown().await.unwrap();
}

/// A worker's records with a tree of `files` files, each `depth` deep
/// (sharing one directory chain), sized to pass the count checks.
fn wide(store: &Store, name: &str, files: usize, depth: usize) -> Prepared {
    let mut t = prepared(store, &info(name, GIB));
    let chain: Vec<Vec<u8>> = (1..depth).map(|i| format!("d{i}").into_bytes()).collect();
    let list: Vec<lodestar_metainfo::File> = (0..files)
        .map(|i| {
            let mut path = chain.clone();
            path.push(format!("{i:06}").into_bytes());
            lodestar_metainfo::File {
                path,
                size: 1,
                attrs: lodestar_metainfo::FileAttrs::empty(),
                symlink_target: None,
                pieces_root: None,
                hashes: lodestar_metainfo::FileHashes::default(),
            }
        })
        .collect();
    let raw =
        lodestar_store::filetree::encode(&list, lodestar_metainfo::TextEncoding::UTF8).unwrap();
    // What the info dict would weigh: it spells out every full path,
    // plus about 30 bytes of bencode per file.
    let spelled: usize = list
        .iter()
        .map(|f| f.path.iter().map(|p| p.len() + 3).sum::<usize>() + 30)
        .sum();
    t.info_size = spelled as u64;
    t.files = lodestar_store::BlobCodec::new(19).compress(&raw).unwrap();
    t.size = files as u64;
    t.file_count = files as u64;
    // A multi-file torrent's row, as its own dict derives it.
    t.piece_count = (files as u64).div_ceil(t.piece_len);
    t.flags &= !lodestar_store::flags::SINGLE_FILE;
    t
}

/// The path-component budget (`BATCH_COMPONENTS`) holds per record and per
/// batch: one tree over it is refused alone; of two that each fit, the
/// second, which would pass the batch's total, is refused. Taken directly,
/// with no writer, on the intake's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_components_are_budgeted_per_record_and_per_batch() {
    use lodestar_pipeline::remote::{BATCH_COMPONENTS, NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("c.db"), StoreConfig::default()).unwrap();
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    };
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let leased = |t: Prepared| {
        let c = Claim {
            key: t.info_hash,
            source: src(1),
            strategy: 0,
            at: now(),
        };
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(token),
            ..
        } = authority.claim_batch(W, &[c], now(), None, None)[0]
        else {
            panic!()
        };
        record(t, token)
    };
    // 200,000 files, 11 deep: 2.2M components, over the budget alone.
    let over = wide(&ws, "Over.2024.mkv", 200_000, 11);
    // 100,000 files, 12 deep: 1.2M each; two pass the batch's 2M.
    let a = wide(&ws, "Half.A.2024.mkv", 100_000, 12);
    let b = wide(&ws, "Half.B.2024.mkv", 100_000, 12);
    const { assert!(1_200_000 < BATCH_COMPONENTS && 2_400_000 > BATCH_COMPONENTS) };
    let one = sealed_bytes(W, 60, vec![leased(over)]);
    let taken = intake.take(W, &one, &parts, now()).unwrap();
    assert!(taken.results.is_empty(), "over the budget alone");
    let two = sealed_bytes(W, 61, vec![leased(a.clone()), leased(b.clone())]);
    let taken = intake.take(W, &two, &parts, now()).unwrap();
    assert_eq!(taken.results.len(), 1, "the first fits, the second doesn't");
    assert_eq!(taken.results[0].key, a.info_hash);
    assert_eq!(stat(&intake, |s| &s.refused), 2);
    // A new batch has its own budget.
    let three = sealed_bytes(W, 62, vec![leased(b)]);
    assert_eq!(
        intake.take(W, &three, &parts, now()).unwrap().results.len(),
        1
    );
}

/// The row's other fields are checked or rebuilt from the decoded list
/// (the security review): a `piece_count` that doesn't follow from the
/// files and `piece_len`, a `piece_len` metainfo refuses, or `SINGLE_FILE`
/// on a list that can't be one refuses the record; `PADDING` and
/// `SYMLINKS` are stored as the list says, whatever the worker sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_row_follows_the_file_list() {
    let rig = Rig::new(&[], 0).await;
    // One 1 MiB piece, as its one piece hash says (no metainfo warning).
    let mut count = prepared(&rig.worker_store, &info("Count.2025.mkv", 1 << 20));
    assert_eq!(count.flags & lodestar_store::flags::WARNINGS, 0);
    count.piece_count += 1;
    let mut zero = prepared(&rig.worker_store, &info("Zero.2025.mkv", GIB));
    zero.piece_len = 0;
    let mut single = prepared(&rig.worker_store, &big_info("Single.2025", 2, 10));
    single.flags |= lodestar_store::flags::SINGLE_FILE;
    let mut flagged = prepared(&rig.worker_store, &info("Flagged.2025.mkv", GIB));
    flagged.flags |= lodestar_store::flags::PADDING | lodestar_store::flags::SYMLINKS;
    let keys: Vec<InfoHash> = [&count, &zero, &single, &flagged]
        .iter()
        .map(|t| t.info_hash)
        .collect();
    let records = [count, zero, single, flagged]
        .into_iter()
        .map(|t| {
            let token = rig.lease(W, t.info_hash, 1);
            record(t, token)
        })
        .collect();
    assert!(rig.sink.results(W, payload(70, records)));
    let id = rig.wait_stored(&keys[3]).await;
    let row = reader(&rig.db).torrent(id).unwrap().unwrap();
    let both = lodestar_store::flags::PADDING | lodestar_store::flags::SYMLINKS;
    assert_eq!(row.flags & both, 0, "rebuilt from the list");
    let authority = rig.authority();
    until("the refused records' leases end", || {
        keys[..3].iter().all(|k| !authority.in_flight(k))
    })
    .await;
    assert_eq!(stat(&rig.remote(), |s| &s.refused), 3);
    for k in &keys[..3] {
        assert!(rig.stored(k).is_none());
    }
    rig.pipeline.shutdown().await.unwrap();
}

/// An honest torrent whose folders are deep and unshared: its directory
/// paths (copied into every child when decoded) outweigh its info dict,
/// which spells out only the files' paths. It's stored: a record's path
/// bytes are bounded by the largest dict (16 MiB), not its own size.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_honest_deep_tree_is_stored() {
    let rig = Rig::new(&[], 0).await;
    let mut raw = b"d5:filesl".to_vec();
    for f in 0..20 {
        let segs: String = (0..30)
            .map(|d| bytes(&format!("{f:02}{d:02}{}", "x".repeat(96))))
            .collect();
        raw.extend(format!("d6:lengthi1e4:pathl{segs}ee").into_bytes());
    }
    raw.extend(
        format!(
            "e4:name{}12:piece lengthi16384e6:pieces20:{}e",
            bytes("Deep.Honest.2026"),
            "\0".repeat(20)
        )
        .into_bytes(),
    );
    let t = prepared(&rig.worker_store, &raw);
    let shape = lodestar_store::Preparer::from_dict(None, 3, false)
        .decode_files(&t.files, 16 << 20, u64::MAX, u64::MAX)
        .unwrap()
        .2;
    assert!(
        shape.path_bytes > t.info_size + 4096,
        "{} path bytes from a {}-byte dict",
        shape.path_bytes,
        t.info_size
    );
    let key = t.info_hash;
    let token = rig.lease(W, key, 1);
    assert!(rig.sink.results(W, payload(80, vec![record(t, token)])));
    rig.wait_stored(&key).await;
    assert_eq!(stat(&rig.remote(), |s| &s.refused), 0);
    rig.pipeline.shutdown().await.unwrap();
}

/// The intake charges each record's decoded path bytes to the batch: five
/// trees of about 15.6 MB of paths each (one 4,000-byte folder over 3,900
/// files; small as blobs) fit the 64 MiB batch four times, and the fifth
/// is refused alone. Taken directly, with no writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_bytes_are_budgeted_per_batch() {
    use lodestar_pipeline::remote::{BATCH_PATH_BYTES, NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.db"), StoreConfig::default()).unwrap();
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    };
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let mut records = Vec::new();
    let mut keys = Vec::new();
    for n in 0..5u8 {
        let mut t = prepared(&ws, &info(&format!("Wide.{n}.2026.mkv"), GIB));
        let folder = vec![b'a' + n; 4_000];
        let list: Vec<lodestar_metainfo::File> = (0..3_900)
            .map(|i| lodestar_metainfo::File {
                path: vec![folder.clone(), format!("{i:06}").into_bytes()],
                size: 1,
                attrs: lodestar_metainfo::FileAttrs::empty(),
                symlink_target: None,
                pieces_root: None,
                hashes: lodestar_metainfo::FileHashes::default(),
            })
            .collect();
        let raw =
            lodestar_store::filetree::encode(&list, lodestar_metainfo::TextEncoding::UTF8).unwrap();
        // The dict spells out every path: about 4 KB a file.
        t.info_size = 3_900 * 4_040;
        t.files = lodestar_store::BlobCodec::new(19).compress(&raw).unwrap();
        t.size = 3_900;
        t.file_count = 3_900;
        t.piece_count = 1;
        t.flags &= !lodestar_store::flags::SINGLE_FILE;
        keys.push(t.info_hash);
        let c = Claim {
            key: t.info_hash,
            source: src(1),
            strategy: 0,
            at: now(),
        };
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(token),
            ..
        } = authority.claim_batch(W, &[c], now(), None, None)[0]
        else {
            panic!()
        };
        records.push(record(t, token));
    }
    const { assert!(BATCH_PATH_BYTES == 64 << 20) };
    let bytes = sealed_bytes(W, 90, records);
    let taken = intake.take(W, &bytes, &parts, now()).unwrap();
    let kept: Vec<InfoHash> = taken.results.iter().map(|r| r.key).collect();
    assert_eq!(kept, keys[..4], "four fit; the fifth passes the budget");
    assert_eq!(stat(&intake, |s| &s.refused), 1);
}

/// A blob whose shape passes but which fails to decode (a trailing byte
/// `shape` doesn't read) still costs the batch what decoding it built: one
/// 4,000-byte folder over 4,100 files is 16.4 MB of paths from a 20 KB
/// blob. Four of them spend the 64 MiB, and the next four are refused by
/// their shape, before decoding (the security review).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blob_that_fails_to_decode_still_spends_the_batch() {
    use lodestar_pipeline::remote::{NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    };
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let mut records = Vec::new();
    for n in 0..8u8 {
        let folder = vec![b'a' + n; 4_000];
        let list: Vec<lodestar_metainfo::File> = (0..4_100)
            .map(|_| lodestar_metainfo::File {
                path: vec![folder.clone(), b"f".to_vec()],
                size: 1,
                attrs: lodestar_metainfo::FileAttrs::empty(),
                symlink_target: None,
                pieces_root: None,
                hashes: lodestar_metainfo::FileHashes::default(),
            })
            .collect();
        let mut raw =
            lodestar_store::filetree::encode(&list, lodestar_metainfo::TextEncoding::UTF8).unwrap();
        raw.push(0);
        let shape = lodestar_store::filetree::shape(&raw, usize::MAX, usize::MAX).unwrap();
        assert!(shape.path_bytes > 16_000_000, "{}", shape.path_bytes);
        assert!(
            lodestar_store::filetree::decode(&raw).is_err(),
            "the trailing byte"
        );
        let mut t = prepared(&ws, &info(&format!("Trail.{n}.2026.mkv"), GIB));
        t.info_size = 28_000;
        assert!(raw.len() < 28_000);
        t.files = lodestar_store::Compressed {
            codec: lodestar_store::Codec::Raw,
            dict_id: None,
            data: raw,
        };
        let c = Claim {
            key: t.info_hash,
            source: src(1),
            strategy: 0,
            at: now(),
        };
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(token),
            ..
        } = authority.claim_batch(W, &[c], now(), None, None)[0]
        else {
            panic!()
        };
        records.push(record(t, token));
    }
    let bytes = sealed_bytes(W, 95, records);
    let taken = intake.take(W, &bytes, &parts, now()).unwrap();
    assert!(taken.results.is_empty());
    assert_eq!(stat(&intake, |s| &s.refused), 8);
    assert_eq!(
        stat(&intake, |s| &s.over_budget),
        4,
        "refused before decoding"
    );
}

/// A late result refused for its content uses up the coordinator's record
/// of the key: replayed in later batches, it finds no record and is stale,
/// never rebuilt again (the security review: a free key's replays would
/// otherwise cost a rebuild each, with no lease spent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_late_result_is_rebuilt_at_most_once() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = Store::open(ws_dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let mut t = prepared(&ws, &info("Replayed.Late.2026.mkv", GIB));
    let key = t.info_hash;
    let queue = [QueuedKey {
        info_hash: key,
        source: src(5),
        cohort: now().div_euclid(3600) - 1,
        strategy: 3,
        priority: 0,
        attempts: 0,
        private_source: false,
    }];
    let rig = Rig::new(&queue, 0).await;
    let authority = rig.authority();
    // The built-in worker held it after the restart, then lost it.
    let l = authority.get(&key).unwrap();
    assert!(authority.lost(&key, l.token, 1).is_some());
    // Its counts don't match its file list: refused when rebuilt.
    t.size += 1;
    let earlier = Token(((now() as u64 - 3_600) << 32) | 9);
    for batch in 1..=6 {
        assert!(
            rig.sink
                .results(W, payload(100 + batch, vec![record(t.clone(), earlier)]))
        );
    }
    rig.wait_records(6).await;
    let remote = rig.remote();
    assert_eq!(stat(&remote, |s| &s.refused), 1, "rebuilt once");
    assert_eq!(stat(&remote, |s| &s.stale), 5);
    assert_eq!(stat(&remote, |s| &s.late), 0);
    assert!(rig.stored(&key).is_none() && !authority.in_flight(&key));
    rig.pipeline.shutdown().await.unwrap();
}

/// WK-15: each remote worker's admitted rediscovered and dropped counters
/// are summed again in metric 18 (keys 4 and 5), equal per worker to its
/// metric-17 rows, which stay the source of truth: over several hours of
/// the window, for a worker with only dropped rows, and never for a
/// refused counter or one over a row's cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_totals_match_the_packed_rows() {
    use lodestar_pipeline::marginal::key;
    use lodestar_pipeline::wire::CountersBatch;
    use lodestar_store::Counter;
    let rig = Rig::new(&[], 0).await;
    let hour = now().div_euclid(3600);
    let c = |hour, metric, dim, key, value| Counter {
        hour,
        metric,
        dim,
        key,
        value,
    };
    let other = WorkerId(9);
    // Two of these pass the row's cap of 2^32 an hour.
    const BIG: i64 = 3 << 30;
    let send = |w: WorkerId, batch: u64, counters: Vec<Counter>| {
        let bytes = CountersBatch { batch, counters }.encode().unwrap();
        assert!(rig.sink.counters(w, Payload { batch, bytes }));
    };
    send(
        W,
        1,
        vec![
            c(hour, metric::REDISCOVERED, 3, 2, 40),
            c(hour, metric::REDISCOVERED, 4, 0, 6),
            c(hour - 5, metric::REDISCOVERED, 3, 2, 11),
            c(hour - 23, metric::DROPPED, 3, 0, 4),
            // Refused: outside the window, and outside the domain.
            c(hour - 30, metric::REDISCOVERED, 3, 2, 1_000),
            c(hour, metric::DROPPED, 3, 1, 1_000),
            // Scrapes: per worker in metric 17 only.
            c(hour, metric::SCRAPED, 1, 0, 3),
        ],
    );
    // A worker with only dropped rows, one of them over its row's cap.
    send(
        other,
        2,
        vec![
            c(hour - 1, metric::DROPPED, 3, 0, 2),
            c(hour - 1, metric::DROPPED, 3, 0, BIG),
            c(hour - 1, metric::DROPPED, 3, 0, BIG),
        ],
    );
    let db = rig.db.clone();
    let rows = move |m| reader(&db).rollup(m, 0, i64::MAX).unwrap();
    until("the counters are flushed", || {
        rows(metric::WORKER_LEASES).len() >= 4
    })
    .await;
    // Per (worker, key 4 or 5): metric 17's sums, and metric 18's.
    let mut packed = std::collections::BTreeMap::new();
    for r in rows(metric::BY_WORKER) {
        let k = match (r.key >> 16) as u32 {
            metric::REDISCOVERED => key::REDISCOVERED,
            metric::DROPPED => key::DROPPED,
            _ => continue,
        };
        *packed.entry((r.dim, k)).or_insert(0i64) += r.value;
    }
    let mut totals = std::collections::BTreeMap::new();
    for r in rows(metric::WORKER_LEASES) {
        *totals.entry((r.dim, r.key)).or_insert(0i64) += r.value;
    }
    assert_eq!(totals, packed);
    // The row's cap (2^32 per worker per hour) holds the rest back.
    assert_eq!(
        totals.into_iter().collect::<Vec<_>>(),
        [
            ((W.0, key::REDISCOVERED), 57),
            ((W.0, key::DROPPED), 4),
            ((other.0, key::DROPPED), 1 << 32),
        ]
    );
    rig.pipeline.shutdown().await.unwrap();
}

/// Counters (WK-9b, requirement 2) reach the rollups twice, as the fleet's
/// row and as the worker's `BY_WORKER` row, whose dim is the session's
/// worker. They're accepted only within their metric's domain and the
/// window. A worker can't write the coordinator's own metrics, or
/// `BY_WORKER` rows, its own or another worker's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counters_go_into_the_rollups_with_the_worker() {
    use lodestar_pipeline::wire::CountersBatch;
    use lodestar_store::Counter;
    let rig = Rig::new(&[], 0).await;
    let hour = now().div_euclid(3600);
    let c = |hour, metric, dim, key, value| Counter {
        hour,
        metric,
        dim,
        key,
        value,
    };
    let counters = vec![
        c(hour, metric::REDISCOVERED, 3, 2, 40),
        // Out of the metric's domain: its dim, then its key.
        c(hour, metric::REDISCOVERED, 64, 2, 5),
        c(hour, metric::REDISCOVERED, 3, 256, 5),
        // The coordinator's own count.
        c(hour, metric::HARVESTED, 3, 2, 1_000),
        // Another worker's row, and one of its own: both written only here.
        c(
            hour,
            metric::BY_WORKER,
            8,
            metric::by_worker_key(metric::REDISCOVERED, 3, 2),
            99,
        ),
        c(
            hour,
            metric::BY_WORKER,
            W.0,
            metric::by_worker_key(metric::REDISCOVERED, 3, 2),
            99,
        ),
        // Outside the window: older than a day, or ahead of the clock.
        c(hour - 30, metric::REDISCOVERED, 3, 2, 5),
        c(hour + 2, metric::REDISCOVERED, 3, 2, 5),
    ];
    let bytes = CountersBatch { batch: 1, counters }.encode().unwrap();
    assert!(rig.sink.counters(W, Payload { batch: 1, bytes }));
    let remote = rig.remote();
    assert_eq!(stat(&remote, |s| &s.counters), 1);
    assert_eq!(stat(&remote, |s| &s.counters_refused), 7);
    // Undecodable: answered, and nothing kept.
    assert!(rig.sink.counters(
        W,
        Payload {
            batch: 2,
            bytes: vec![0xff; 8]
        }
    ));
    assert_eq!(stat(&remote, |s| &s.undecodable), 1);
    let db = rig.db.clone();
    let rows = move |m, from, to| reader(&db).rollup(m, from, to).unwrap();
    until("the counters are flushed", || {
        !rows(metric::BY_WORKER, hour, hour + 1).is_empty()
    })
    .await;
    let by_worker: Vec<_> = rows(metric::BY_WORKER, 0, i64::MAX)
        .iter()
        .map(|c| (c.hour, c.dim, c.key, c.value))
        .collect();
    let key = metric::by_worker_key(metric::REDISCOVERED, 3, 2);
    assert_eq!(by_worker, [(hour, W.0, key, 40)]);
    let fleet: Vec<_> = rows(metric::REDISCOVERED, 0, i64::MAX)
        .iter()
        .map(|c| (c.hour, c.dim, c.key, c.value))
        .collect();
    assert_eq!(fleet, [(hour, 3, 2, 40)]);
    // And once more as the worker's rediscovered total (WK-15).
    let leases: Vec<_> = rows(metric::WORKER_LEASES, 0, i64::MAX)
        .iter()
        .map(|c| (c.hour, c.dim, c.key, c.value))
        .collect();
    let rediscovered = lodestar_pipeline::marginal::key::REDISCOVERED;
    assert_eq!(leases, [(hour, W.0, rediscovered, 40)]);
    assert!(rows(metric::HARVESTED, 0, i64::MAX).is_empty());
    rig.pipeline.shutdown().await.unwrap();
}

/// Remote and waiting leases are saved with the fetch queue (§19.3), each
/// once. With no server (workers off, WK-1), a revoked worker's key goes
/// to the built-in worker within a heartbeat instead of waiting. After a
/// restart, a late result for a key the remote worker held finds its
/// record (plan Q7) and is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_leases_survive_a_restart_and_parked_ones_go_to_the_built_in_worker() {
    let rig = Rig::new(&[], 0).await;
    let t = prepared(&rig.worker_store, &info("Held.Over.Restart.2026.mkv", GIB));
    let (held, parked) = (t.info_hash, InfoHash([2; 20]));
    let token = rig.lease(W, held, 1);
    rig.lease(WorkerId(8), parked, 2);
    let authority = rig.authority();
    let mut rows: Vec<_> = rig
        .remote()
        .queue_rows()
        .into_iter()
        .map(|q| (q.info_hash, q.source))
        .collect();
    rows.sort();
    assert_eq!(rows, [(parked, src(2)), (held, src(1))]);
    // Worker 8 is revoked: its key waits, then goes to the built-in worker.
    assert_eq!(authority.evict(WorkerId(8)), 1);
    // One condition for both: `hand_parked` makes the built-in worker the
    // holder before the pool takes the key, and counts it only after, so
    // waiting for the holder alone could read `parked` in between.
    until("the built-in worker holds the parked key, counted", || {
        authority
            .get(&parked)
            .is_some_and(|l| l.holder == WorkerId::LOCAL)
            && stat(&rig.remote(), |s| &s.parked) == 1
    })
    .await;
    assert_eq!(authority.get(&parked).unwrap().source, src(2), "kept");
    let Rig { db, pipeline, .. } = rig;
    let stopped = now();
    let store = pipeline.shutdown().await.unwrap();
    let queue = store.load_fetch_queue().unwrap();
    let saved = |k: InfoHash| queue.iter().filter(|q| q.info_hash == k).count();
    assert_eq!((saved(held), saved(parked)), (1, 1), "each once");
    // A new epoch: the remote worker's token is from before it.
    until("a new second", || now() > stopped).await;
    let pipeline = Pipeline::start(config(), store, vec![]).await.unwrap();
    let sink = sink_for(&pipeline, &db, Arc::new(Fake::default()), 0);
    let authority = pipeline.monitor().lease_authority().clone();
    let l = authority.get(&held).expect("the built-in worker holds it");
    assert_eq!(l.holder, WorkerId::LOCAL);
    assert!(authority.lost(&held, l.token, 1).is_some());
    assert!(sink.results(W, payload(1, vec![record(t, token)])));
    until("the late result is stored", || {
        reader(&db).find(&held).unwrap().is_some()
    })
    .await;
    assert_eq!(stat(pipeline.monitor().remote(), |s| &s.late), 1);
    until("the late result's lease ends", || {
        !authority.in_flight(&held)
    })
    .await;
    // The funnel across both runs: two harvested (the remote grants; the
    // parked key and the restart are re-grants), the held key lost then
    // stored, one correction, the parked key in flight: 2 = 2 − 1 + 1.
    let store = pipeline.shutdown().await.unwrap();
    let funnel = store.cohorts(0, now().div_euclid(3600) + 1).unwrap();
    let count = |stage: Stage| -> i64 {
        funnel
            .iter()
            .filter(|c| c.stage == stage as u8)
            .map(|c| c.count)
            .sum()
    };
    let late_rows: i64 = store
        .rollup(metric::LATE_RESULTS, 0, now().div_euclid(3600) + 1)
        .unwrap()
        .iter()
        .map(|c| c.value)
        .sum();
    assert_eq!(count(Stage::Harvested), 2);
    assert_eq!(
        (count(Stage::Lost), count(Stage::Stored), late_rows),
        (1, 1, 1)
    );
    assert_eq!(
        count(Stage::Harvested),
        count(Stage::Lost) + count(Stage::Stored) - late_rows + 1
    );
}

/// Backpressure (§19.5): once the writer's queue reaches its high-water
/// mark, no lease is granted however many are asked for; batches already
/// harvested still queue, up to the intake's bound, then are refused for
/// now (they stay in the worker's spool). Neither queue grows past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backpressure_bounds_the_writer_queue() {
    use lodestar_cluster::GrantAdmission;
    use lodestar_pipeline::remote::{INTAKE_QUEUE, NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("b.db"), StoreConfig::default()).unwrap();
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    // A writer that never reads, with a high-water mark of 6.
    let (writer, _hold) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = Arc::new(RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    }));
    let sink = intake.sink(SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    });
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let send = |n: u64| {
        let t = prepared(&ws, &info(&format!("Batch.{n}.2020.mkv"), GIB));
        let c = Claim {
            key: t.info_hash,
            source: src(1),
            strategy: 0,
            at: now(),
        };
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(token),
            ..
        } = authority.claim_batch(W, &[c], now(), None, None)[0]
        else {
            panic!()
        };
        sink.results(W, payload(n, vec![record(t, token)]))
    };
    // Fill the writer's queue one batch at a time: grants go on below the
    // mark and stop at it.
    for n in 1..=8u64 {
        assert_eq!(sink.admit(W, 10), if n <= 6 { 10 } else { 0 }, "{n}");
        assert!(send(n));
        until("the batch reaches the writer", || {
            intake.writer_fill() == n as usize
        })
        .await;
    }
    // A flood of grant requests: none is admitted.
    for _ in 0..1_000 {
        assert_eq!(sink.admit(W, 64), 0);
    }
    assert_eq!(stat(&intake, |s| &s.held_back), 2 * 10 + 1_000 * 64);
    // Harvested batches still queue, up to the intake's bound (and the one
    // its feed holds for the writer), then wait in the worker's spool.
    let mut n = 9;
    while send(n) {
        n += 1;
        assert!(n < 9 + INTAKE_QUEUE as u64 + 2, "the intake's queue grew");
    }
    assert!(sink.queued() <= INTAKE_QUEUE);
    assert_eq!(intake.writer_fill(), 8, "the writer's queue stayed bounded");
    assert_eq!(stat(&intake, |s| &s.backpressure), 1);
}

/// A batch whose path components are spent refuses a tree claiming more
/// directories than are left, by its count alone, as over the budget
/// (WK-9b): a fresh batch reads the same claim and refuses it as corrupt,
/// not over the budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tree_over_the_components_left_is_over_the_budget() {
    use lodestar_pipeline::remote::{NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("d.db"), StoreConfig::default()).unwrap();
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    };
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let leased = |t: Prepared| {
        let c = Claim {
            key: t.info_hash,
            source: src(1),
            strategy: 0,
            at: now(),
        };
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(token),
            ..
        } = authority.claim_batch(W, &[c], now(), None, None)[0]
        else {
            panic!()
        };
        record(t, token)
    };
    // 1.2M components, of the batch's 2M.
    let first = wide(&ws, "Spends.2024.mkv", 100_000, 12);
    // A blob that claims 900,000 directories and no file.
    let claim = |n: u8| {
        let mut raw = vec![
            lodestar_store::filetree::FORMAT_VERSION,
            0,
            lodestar_metainfo::TextEncoding::UTF8.id(),
        ];
        for mut v in [900_000u64, 0] {
            while v >= 0x80 {
                raw.push((v as u8) | 0x80);
                v >>= 7;
            }
            raw.push(v as u8);
        }
        raw.resize(raw.len() + 1_800_000, 0);
        let mut t = prepared(&ws, &info(&format!("Claim.{n}.2024.mkv"), GIB));
        t.info_size = raw.len() as u64;
        t.files = lodestar_store::BlobCodec::new(19).compress(&raw).unwrap();
        t
    };
    let spent = sealed_bytes(W, 70, vec![leased(first), leased(claim(1))]);
    let taken = intake.take(W, &spent, &parts, now()).unwrap();
    assert_eq!(taken.results.len(), 1);
    assert_eq!(stat(&intake, |s| &s.refused), 1);
    assert_eq!(stat(&intake, |s| &s.over_budget), 1);
    let fresh = sealed_bytes(W, 71, vec![leased(claim(2))]);
    assert!(
        intake
            .take(W, &fresh, &parts, now())
            .unwrap()
            .results
            .is_empty()
    );
    assert_eq!(stat(&intake, |s| &s.refused), 2);
    assert_eq!(stat(&intake, |s| &s.over_budget), 1, "not the budget's");
}

/// Taking a late result's record is the check itself (WK-9b): two copies
/// taken at once both find the record and pass the store check together
/// (held here at the store check until both are in it); then one takes the
/// record and is rebuilt, and the other finds it gone and is stale. The
/// record is rebuilt once however batches are fed.
#[test]
fn copies_of_a_late_result_taken_at_once_are_rebuilt_once() {
    use lodestar_cluster::RestoredLease;
    use lodestar_pipeline::remote::{NoStanding, Prior, RemoteParts};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.db"), StoreConfig::default()).unwrap();
    let ws = Store::open(dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let mut t = prepared(&ws, &info("Twice.Late.2026.mkv", GIB));
    // Refused when rebuilt: its counts don't match its file list.
    t.size += 1;
    let key = t.info_hash;
    let started = now() - 60;
    let epoch = started as u32;
    let restored = RestoredLease {
        cohort: started.div_euclid(3600) - 1,
        source: src(5),
        sources: 1 << 5,
        strategy: 3,
        discovered_at: started - 3_600,
    };
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, _p) = tokio::sync::mpsc::channel(8);
    let intake = RemoteIntake::new(RemoteParts {
        authority,
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::new([(key, restored)], started),
        private_sources: 0,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: None,
    });
    let preparer = store.preparer();
    let gate = Arc::new(Gate {
        reader: Mutex::new(store),
        entered: std::sync::atomic::AtomicUsize::new(0),
        wanted: 2,
        open: (Mutex::new(false), std::sync::Condvar::new()),
    });
    let parts = SinkParts {
        preparer: Mutex::new(preparer),
        standing: Arc::new(NoStanding),
        keys: test_keys(),
        stored: gate.clone(),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    };
    let earlier = Token(((epoch as u64 - 3_600) << 32) | 9);
    let bytes = |batch| sealed_bytes(W, batch, vec![record(t.clone(), earlier)]);
    let (one, two) = (bytes(1), bytes(2));
    std::thread::scope(|s| {
        let a = s.spawn(|| intake.take(W, &one, &parts, now()));
        let b = s.spawn(|| intake.take(W, &two, &parts, now()));
        assert!(a.join().unwrap().unwrap().results.is_empty());
        assert!(b.join().unwrap().unwrap().results.is_empty());
    });
    assert_eq!(gate.entered.load(Ordering::SeqCst), 2, "both were checked");
    assert_eq!(stat(&intake, |s| &s.refused), 1, "rebuilt once");
    assert_eq!(stat(&intake, |s| &s.stale), 1);
    assert_eq!(stat(&intake, |s| &s.late), 0);
}

/// A store check that lets no caller through until `wanted` are in it,
/// waiting at most [`WAIT`].
struct Gate {
    reader: Mutex<Store>,
    entered: std::sync::atomic::AtomicUsize,
    wanted: usize,
    open: (Mutex<bool>, std::sync::Condvar),
}
impl StoredKeys for Gate {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        let (open, cv) = &self.open;
        let mut is_open = open.lock().unwrap();
        if self.entered.fetch_add(1, Ordering::SeqCst) + 1 >= self.wanted {
            *is_open = true;
            cv.notify_all();
        }
        let (is_open, timeout) = cv.wait_timeout_while(is_open, WAIT, |o| !*o).unwrap();
        assert!(*is_open && !timeout.timed_out(), "timed out at the gate");
        drop(is_open);
        let r = self.reader.lock().unwrap();
        keys.iter().map(|k| r.find(k).unwrap().is_some()).collect()
    }
}

/// Parked keys handed to the built-in worker keep their privacy (§18.7,
/// BEP 27: a key every source of which is private never reaches public
/// peer finding), and one it refuses (held, or on the filter's hash
/// floor) ends its lease as blocked and is never fetched (security
/// review; the floor's arm, WK-9c). A worker's hints for
/// a key the built-in worker holds never make it public either.
#[test]
fn parked_keys_keep_their_privacy_and_refusals() {
    use lodestar_pipeline::remote::{Prior, Refusals, RemoteParts};
    let epoch = (now() as u32).wrapping_sub(10);
    let authority = Arc::new(LeaseAuthority::new(
        epoch,
        600,
        Arc::new(lodestar_cluster::NoEvents),
    ));
    let (writer, _w) = tokio::sync::mpsc::channel(8);
    let (pool, mut to_pool) = tokio::sync::mpsc::channel(8);
    let held = Arc::new(lodestar_filter::HeldSet::default());
    let (private, public, blocked, floored) = (
        InfoHash([3; 20]),
        InfoHash([1; 20]),
        InfoHash([9; 20]),
        InfoHash([0xC5; 20]),
    );
    let intake = RemoteIntake::new(RemoteParts {
        authority: authority.clone(),
        writer,
        pool,
        rollups: Arc::new(lodestar_pipeline::Rollups::default()),
        epoch,
        prior: Prior::default(),
        // Source 3 finds only private keys.
        private_sources: 1 << 3,
        leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
        refusals: Some(Refusals {
            held: held.clone(),
            filter: Arc::new(filter_blocking(&[floored])),
        }),
    });
    for (key, s) in [(private, 3), (public, 1), (blocked, 1), (floored, 1)] {
        let c = Claim {
            key,
            source: src(s),
            strategy: 0,
            at: now(),
        };
        assert!(matches!(
            authority.claim(WorkerId(8), c, now()),
            Claimed::Granted(_)
        ));
    }
    held.insert(blocked.0);
    assert_eq!(authority.evict(WorkerId(8)), 4);
    intake.hand_parked(now());
    let mut handed = HashMap::new();
    while let Ok(d) = to_pool.try_recv() {
        assert!(!d.reply);
        handed.insert(d.found.key, d.found.private_source);
    }
    assert_eq!(handed, HashMap::from([(private, true), (public, false)]));
    assert!(!authority.in_flight(&blocked), "its lease ended as blocked");
    assert!(!authority.in_flight(&floored), "so did the floor's");
    assert_eq!(stat(&intake, |s| &s.parked), 2);
    assert_eq!(stat(&intake, |s| &s.parked_refused), 2);
    // A remote worker's hints for the private key, which the built-in
    // worker holds now: a reply whose flag can't make it public.
    let token = authority.get(&private).unwrap().token;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("h.db"), StoreConfig::default()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let sink = Arc::new(intake).sink(SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(lodestar_pipeline::remote::NoStanding),
        keys: test_keys(),
        stored: Arc::new(Reader(Mutex::new(store))),
        clocks: Arc::new(Offset(0)),
        hints_allow_private: false,
    });
    sink.local_hints(vec![HintItem {
        key: private.0,
        token: token.0,
        hints: vec!["93.184.216.34:6881".parse().unwrap()],
    }]);
    let d = to_pool.try_recv().expect("the hint reached the pool");
    assert!(d.reply && d.found.private_source, "{d:?}");
}

/// A saved row for a key that was stored after the row was taken (a
/// remote result still in the writer's queue at shutdown, or a crash
/// between a commit and the next save) isn't restored: the key is
/// neither fetched again nor counted twice (security review).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_row_for_a_stored_key_is_not_restored() {
    let rig = Rig::new(&[], 0).await;
    let t = prepared(&rig.worker_store, &info("Committed.Late.2026.mkv", GIB));
    let key = t.info_hash;
    let token = rig.lease(W, key, 1);
    assert!(rig.sink.results(W, payload(1, vec![record(t, token)])));
    rig.wait_stored(&key).await;
    let authority = rig.authority();
    until("its lease ends", || !authority.in_flight(&key)).await;
    let Rig { db, pipeline, .. } = rig;
    let mut store = pipeline.shutdown().await.unwrap();
    // The row the race leaves: taken while the lease was still held.
    store
        .save_fetch_queue(&[QueuedKey {
            info_hash: key,
            source: src(1),
            cohort: now().div_euclid(3600),
            strategy: 3,
            priority: 0,
            attempts: 0,
            private_source: false,
        }])
        .unwrap();
    let pipeline = Pipeline::start(config(), store, vec![]).await.unwrap();
    let authority = pipeline.monitor().lease_authority().clone();
    assert!(authority.get(&key).is_none(), "not restored");
    assert_eq!(pipeline.monitor().remote().late_records(), 0);
    let store = pipeline.shutdown().await.unwrap();
    let funnel = store.cohorts(0, now().div_euclid(3600) + 1).unwrap();
    let stored: i64 = funnel
        .iter()
        .filter(|c| c.stage == Stage::Stored as u8)
        .map(|c| c.count)
        .sum();
    assert_eq!(stored, 1);
    assert!(store.load_fetch_queue().unwrap().is_empty());
    drop(db);
}

/// A content filter whose hash floor (CS-1) blocks `hashes`, loaded the
/// way the pipeline loads the store's.
fn filter_blocking(hashes: &[InfoHash]) -> lodestar_filter::Handle {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let raw: Vec<[u8; 20]> = hashes.iter().map(|h| h.0).collect();
    store.add_cs_hashes(&raw, 0, 1).unwrap();
    let filter = lodestar_pipeline::default_filter();
    lodestar_pipeline::load_filter(
        &filter,
        &lodestar_pipeline::StoreFilter::read(&store).unwrap(),
    );
    for h in &raw {
        assert!(filter.current().blocked_hash(&[*h]).is_some());
    }
    filter
}

/// Saved keys held or on the filter's hash floor since they were saved
/// are restored only to end as blocked (WK-9c): never fetched, never
/// saved again, and their funnel rows are terminal, so the funnel still
/// reconciles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_keys_held_or_blocked_are_not_restored() {
    let (plain, held_key, floored) = (InfoHash([1; 20]), InfoHash([2; 20]), InfoHash([3; 20]));
    let cohort = now().div_euclid(3600) - 1;
    let queue: Vec<QueuedKey> = [plain, held_key, floored]
        .iter()
        .map(|k| QueuedKey {
            info_hash: *k,
            source: src(1),
            cohort,
            strategy: 0,
            priority: 0,
            attempts: 0,
            private_source: false,
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("l.db"), StoreConfig::default()).unwrap();
    store.save_fetch_queue(&queue).unwrap();
    store.add_cs_hashes(&[floored.0], 0, 1).unwrap();
    let held = Arc::new(lodestar_filter::HeldSet::default());
    held.insert(held_key.0);
    let config = PipelineConfig { held, ..config() };
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    let authority = pipeline.monitor().lease_authority().clone();
    assert!(authority.get(&plain).is_some());
    assert!(authority.get(&held_key).is_none() && authority.get(&floored).is_none());
    assert_eq!(
        pipeline.monitor().remote().late_records(),
        1,
        "only the plain key"
    );
    let store = pipeline.shutdown().await.unwrap();
    let saved: Vec<InfoHash> = store
        .load_fetch_queue()
        .unwrap()
        .iter()
        .map(|q| q.info_hash)
        .collect();
    assert_eq!(saved, [plain]);
    let funnel = store.cohorts(cohort, cohort + 1).unwrap();
    let lost: Vec<(u8, i64)> = funnel
        .iter()
        .filter(|c| c.stage == Stage::Lost as u8)
        .map(|c| (c.reason, c.count))
        .collect();
    assert_eq!(lost, [(lodestar_sources::Loss::Blocked as u8, 2)]);
}

/// Sealed records (WK-9c; WK-8's format): one that opens under this
/// session's worker id is rebuilt and stored like any record; every other
/// is refused alone, counted, and its lease ended once as invalid. The
/// cases: a wrong key, a key id the coordinator doesn't have, a tampered
/// seal, the `SEALED` flag over a plain record's bytes, a record in clear
/// (every remote record is sealed), one sealed for another worker, and a
/// sealed record without its flag (it doesn't parse: the batch is
/// undecodable, and the lease lasts while the worker heartbeats).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_records_that_open_are_taken() {
    let rig = Rig::new(&[], 0).await;
    let names = [
        "Opens.2026.mkv",
        "Wrong.Key.2026.mkv",
        "Unknown.Key.2026.mkv",
        "Tampered.2026.mkv",
        "Flag.On.Plain.2026.mkv",
        "In.Clear.2026.mkv",
        "Other.Worker.2026.mkv",
        "Flag.Missing.2026.mkv",
    ];
    let recs: Vec<ResultRecord> = names
        .iter()
        .map(|n| {
            let t = prepared(&rig.worker_store, &info(n, GIB));
            let token = rig.lease(W, t.info_hash, 1);
            record(t, token)
        })
        .collect();
    let keys: Vec<InfoHash> = recs.iter().map(|r| r.key).collect();
    let one = |i: usize| vec![recs[i].clone()];
    let mut payloads = vec![
        sealed_bytes(W, 1, one(0)),
        sealed_with(&WireKey::new(1, [8; 32]), W, 2, one(1)),
        sealed_with(&WireKey::new(2, [7; 32]), W, 3, one(2)),
    ];
    // A flipped byte in the seal's tag (the seal is the record's last
    // field).
    let mut tampered = sealed_bytes(W, 4, one(3));
    *tampered.last_mut().unwrap() ^= 1;
    payloads.push(tampered);
    // The seal's bytes overwritten, from its end, with the plain record a
    // seal would hold.
    let mut plain_flagged = sealed_bytes(W, 5, one(4));
    let plain = ResultsBatch {
        batch: 0,
        records: one(4),
    }
    .encode()
    .unwrap();
    let at = plain_flagged.len() - plain.len();
    plain_flagged[at..].copy_from_slice(&plain);
    payloads.push(plain_flagged);
    payloads.push(
        ResultsBatch {
            batch: 6,
            records: one(5),
        }
        .encode()
        .unwrap(),
    );
    payloads.push(sealed_bytes(WorkerId(8), 7, one(6)));
    // Version, batch id, one record, then its flags: `SEALED` is two
    // bytes of varint. Cleared, the rest reads as a record in clear.
    let mut unflagged = sealed_bytes(W, 8, one(7));
    assert_eq!(&unflagged[3..5], [0x80, 0x20], "the SEALED flag");
    unflagged.splice(3..5, [0u8]);
    payloads.push(unflagged);
    for (i, bytes) in payloads.into_iter().enumerate() {
        assert!(rig.sink.results(
            W,
            Payload {
                batch: i as u64 + 1,
                bytes
            }
        ));
    }
    rig.wait_records(7).await;
    let remote = rig.remote();
    until("the undecodable batch is counted", || {
        stat(&remote, |s| &s.undecodable) == 1
    })
    .await;
    rig.wait_stored(&keys[0]).await;
    assert_eq!(stat(&remote, |s| &s.unopened), 5);
    assert_eq!(stat(&remote, |s| &s.clear), 1);
    assert_eq!(stat(&remote, |s| &s.committed), 1);
    let authority = rig.authority();
    until("the refused records' leases end", || {
        keys[1..7].iter().all(|k| !authority.in_flight(k))
    })
    .await;
    for k in &keys[1..] {
        assert!(rig.stored(k).is_none());
    }
    assert!(
        authority.in_flight(&keys[7]),
        "an undecodable batch ends nothing"
    );
    rig.pipeline.shutdown().await.unwrap();
}

/// End to end (WK-8's last hop, WK-9c): a worker's sealed record whose
/// name the commit's own re-judge quarantines (CS-2a here) is opened,
/// rebuilt, and lands in the quarantine with its name sealed under the
/// coordinator's own key. The name is nowhere in the data folder in clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sealed_quarantine_reaches_the_quarantine_sealed() {
    const NAME: &str = "Title 15yo lexa";
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let seal_key = lodestar_filter::Sealer::generate_key().unwrap();
    // The stand-in term lists `pipeline.rs` uses, so the pattern (CS-2a)
    // hits.
    use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Switches, TermSet};
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let config = PipelineConfig {
        sealer: Some(Arc::new(lodestar_filter::Sealer::new(&seal_key))),
        filter: Arc::new(Handle::new(Engine::new(Core::new(
            lists,
            CsList::default(),
            Switches::default(),
        )))),
        ..config()
    };
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    let sink = sink_for(&pipeline, &db, Arc::new(Fake::default()), 0);
    let worker_dir = tempfile::tempdir().unwrap();
    let ws = Store::open(worker_dir.path().join("w.db"), StoreConfig::default()).unwrap();
    let t = prepared(&ws, &info(NAME, GIB));
    let key = t.info_hash;
    let c = Claim {
        key,
        source: src(1),
        strategy: 3,
        at: now() - 30,
    };
    let authority = pipeline.monitor().lease_authority().clone();
    let BatchAnswer::Claimed {
        claimed: Claimed::Granted(token),
        ..
    } = authority.claim_batch(W, &[c], now(), None, None)[0]
    else {
        panic!()
    };
    let bytes = sealed_bytes(W, 1, vec![record(t, token)]);
    assert!(!bytes.windows(NAME.len()).any(|w| w == NAME.as_bytes()));
    assert!(sink.results(W, Payload { batch: 1, bytes }));
    until("quarantined", || {
        pipeline.writer_stats().quarantined.load(Ordering::Relaxed) == 1
    })
    .await;
    until("its lease ends", || !authority.in_flight(&key)).await;
    let store = pipeline.shutdown().await.unwrap();
    assert!(store.find(&key).unwrap().is_none());
    let q = store.quarantined(10).unwrap();
    assert_eq!(
        (q.len(), q[0].hash, q[0].rule.as_str()),
        (1, key.0, "CS-2a")
    );
    let sealed = q[0].sealed.as_deref().expect("a sealed name");
    assert_eq!(
        lodestar_filter::Sealer::new(&seal_key)
            .open(sealed)
            .unwrap(),
        NAME
    );
    drop(store);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let data = std::fs::read(&path).unwrap();
            assert!(
                !data.windows(NAME.len()).any(|w| w == NAME.as_bytes()),
                "{} holds the name in clear",
                path.display()
            );
        }
    }
}
