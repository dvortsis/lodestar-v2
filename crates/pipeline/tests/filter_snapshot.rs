//! What a worker judges with (WK-8, architecture §19.4 and §20, plan Q1),
//! end to end on the worker's side: the coordinator's snapshot travels in
//! chunks, the worker builds its engine from it, and its real stages (pool,
//! prepare, judge) decide what reaches the queue that feeds the wire and
//! the spool. A hash-list drop never does; everything else does, sealed.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_cluster::proto::SnapshotKind;
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::Action;
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::StoreFilter;
use lodestar_pipeline::gate::GateStats;
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::stages::{
    ByteReceiver, OUTBOUND_BYTES, Queues, Reports, WorkerLeases, WorkerStages, byte_queue,
};
use lodestar_pipeline::wire::ResultsBatch;
use lodestar_pipeline::wire::filter_snapshot::{
    ContentList, DictSnapshot, FilterSnapshot, Incoming, Judge, Judging, SealedRecord, Step,
    WireKey, WireKeys, chunk, chunk_count, version_of,
};
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, FunnelCounts, Loss, PoolConfig, Shutdown,
};
use lodestar_store::{
    FilterState, PreparedTorrent, Preparer, RuleRow, RuleSet, SourceId, Store, StoreConfig,
    WriteBatch,
};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

const WORKER: WorkerId = WorkerId(7);

thread_local! {
    /// Read once per test thread, so the snapshot and the coordinator's
    /// keyring agree.
    static KEY_DAY: u32 = key_day();
}
/// A word only our own rule quarantines.
const SECRET: &str = "zebraquark";

fn info(name: &str, length: i64) -> Vec<u8> {
    to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(length)),
            (b"name".to_vec(), OwnedValue::from(name)),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .collect(),
    ))
}

fn key_of(raw: &[u8]) -> InfoHash {
    InfoHash(lodestar_peer_wire::testing::Seed::v1(raw.to_vec()).info_hash)
}

#[derive(Default)]
struct MapFetch(Mutex<HashMap<InfoHash, Vec<u8>>>);

impl Fetcher for MapFetch {
    fn fetch(
        &self,
        _: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let info = self.0.lock().unwrap().get(&key).cloned();
        Box::pin(async move {
            info.map(|info| Fetched {
                info,
                v2: false,
                encrypted: false,
                pex: Vec::new(),
                client: None,
            })
            .ok_or(FailureKind::Refused)
        })
    }
}

#[derive(Default)]
struct Told(Mutex<Vec<(InfoHash, Token, Loss)>>);

impl Reports for Told {
    fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        self.0.lock().unwrap().push((*key, token, loss));
        true
    }
}

/// The wire key's day. A worker seals while its clock is on that day or
/// before it, so tomorrow's id keeps the test sealing even if it runs
/// across midnight UTC (the stages judge on the real clock).
fn key_day() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (secs / 86_400) as u32 + 1
}

fn source() -> SourceId {
    SourceId::new(1).unwrap()
}

/// What the coordinator would send: one CS-1 entry, one CS-8 signature
/// (of a 60 MB payload, specific), a subscribed list that drops, and your
/// rule that quarantines.
fn coordinator_snapshot(cs1: InfoHash, sig: u64, listed: InfoHash) -> FilterSnapshot {
    FilterSnapshot {
        key: WireKey::new(KEY_DAY.with(|d| *d), [9; 32]),
        filter: StoreFilter {
            state: FilterState {
                cs_hashes: vec![(cs1.0, 0)],
                cs_sigs: vec![sig],
                ..FilterState::default()
            },
            rules: RuleSet {
                rules: vec![RuleRow {
                    id: 1,
                    query: SECRET.into(),
                    action: Action::Quarantine.code(),
                    enabled: true,
                    shadow_until: None,
                    unless_query: None,
                }],
                ..RuleSet::default()
            },
            class_rules: Vec::new(),
            spam_sigs: Vec::new(),
        },
        lists: vec![ContentList {
            id: 3,
            floor: false,
            action: Action::Drop.code(),
            hashes: vec![listed.0],
        }],
    }
}

/// Sends `payload` chunk by chunk, as the coordinator paces them, and
/// returns what the worker assembled.
fn transfer(kind: SnapshotKind, version: u64, payload: &[u8]) -> Vec<u8> {
    let mut inc = Incoming::new(kind);
    for seq in 0..chunk_count(payload.len()) {
        match inc.take(chunk(kind, version, payload, seq)).unwrap() {
            Step::Ack(a) => assert_eq!(a.seq, seq),
            Step::Done(a, c) => {
                assert_eq!((a.seq, c.version), (seq, version));
                return c.bytes;
            }
        }
    }
    panic!("never complete");
}

