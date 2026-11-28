//! Worker provenance and purge-by-worker (WK-12, V17), through the public
//! API: the commit write, promotion, the purge's steps (a property test:
//! exactly that worker's rows go), the preview counts and the snapshot
//! purge.

use std::collections::{BTreeMap, BTreeSet};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::{Claims, Confidence, Provenance};
use lodestar_store::unverified::{Network, NewUnverified, Promoted};
use lodestar_store::workers::{MAX_WORKER_PURGE_STEP, WorkerTorrents};
use lodestar_store::{
    Error, Inserted, NewTorrent, Prepared, PreparedTorrent, SourceId, Store, StoreConfig,
    WriteBatch, flags,
};
use proptest::prelude::*;

const T: i64 = 1_700_000_000;
const CODES: lodestar_store::SearchableCodes = lodestar_store::SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

fn info_bytes(name: &str, files: usize) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let mut e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    if files == 0 {
        e.push((b"length".to_vec(), OwnedValue::Int(5)));
    } else {
        let list = (0..files)
            .map(|f| {
                OwnedValue::Dict(
                    [
                        (b"length".to_vec(), OwnedValue::Int(1_000 + f as i64)),
                        (
                            b"path".to_vec(),
                            OwnedValue::List(vec![b(format!(
                                "{name}.part{f:03}.some.longer.file.name.mkv"
                            )
                            .as_bytes())]),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .collect();
        e.push((b"files".to_vec(), OwnedValue::List(list)));
    }
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn prepared(store: &Store, name: &str) -> Prepared {
    store
        .preparer()
        .prepare(&Info::parse(&info_bytes(name, 0)).unwrap(), None)
        .unwrap()
}

fn pt(p: &Prepared, source: SourceId) -> PreparedTorrent<'_> {
    PreparedTorrent {
        torrent: p,
        discovered_at: T,
        metadata_at: T,
        extra_flags: 0,
        source,
        verified: true,
    }
}

fn dht(store: &mut Store) -> SourceId {
    store.register_source("dht", "1", T).unwrap()
}

/// Commits `ps` in one batch, prepared by `by` (empty: all local).
fn commit(store: &mut Store, ps: &[&Prepared], source: SourceId, by: &[u32]) -> Vec<Inserted> {
    let pts: Vec<_> = ps.iter().map(|p| pt(p, source)).collect();
    store
        .write(&WriteBatch {
            prepared: &pts,
            prepared_by: by,
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn ids(store: &Store, sql: &str) -> BTreeSet<i64> {
    let conn = store.connection();
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn named(name: &str) -> Claims {
    Claims {
        name: Some(name.into()),
        size: Some(5),
        files: vec![],
        trackers: vec![],
        ids: vec![],
        description: None,
        publisher: None,
        provenance: Provenance {
            external_ref: None,
            published_at: None,
            confidence: Confidence::Medium,
        },
        adult: None,
        details: None,
    }
}

/// A remote worker's new rows record it; the built-in worker's and a
/// batch with no `prepared_by` record nothing; a second delivery of a
/// stored torrent by another worker doesn't take it over.
#[test]
fn remote_commits_record_their_worker_and_local_ones_nothing() {
    let mut store = mem();
    let src = dht(&mut store);
    let (a, b, c, d) = (
        prepared(&store, "a"),
        prepared(&store, "b"),
        prepared(&store, "c"),
        prepared(&store, "d"),
    );
    let out = commit(&mut store, &[&a, &b, &c], src, &[0, 7, 9]);
    let id = |i: Inserted| i.stored().unwrap();
    assert_eq!(store.torrent_worker(id(out[0])).unwrap(), None);
    assert_eq!(store.torrent_worker(id(out[1])).unwrap(), Some(7));
    assert_eq!(store.torrent_worker(id(out[2])).unwrap(), Some(9));
    let local = commit(&mut store, &[&d], src, &[]);
    assert_eq!(store.torrent_worker(id(local[0])).unwrap(), None);
    // Already stored and complete: its metadata stays worker 7's.
    let again = commit(&mut store, &[&b], src, &[9]);
    assert_eq!(again[0], Inserted::Existing(id(out[1])));
    assert_eq!(store.torrent_worker(id(out[1])).unwrap(), Some(7));
    assert_eq!(count(&store, "SELECT count(*) FROM torrent_worker"), 2);
    assert_eq!(
        store.worker_torrents(7).unwrap(),
        WorkerTorrents {
            torrents: 1,
            with_claims: 0
        }
    );
    assert_eq!(store.worker_torrents(0).unwrap(), WorkerTorrents::default());
}

/// A single-machine install (every batch local) writes no row at all.
#[test]
fn a_single_machine_install_writes_nothing() {
    let mut store = mem();
    let src = dht(&mut store);
    for n in 0..20 {
        let p = prepared(&store, &format!("t{n}"));
        commit(&mut store, &[&p], src, if n % 2 == 0 { &[] } else { &[0] });
    }
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), 20);
    assert_eq!(count(&store, "SELECT count(*) FROM torrent_worker"), 0);
}

/// `prepared_by` that doesn't match `prepared` is refused, writing nothing.
#[test]
fn a_mismatched_prepared_by_is_refused() {
    let mut store = mem();
    let src = dht(&mut store);
    let (a, b) = (prepared(&store, "a"), prepared(&store, "b"));
    let pts = [pt(&a, src), pt(&b, src)];
    let err = store
        .write(&WriteBatch {
            prepared: &pts,
            prepared_by: &[7],
            ..WriteBatch::default()
        })
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), 0);
}

/// A remote record that upgrades a partial import writes its metadata, so
/// the row records that worker.
#[test]
fn a_remote_upgrade_of_a_partial_import_takes_its_worker() {
    let mut store = mem();
    let src = dht(&mut store);
    let raw = info_bytes("pack", 3);
    let real = Info::parse(&raw).unwrap();
    let mut partial = real.clone();
    partial.files.clear();
    let id = store
        .insert(&[NewTorrent {
            info: &partial,
            raw_info: None,
            discovered_at: T,
            metadata_at: T,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            source: src,
        }])
        .unwrap()[0]
        .id();
    assert_eq!(store.torrent_worker(id).unwrap(), None);
    let p = store.preparer().prepare(&real, None).unwrap();
    let out = commit(&mut store, &[&p], src, &[5]);
    assert_eq!(out[0], Inserted::Existing(id));
    assert_eq!(
        store.torrent(id).unwrap().unwrap().flags & flags::FILES_PARTIAL,
        0
    );
    assert_eq!(store.torrent_worker(id).unwrap(), Some(5));
}

/// A remote worker's promotion records it, in the promotion's own
/// transaction; a local one records nothing. The preview counts the
/// torrents that carry a source's claims.
#[test]
fn a_remote_promotion_records_its_worker() {
    let mut store = mem();
    let src = dht(&mut store);
    let feed = store.register_source("nyaa", "1", T).unwrap();
    let (a, b) = (prepared(&store, "a"), prepared(&store, "b"));
    let claims = named("A");
    for p in [&a, &b] {
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &p.info_hash.0,
                source: feed,
                private_source: false,
                claims: Some(&claims),
                at: T,
            }])
            .unwrap();
    }
    let item = |s: &Store, p: &Prepared| {
        s.unverified(Network::Bt, &p.info_hash.0)
            .unwrap()
            .unwrap()
            .id
    };
    let (ia, ib) = (item(&store, &a), item(&store, &b));
    assert_eq!(
        store.promote_from(ia, &pt(&a, src), CODES, T, 7).unwrap(),
        Promoted::Kept(ia)
    );
    assert_eq!(
        store.promote(ib, &pt(&b, src), CODES, T).unwrap(),
        Promoted::Kept(ib)
    );
    assert_eq!(store.torrent_worker(ia).unwrap(), Some(7));
    assert_eq!(store.torrent_worker(ib).unwrap(), None);
    let c = prepared(&store, "c");
    commit(&mut store, &[&c], src, &[7]);
    assert_eq!(
        store.worker_torrents(7).unwrap(),
        WorkerTorrents {
            torrents: 2,
            with_claims: 1
        }
    );
    // The purge takes the promoted torrent's claims with it.
    while !store.purge_worker_step(7, 10).unwrap().done {}
    assert!(store.torrent(ia).unwrap().is_none());
    assert_eq!(
        count(
            &store,
            &format!("SELECT count(*) FROM torrent_claims WHERE torrent_id = {ia}")
        ),
        0
    );
    assert!(store.torrent(ib).unwrap().is_some());
    assert_eq!(store.worker_torrents(7).unwrap(), WorkerTorrents::default());
}

/// The cap holds whatever limit is asked for.
#[test]
fn a_step_never_passes_the_cap() {
    let mut store = mem();
    let src = dht(&mut store);
    let n = MAX_WORKER_PURGE_STEP + 3;
    let ps: Vec<_> = (0..n).map(|i| prepared(&store, &format!("t{i}"))).collect();
    let refs: Vec<_> = ps.iter().collect();
    commit(&mut store, &refs, src, &vec![3; n]);
    let first = store.purge_worker_step(3, usize::MAX).unwrap();
    assert_eq!(first.deleted.len(), MAX_WORKER_PURGE_STEP);
    assert!(!first.done);
    let second = store.purge_worker_step(3, usize::MAX).unwrap();
    assert_eq!((second.deleted.len(), second.done), (3, true));
}

/// Ruling 4 (c): the rows a pre-V17 build's workers wrote, with
/// `REMOTE_UNVERIFIED` and no `torrent_worker` row, are counted and purged
/// on their own; local rows and attributed remote rows stay.
#[test]
fn unattributed_remote_rows_are_purged_alone() {
    let mut store = mem();
    let src = dht(&mut store);
    let ps: Vec<_> = (0..9).map(|i| prepared(&store, &format!("t{i}"))).collect();
    let write = |store: &mut Store, p: &Prepared, remote: bool, by: &[u32]| {
        let t = PreparedTorrent {
            extra_flags: if remote { flags::REMOTE_UNVERIFIED } else { 0 },
            ..pt(p, src)
        };
        store
            .write(&WriteBatch {
                prepared: &[t],
                prepared_by: by,
                ..WriteBatch::default()
            })
            .unwrap()
            .prepared[0]
            .id()
    };
    // As a v0.5.2 coordinator stored them: flagged, no worker recorded.
    let old: Vec<i64> = ps[..5]
        .iter()
        .map(|p| write(&mut store, p, true, &[]))
        .collect();
    let attributed: Vec<i64> = ps[5..7]
        .iter()
        .map(|p| write(&mut store, p, true, &[4]))
        .collect();
    let local: Vec<i64> = ps[7..]
        .iter()
        .map(|p| write(&mut store, p, false, &[]))
        .collect();
    assert_eq!(
        store.unattributed_remote_torrents().unwrap(),
        WorkerTorrents {
            torrents: 5,
            with_claims: 0
        }
    );
    let sweep = |store: &mut Store| {
        let (mut deleted, mut after, mut steps) = (Vec::new(), 0, 0);
        loop {
            let s = store.purge_unattributed_remote_step(after, 2).unwrap();
            assert!(s.deleted.len() <= 2);
            deleted.extend(s.deleted);
            steps += 1;
            match s.next {
                Some(n) => {
                    assert!(n > after, "the cursor moves on");
                    after = n;
                }
                None => return (deleted, steps),
            }
        }
    };
    let (deleted, steps) = sweep(&mut store);
    assert_eq!(deleted, old);
    assert_eq!(steps, 3, "2 + 2 + 1, one pass");
    for id in &old {
        assert!(store.torrent(*id).unwrap().is_none());
    }
    for id in attributed.iter().chain(&local) {
        assert!(store.torrent(*id).unwrap().is_some(), "{id} kept");
    }
    assert_eq!(store.worker_torrents(4).unwrap().torrents, 2);
    assert_eq!(
        store.unattributed_remote_torrents().unwrap(),
        WorkerTorrents::default()
    );
    assert_eq!(count(&store, "SELECT count(*) FROM snapshot_deletions"), 5);
    let logged = ids(&store, "SELECT torrent_id FROM reindex_log");
    assert!(old.iter().all(|id| logged.contains(id)));
    // One pass is complete: after V17 a remote row is attributed as it's
    // written, so a sweep after the first finds nothing new.
    let late = prepared(&store, "late");
    let late = write(&mut store, &late, true, &[4]);
    let (again, _) = sweep(&mut store);
    assert!(again.is_empty());
    assert!(store.torrent(late).unwrap().is_some());
    assert_eq!(
        store.unattributed_remote_torrents().unwrap(),
        WorkerTorrents::default()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Purge-by-worker deletes exactly that worker's torrents, in any step
    /// size, and nothing else: other workers' rows and provenance, local
    /// rows, and rows the worker only saw again. Each deleted id is queued
    /// for the indexer and each hash journalled for the snapshot purge,
    /// and no other.
    #[test]
    fn purge_by_worker_removes_exactly_its_rows(
        by in proptest::collection::vec(0u32..=4, 1..40),
        again in proptest::collection::vec((0usize..40, 0u32..=4), 0..10),
        target in 1u32..=4,
        step in 1usize..8,
    ) {
        let mut store = mem();
        let src = dht(&mut store);
        let ps: Vec<_> = (0..by.len()).map(|i| prepared(&store, &format!("t{i}"))).collect();
        let refs: Vec<_> = ps.iter().collect();
        let out = commit(&mut store, &refs, src, &by);
        // Seen again by other workers or the built-in one: provenance
        // stays the first's.
        for &(i, w) in &again {
            if let Some(p) = ps.get(i) {
                commit(&mut store, &[p], src, &[w]);
            }
        }
        let id_of: Vec<i64> = out.iter().map(|i| i.stored().unwrap()).collect();
        let want: BTreeSet<i64> = id_of
            .iter()
            .zip(&by)
            .filter(|&(_, &w)| w == target)
            .map(|(&id, _)| id)
            .collect();
        let before: BTreeMap<i64, Option<u32>> = id_of
            .iter()
            .map(|&id| (id, store.torrent_worker(id).unwrap()))
            .collect();
        prop_assert_eq!(store.worker_torrents(target).unwrap().torrents, want.len() as u64);
        let logged_before = ids(&store, "SELECT torrent_id FROM reindex_log");
        let journal_before = count(&store, "SELECT count(*) FROM snapshot_deletions");
        prop_assert_eq!(journal_before, 0);

        let mut deleted = Vec::new();
        loop {
            let s = store.purge_worker_step(target, step).unwrap();
            prop_assert!(s.deleted.len() <= step);
            deleted.extend(s.deleted);
            if s.done {
                break;
            }
        }
        let deleted_set: BTreeSet<i64> = deleted.iter().copied().collect();
        prop_assert_eq!(deleted.len(), deleted_set.len(), "each once");
        prop_assert_eq!(&deleted_set, &want);
        for (&id, &w) in &before {
            let stored = store.torrent(id).unwrap().is_some();
            prop_assert_eq!(stored, !want.contains(&id));
            if !want.contains(&id) {
                prop_assert_eq!(store.torrent_worker(id).unwrap(), w);
            }
        }
        let logged: BTreeSet<i64> = ids(&store, "SELECT torrent_id FROM reindex_log")
            .difference(&logged_before)
            .copied()
            .collect();
        prop_assert_eq!(&logged, &want);
        let want_hashes: BTreeSet<Vec<u8>> = id_of
            .iter()
            .zip(&ps)
            .filter(|(id, _)| want.contains(id))
            .map(|(_, p)| p.info_hash.0.to_vec())
            .collect();
        let conn = store.connection();
        let mut stmt = conn.prepare("SELECT hash FROM snapshot_deletions").unwrap();
        let journal: BTreeSet<Vec<u8>> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        prop_assert_eq!(journal, want_hashes);
        drop(stmt);
        prop_assert_eq!(store.worker_torrents(target).unwrap(), WorkerTorrents::default());
        prop_assert!(store.purge_worker_step(target, step).unwrap().done);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// The snapshot purge (owner §2.21): a worker's purged torrents leave a
/// snapshot taken before the purge, with their `torrent_worker` rows;
/// another worker's and local torrents stay in it.
#[test]
fn a_purged_workers_torrents_leave_the_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    let src = dht(&mut store);
    let names = [
        "PurgedWorkerMarker.one",
        "PurgedWorkerMarker.two",
        "KeptWorkerMarker",
        "KeptLocalMarker",
    ];
    let ps: Vec<_> = names.iter().map(|n| prepared(&store, n)).collect();
    let refs: Vec<_> = ps.iter().collect();
    commit(&mut store, &refs, src, &[7, 7, 9, 0]);
    // A snapshot of this V17 store, as the next migration would take it.
    let snap = dir.path().join("lodestar.pre-v18.db");
    store
        .connection()
        .execute("VACUUM INTO ?1", [snap.to_string_lossy()])
        .unwrap();
    while !store.purge_worker_step(7, 1).unwrap().done {}
    let report = store.purge_snapshots(now(), 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.torrents, 2, "{report:?}");
    let bytes = std::fs::read(&snap).unwrap();
    let has = |m: &str| bytes.windows(m.len()).any(|w| w == m.as_bytes());
    assert!(
        !has("PurgedWorkerMarker"),
        "no purged name left in the file"
    );
    assert!(has("KeptWorkerMarker") && has("KeptLocalMarker"));
    let conn = lodestar_store::rusqlite::Connection::open(&snap).unwrap();
    let rows: Vec<(String, Option<i64>)> = conn
        .prepare(
            "SELECT t.name, w.worker FROM torrents t
             LEFT JOIN torrent_worker w ON w.torrent_id = t.id ORDER BY t.name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("KeptLocalMarker".to_string(), None),
            ("KeptWorkerMarker".to_string(), Some(9)),
        ]
    );
    let orphans: i64 = conn
        .query_row(
            "SELECT count(*) FROM torrent_worker WHERE torrent_id NOT IN (SELECT id FROM torrents)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
    let fk: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0, "no child row left without its torrent");
    let ok: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
}

/// WK-12's step measurement (runner ruling 6): on a store of 1M torrents
/// with file lists, how long one purge step of each size holds the write
/// lock. Run with `cargo test --release -p lodestar-store --test workers
/// purge_step_time -- --ignored --nocapture`.
#[test]
#[ignore = "measurement; writes a ~700 MB store"]
fn purge_step_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    // 1M torrents with ~500-byte file blobs, parsed records and claims on
    // some, every 4th a remote worker's: the deletes walk real indexes and
    // carry every trigger and cascade a crawl's rows do.
    store
        .connection()
        .execute_batch(
            "BEGIN;
             INSERT INTO sources (id, plugin, version, state, added_at) VALUES (1, 'nyaa', '1', 0, 0)
                 ON CONFLICT DO NOTHING;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
             INSERT INTO torrents (id, info_hash, info_hash_v2, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at, sources)
             SELECT i, randomblob(20), CASE WHEN i % 10 = 0 THEN randomblob(32) END,
                 'Some.Release.Name.2024.1080p.WEB-DL.x264-GROUP.' || i,
                 i, 16384, 1, 12, 100, 16384, 0, 0, 0, 3 FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
             INSERT INTO torrent_files (torrent_id, codec, data) SELECT i, 0, randomblob(500) FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
             INSERT INTO torrent_claims (torrent_id, source_id, flags, data)
                 SELECT i, 1, 0, randomblob(200) FROM n WHERE i % 8 = 0;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
             INSERT INTO torrent_worker (torrent_id, worker) SELECT i, 7 FROM n WHERE i % 4 = 0;
             UPDATE id_seq SET next = 1000001;
             COMMIT;",
        )
        .unwrap();
    let bytes = std::fs::metadata(&path).unwrap().len();
    let mut lines = Vec::new();
    for limit in [256, 512, MAX_WORKER_PURGE_STEP, 2048, 4096] {
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let s = store.purge_worker_step(7, limit).unwrap();
            times.push(t.elapsed());
            assert_eq!(s.deleted.len(), limit.min(MAX_WORKER_PURGE_STEP));
        }
        times.sort();
        lines.push(format!(
            "limit {limit} (deletes {}): median {:?}, max {:?}",
            limit.min(MAX_WORKER_PURGE_STEP),
            times[2],
            times[4]
        ));
    }
    eprintln!(
        "1M torrents, {} MB:\n{}",
        bytes / 1_000_000,
        lines.join("\n")
    );
}
