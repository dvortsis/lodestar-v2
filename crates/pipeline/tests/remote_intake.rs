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

fn payload(batch: u64, records: Vec<ResultRecord>) -> Payload {
    Payload {
        batch,
        bytes: ResultsBatch { batch, records }.encode().unwrap(),
    }
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
        let sink = pipeline.monitor().remote().sink(SinkParts {
            preparer: Mutex::new(reader(&db).preparer()),
            standing: standing.clone(),
            stored: Arc::new(Reader(Mutex::new(reader(&db)))),
            clocks: Arc::new(Offset(offset_ms)),
            hints_allow_private: false,
        });
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
    // Worker 8 names worker 7's tokens.
    let other = WorkerId(8);
    assert!(
        rig.sink
            .results(other, payload(3, vec![record(a.clone(), ta)]))
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
        assert!(
            rig.sink
                .results(WorkerId(9), payload(batch, vec![record(b.clone(), tb2)]))
        );
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
    assert!(
        rig.sink
            .results(w9, payload(30, vec![record(torrents[4].clone(), t4)]))
    );
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
fn big_info(name: &str, files: usize, path_len: usize) -> Vec<u8> {
    let mut out = b"d5:filesl".to_vec();
    for f in 0..files {
        let seg = format!("{f:06}{}", "a".repeat(path_len - 6));
        out.extend(format!("d6:lengthi1048576e4:pathl{}:{seg}ee", seg.len()).into_bytes());
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
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
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
    let one = ResultsBatch {
        batch: 60,
        records: vec![leased(over)],
    }
    .encode()
    .unwrap();
    let taken = intake.take(W, &one, &parts, now()).unwrap();
    assert!(taken.results.is_empty(), "over the budget alone");
    let two = ResultsBatch {
        batch: 61,
        records: vec![leased(a.clone()), leased(b.clone())],
    }
    .encode()
    .unwrap();
    let taken = intake.take(W, &two, &parts, now()).unwrap();
    assert_eq!(taken.results.len(), 1, "the first fits, the second doesn't");
    assert_eq!(taken.results[0].key, a.info_hash);
    assert_eq!(stat(&intake, |s| &s.refused), 2);
    // A new batch has its own budget.
    let three = ResultsBatch {
        batch: 62,
        records: vec![leased(b)],
    }
    .encode()
    .unwrap();
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
    });
    let parts = SinkParts {
        preparer: Mutex::new(store.preparer()),
        standing: Arc::new(NoStanding),
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
    let bytes = ResultsBatch { batch: 90, records }.encode().unwrap();
    let taken = intake.take(W, &bytes, &parts, now()).unwrap();
    let kept: Vec<InfoHash> = taken.results.iter().map(|r| r.key).collect();
    assert_eq!(kept, keys[..4], "four fit; the fifth passes the budget");
    assert_eq!(stat(&intake, |s| &s.refused), 1);
}