async fn until(what: &str, mut done: impl FnMut() -> bool, saw: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out: {what}; saw {}",
            saw()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn recv(rx: &mut ByteReceiver<SealedRecord>) -> SealedRecord {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an outbound record within 10 s")
        .expect("open")
        .into_inner()
}

/// The plan's gates on the worker's side: a drop-action fixture never
/// reaches the queue that feeds the wire and the spool, and a
/// quarantine-action fixture does, sealed, opening on the coordinator.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drops_stay_on_the_worker_and_quarantine_goes_sealed() {
    let clean = info("A holiday video", 1 << 20);
    let cs1 = info("On the floor", 1 << 20);
    let repost = info("A renamed repost", 60 << 20);
    let listed = info("On a list", 1 << 20);
    let held = info(&format!("{SECRET} holiday video"), 1 << 20);
    let raws = [&clean, &cs1, &repost, &listed, &held];
    let keys: Vec<InfoHash> = raws.iter().map(|r| key_of(r)).collect();
    let files = {
        let i = Info::parse(&repost).unwrap();
        lodestar_classify::file_infos(&i.files, i.path_encoding)
    };
    let sig = lodestar_filter::payload_sig(&files).unwrap();
    assert!(sig.specific);

    // The coordinator's snapshot, in chunks, into the worker's judge.
    let snapshot = coordinator_snapshot(keys[1], sig.sig, keys[3]);
    let bytes = snapshot.encode().unwrap();
    let version = version_of(&bytes);
    let got = FilterSnapshot::decode(&transfer(SnapshotKind::Filter, version, &bytes)).unwrap();
    let judge = Arc::new(Judge::new(WORKER));
    assert!(judge.install(&got, version).is_empty());
    assert_eq!(judge.version(), version);

    let fetcher = Arc::new(MapFetch::default());
    for (k, r) in keys.iter().zip(raws) {
        fetcher.0.lock().unwrap().insert(*k, r.clone());
    }
    let told = Arc::new(Told::default());
    let leases = Arc::new(WorkerLeases::default());
    let (tx, rx) = mpsc::channel(64);
    let (stop, shutdown): (watch::Sender<bool>, Shutdown) = Shutdown::new();
    let (out, mut wire) = byte_queue::<SealedRecord>(OUTBOUND_BYTES, 64);
    let defaults = StoreConfig::default();
    let running = WorkerStages {
        worker: WORKER,
        pool: PoolConfig::default(),
        fetcher,
        peer_finders: Vec::new(),
        weights: HashMap::new(),
        restore: Vec::new(),
        preparer: Preparer::from_dict(None, defaults.compression_level, false),
        filter: judge.filter(),
        reports: told.clone(),
        leases: leases.clone(),
        funnel: Arc::new(FunnelCounts::default()),
        gate: Arc::new(GateStats::new(0)),
        stats: Arc::default(),
        adult: AdultSources::default(),
        queues: Queues::worker(),
    }
    .spawn(
        rx,
        shutdown,
        Judging {
            judge: judge.clone(),
            reports: told.clone(),
            out,
        },
    );
    for (n, k) in keys.iter().enumerate() {
        leases.insert(*k, Token(100 + n as u64));
        let mut found = Found::key(*k, 0);
        found.peers = vec!["192.0.2.1:6881".parse().unwrap()];
        tx.send(Discovery {
            source: source(),
            found,
            at: 1_700_000_000,
            reply: false,
        })
        .await
        .unwrap();
    }

    // Exactly two reach the queue, both sealed, their verdicts inside
    // (opened below): the clean one and the quarantined one.
    let mut outbound = vec![recv(&mut wire).await, recv(&mut wire).await];
    outbound.sort_by_key(|o| keys.iter().position(|k| *k == o.key));
    assert_eq!(
        (outbound[0].key, outbound[0].rule_version),
        (keys[0], version)
    );
    assert_eq!(outbound[1].key, keys[4]);

    // The three drops are reported lost as blocked, under their tokens.
    until(
        "the three drops reported",
        || told.0.lock().unwrap().len() == 3,
        || format!("{:?}", told.0.lock().unwrap()),
    )
    .await;
    let mut lost = told.0.lock().unwrap().clone();
    lost.sort_by_key(|(_, t, _)| t.0);
    assert_eq!(
        lost,
        vec![
            (keys[1], Token(101), Loss::Blocked),
            (keys[2], Token(102), Loss::Blocked),
            (keys[3], Token(103), Loss::Blocked),
        ]
    );
    let counts = &judge.counts;
    let n = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
    // The floor's CS-1 key and the list's hash are refused by prepare
    // before parsing (the key alone decides); the repost (CS-8a), which
    // only its files show, by the judge.
    assert_eq!(
        (n(&counts.dropped), n(&counts.held), n(&counts.clear)),
        (1, 1, 1)
    );
    assert!(leases.is_empty(), "no lease left behind");

    // On the wire: nothing of the dropped torrents, and the sealed name
    // nowhere, in the payload or in Debug.
    let payload = ResultsBatch {
        batch: 1,
        records: Vec::new(),
    }
    .encode_with(&outbound)
    .unwrap();
    for needle in [
        SECRET,
        "A holiday video",
        "On the floor",
        "renamed repost",
        "On a list",
    ] {
        assert!(
            !payload
                .windows(needle.len())
                .any(|w| w == needle.as_bytes()),
            "{needle} on the wire"
        );
    }
    assert!(!format!("{outbound:?} {judge:?}").contains(SECRET));

    // The coordinator opens it under the authenticated worker's id and
    // gets the whole record back, for the commit to re-judge.
    let (clear, sealed) = ResultsBatch::decode_with(&payload).unwrap();
    assert!(clear.records.is_empty(), "nothing leaves a worker in clear");
    let keyring = WireKeys::default();
    keyring.set(vec![WireKey::new(KEY_DAY.with(|d| *d), [9; 32])]);
    let clean = keyring.open(&sealed[0], WORKER).unwrap();
    assert_eq!(
        (clean.torrent.name.as_str(), clean.action),
        ("A holiday video", None)
    );
    let opened = keyring.open(&sealed[1], WORKER).unwrap();
    assert_eq!(opened.torrent.name, format!("{SECRET} holiday video"));
    assert_eq!(opened.action, Some(Action::Quarantine));
    assert!(keyring.open(&sealed[1], WorkerId(8)).is_err());
    // An intake that doesn't open sealed records refuses them (fail closed).
    assert!(ResultsBatch::decode(&payload).unwrap().records.is_empty());

    stop.send(true).unwrap();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(10), running.pool_task)
        .await
        .expect("the pool stops")
        .unwrap();
}

