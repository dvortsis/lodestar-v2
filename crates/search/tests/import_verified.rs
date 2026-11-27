//! X-import-verified-no: a stored torrent is indexed `verified:yes` only
//! when its info dict was hashed to its key here. One whose metadata came
//! from another indexer's database (Magnetico, bitmagnet) is
//! `verified:imported`: shown by default, left out of `verified:yes`.

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::query::Verified;
use lodestar_search::{SearchIndex, index_dir, parse, stored_verified};
use lodestar_store::import::{
    BatchCommitted, Completeness, Decision, ImportBatch, ImportRecord, ImportTorrent, NewImportJob,
    Phase, Privacy, Verification,
};
use lodestar_store::{Prepared, PreparedTorrent, Store, StoreConfig, WriteBatch, flags};

const HEAP: usize = 15 << 20;
const NOW: i64 = 1_700_000_100;
const T: i64 = 1_700_000_000;
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

fn prepared(store: &Store, name: &str) -> Prepared {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let info = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(5)),
            (b"name".to_vec(), b(name.as_bytes())),
            (b"piece length".to_vec(), OwnedValue::Int(16_384)),
            (b"pieces".to_vec(), b(&[0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    store
        .preparer()
        .prepare(&Info::parse(&info).unwrap(), None)
        .unwrap()
}

/// What another indexer's database gives an adapter: no info dict, so
/// no `info_size` (IMP-2, IMP-3).
fn from_database(store: &Store, name: &str) -> Prepared {
    let mut p = prepared(store, name);
    p.info_size = 0;
    p
}

/// Writes `p` the way the writer does, with `extra_flags`.
fn write(store: &mut Store, p: &Prepared, extra_flags: i64) {
    let source = store.register_source("dht", "1", T).unwrap();
    let rows = [PreparedTorrent {
        torrent: p,
        discovered_at: T,
        metadata_at: T + 1,
        extra_flags,
        source,
        verified: true,
    }];
    store
        .write(&WriteBatch {
            prepared: &rows,
            ..WriteBatch::default()
        })
        .unwrap();
}

/// An IMP-1 job of `adapter` that commits `records` as one batch.
fn import(store: &mut Store, adapter: &str, records: &[(Prepared, Completeness)]) {
    let src = store.register_source(adapter, "1", T).unwrap();
    let job = store
        .create_import_job(&NewImportJob {
            adapter,
            adapter_ver: 1,
            dataset: b"dataset",
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, T)
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Reading, T + 10)
        .unwrap();
    let records: Vec<ImportRecord<'_>> = records
        .iter()
        .map(|(torrent, completeness)| {
            ImportRecord::Torrent(ImportTorrent {
                torrent,
                completeness: *completeness,
                verification: Verification::Unverified,
                privacy: Privacy::Unknown,
                repair: 0,
                source_at: Some(T - 100),
                files_dropped: 0,
                decision: PASS,
            })
        })
        .collect();
    let r = store
        .commit_import_batch(&ImportBatch {
            job: job.id,
            seq: 1,
            position: b"pos",
            source: src,
            source_bytes: 1,
            at: T + 20,
            class_gen: None,
            records: &records,
        })
        .unwrap();
    assert!(matches!(r, BatchCommitted::Committed { .. }), "{r:?}");
}

fn catch_up(index: &SearchIndex, store: &mut Store) {
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
}

/// The names matching `q`, sorted.
fn names(index: &SearchIndex, q: &str) -> Vec<String> {
    names_of(index, parse(q).unwrap())
}

/// The same, as a search runs it: unverified items hidden unless asked.
fn shown(index: &SearchIndex, q: &str) -> Vec<String> {
    names_of(index, parse(q).unwrap().without_unverified())
}

fn names_of(index: &SearchIndex, node: lodestar_search::Node) -> Vec<String> {
    let hits = index.search(&index.query(&node, NOW), 1000).unwrap().hits;
    let mut out = index.names(&hits).unwrap();
    out.sort();
    out
}

/// One store with each kind of stored torrent, indexed.
fn library(root: &Path) -> (Store, SearchIndex) {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    // The crawler's: fetched and hashed.
    let p = prepared(&store, "network");
    write(&mut store, &p, 0);
    // A full `.torrent` (a torrent cache's or a refetch's): an info dict
    // hashed when stored, flagged imported.
    let p = prepared(&store, "torrent-file");
    write(&mut store, &p, flags::IMPORTED);
    // The legacy bitmagnet importer: no info dict, no import record.
    let p = from_database(&store, "bitmagnet-legacy");
    write(&mut store, &p, flags::IMPORTED | flags::FILES_PARTIAL);
    // IMP-2 and IMP-3 jobs: unverified import records.
    let m = from_database(&store, "magnetico");
    import(
        &mut store,
        "magnetico",
        &[(m, Completeness::ReportedComplete)],
    );
    let b = from_database(&store, "bitmagnet");
    import(&mut store, "bitmagnet", &[(b, Completeness::Partial)]);
    let index = SearchIndex::open(&index_dir(root)).unwrap();
    catch_up(&index, &mut store);
    (store, index)
}

const IMPORTS: [&str; 3] = ["bitmagnet", "bitmagnet-legacy", "magnetico"];
const VERIFIED: [&str; 2] = ["network", "torrent-file"];
const ALL: [&str; 5] = [
    "bitmagnet",
    "bitmagnet-legacy",
    "magnetico",
    "network",
    "torrent-file",
];

#[test]
fn each_kind_of_stored_torrent_indexes_its_own_value() {
    let tmp = tempfile::tempdir().unwrap();
    let (_store, index) = library(tmp.path());
    assert_eq!(names(&index, "verified:yes"), VERIFIED);
    assert_eq!(names(&index, "verified:imported"), IMPORTS);
    assert_eq!(names(&index, "verified:no"), Vec::<String>::new());
    assert_eq!(names(&index, "-verified:imported"), VERIFIED);
    assert_eq!(names(&index, "-verified:yes"), IMPORTS);
    assert_eq!(names(&index, "verified:yes,imported"), ALL);
    assert_eq!(names(&index, "verified:imported size:5"), IMPORTS);
}

#[test]
fn imports_are_shown_by_default_and_with_unverified() {
    let tmp = tempfile::tempdir().unwrap();
    let (_store, index) = library(tmp.path());
    // The default clause hides only `verified:no`.
    assert_eq!(shown(&index, ""), ALL);
    assert_eq!(
        parse("x").unwrap().without_unverified().to_query(),
        "x verified:yes,imported"
    );
    // "Show unverified" shows all three.
    assert_eq!(shown(&index, "verified:yes,imported,no"), ALL);
    // Asking for one value is left as asked.
    assert_eq!(shown(&index, "verified:yes"), VERIFIED);
    assert_eq!(shown(&index, "verified:imported"), IMPORTS);
}

#[test]
fn a_network_fetch_verifies_a_partial_import_at_its_next_index_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut store, index) = library(tmp.path());
    // The crawler fetches the bitmagnet job's partial import: the store
    // upgrades it (`upgrade_partial`) and writes its record as verified.
    let fetched = prepared(&store, "bitmagnet");
    write(&mut store, &fetched, 0);
    catch_up(&index, &mut store);
    assert_eq!(
        names(&index, "verified:yes"),
        ["bitmagnet", "network", "torrent-file"]
    );
    assert_eq!(
        names(&index, "verified:imported"),
        ["bitmagnet-legacy", "magnetico"]
    );
}

/// Every flag combination that bears on it, with and without an import
/// record: `yes` exactly when the row isn't imported, the record says
/// verified, or there's no record and the row has an info dict.
#[test]
fn stored_verified_over_every_combination() {
    let records = [
        None,
        Some(Verification::Unknown),
        Some(Verification::Unverified),
        Some(Verification::Verified),
        Some(Verification::Conflicting),
    ];
    let others = [
        0,
        flags::FILES_PARTIAL,
        flags::PRIVATE,
        flags::REMOTE_UNVERIFIED,
        flags::BACKFILL | flags::V2,
    ];
    for imported in [false, true] {
        for other in others {
            for info_size in [0, 1, 4096] {
                for record in records {
                    let row_flags = other | if imported { flags::IMPORTED } else { 0 };
                    let want = if !imported
                        || record == Some(Verification::Verified)
                        || (record.is_none() && info_size > 0)
                    {
                        Verified::Yes
                    } else {
                        Verified::Imported
                    };
                    assert_eq!(
                        stored_verified(row_flags, info_size, record),
                        want,
                        "flags {row_flags:#x}, info_size {info_size}, {record:?}"
                    );
                }
            }
        }
    }
}