/// The dictionary travels like the filter, and a blob prepared under it
/// names its id. A coordinator that doesn't have that dictionary refuses
/// to store the blob: the store's write fails rather than guessing.
/// (Per record, before the write, WK-9's intake refuses it alone:
/// `remote_intake.rs` `a_bad_blob_is_refused_alone`.)
#[test]
fn a_blob_under_a_dictionary_the_coordinator_lacks_is_refused() {
    // A raw-content dictionary: any bytes zstd can prime its window with.
    let dict = DictSnapshot {
        id: 7,
        bytes: b"Show S01E 1080p WEB h264 mkv ".repeat(64),
    };
    let bytes = dict.encode().unwrap();
    let got =
        DictSnapshot::decode(&transfer(SnapshotKind::Dictionary, dict.version(), &bytes)).unwrap();
    assert_eq!(got, dict);
    let mut preparer = Preparer::from_dict(Some((got.id, got.bytes)), 3, false);
    // A season pack: its file tree is long enough for zstd to pay.
    let files = (1..=40)
        .map(|e| {
            OwnedValue::Dict(
                [
                    (b"length".to_vec(), OwnedValue::Int(1 << 20)),
                    (
                        b"path".to_vec(),
                        OwnedValue::List(vec![OwnedValue::from(
                            format!("Show S01E{e:02} 1080p WEB h264.mkv").as_str(),
                        )]),
                    ),
                ]
                .into_iter()
                .collect(),
            )
        })
        .collect();
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"files".to_vec(), OwnedValue::List(files)),
            (b"name".to_vec(), OwnedValue::from("Show S01")),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20 * 160])),
        ]
        .into_iter()
        .collect(),
    ));
    let prepared = preparer.prepare(&Info::parse(&raw).unwrap(), None).unwrap();
    assert_eq!(prepared.files.dict_id, Some(7));

    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert_eq!(store.active_file_dict(), None);
    let pt = PreparedTorrent {
        torrent: &prepared,
        discovered_at: 1,
        metadata_at: 1,
        extra_flags: 0,
        source: source(),
        verified: true,
    };
    let err = store
        .write(&WriteBatch {
            prepared: &[pt],
            ..WriteBatch::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("dictionary"), "{err}");
    assert_eq!(store.find(&prepared.info_hash).unwrap(), None);
}
