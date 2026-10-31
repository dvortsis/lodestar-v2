//! The driver over real Magnetico fixtures (Bitmagnet's run in
//! `bitmagnet/pg_tests.rs`, where PostgreSQL is): the filter, crash
//! replay, stopping, merge and removal.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use lodestar_filter::{Core, CsList, Engine, Lists, Origin, Switches};
use lodestar_store::import::{
    Completeness, Control, ImportCounters, NewImportJob, Phase, Privacy, Removed, Revalidation,
    Verification,
};
use lodestar_store::{PreparedTorrent, SourceId, Store, StoreConfig, WriteBatch, flags};
use rusqlite::types::Value;

use super::*;
use crate::magnetico::fixtures::{Layout, TorrentRow, build};
use crate::magnetico::{OpenPolicy, open};

const NOW: i64 = 1_700_000_000;
const ROWS: u32 = 1_200;

fn now() -> i64 {
    NOW
}

fn engine() -> Engine {
    Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ))
}

/// The hash `TorrentRow::good(seed, …)` gets.
fn hash(seed: u32) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[..4].copy_from_slice(&seed.to_be_bytes());
    h[19] = 1;
    h
}

fn rows(seeds: impl Iterator<Item = u32>) -> Vec<TorrentRow> {
    seeds
        .map(|i| TorrentRow::good(i, &[("d/a", 3), ("d/b", 4)]))
        .collect()
}

fn fixture(dir: &Path, name: &str, rows: &[TorrentRow]) -> PathBuf {
    let path = dir.join(name);
    build(&path, Layout::V3, rows);
    path
}

fn adapter(path: &Path) -> Adapter {
    Adapter::Magnetico(open(path, &OpenPolicy::default()).unwrap())
}

fn mem() -> (Store, SourceId) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
    (store, source)
}

fn create(store: &mut Store, a: &Adapter) -> i64 {
    let (dataset, schema_sig) = a.identity();
    store
        .create_import_job(&NewImportJob {
            adapter: a.name(),
            adapter_ver: a.version(),
            dataset: &dataset,
            schema_sig: &schema_sig,
            options: b"",
            credential: None,
            at: NOW,
        })
        .unwrap()
        .id
}

fn opts<'a>(source: SourceId, engine: &'a Engine, stop: &'a AtomicBool) -> Options<'a> {
    Options {
        source,
        filter: Filter {
            engine,
            sealer: None,
            quarantine_secs: 3_600,
        },
        stop,
        limit: None,
        now,
    }
}

fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

fn job(store: &Store, id: i64) -> ImportJob {
    store.import_job(id).unwrap().unwrap()
}

/// Resumes a paused, blocked or interrupted job against `a`.
fn resume(store: &mut Store, id: i64, a: &Adapter) {
    let j = job(store, id);
    let (dataset, schema_sig) = a.identity();
    let j = store
        .resume_import_job(
            id,
            j.revision,
            &Revalidation {
                dataset: &dataset,
                schema_sig: &schema_sig,
                credential_ok: true,
                space_ok: true,
            },
            NOW,
        )
        .unwrap();
    assert_eq!(j.control, Control::Running);
}

#[test]
fn a_magnetico_source_imports_through_the_filter() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "a.sqlite3", &rows(1..=ROWS));
    let (mut store, source) = mem();
    let mut engine = engine();
    // A hash on the floor's list: refused, and tombstoned.
    engine.core.cs.add_hash(hash(7), Origin::Confirmed);
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!(r.ended, Ended::Exhausted);
    assert_eq!((r.batches, r.records), (3, u64::from(ROWS)));
    let c = r.counters;
    assert_eq!(
        (c.read, c.new, c.filtered),
        (u64::from(ROWS), u64::from(ROWS) - 1, 1)
    );
    assert!(c.conserved());
    assert_eq!(job(&store, id).counters, c);
    assert_eq!(job(&store, id).phase, Phase::Indexing);
    assert_eq!(store.count().unwrap(), u64::from(ROWS) - 1);
    assert!(
        store
            .find(&lodestar_metainfo::InfoHash(hash(7)))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .tombstones(10)
            .unwrap()
            .iter()
            .any(|t| t.hash == hash(7))
    );
    let id1 = store
        .find(&lodestar_metainfo::InfoHash(hash(1)))
        .unwrap()
        .unwrap();
    let row = store.torrent(id1).unwrap().unwrap();
    assert_ne!(row.flags & flags::IMPORTED, 0);
    assert_eq!(
        row.sources,
        1u64 << source.get(),
        "the adapter's source only"
    );
    let q = store.import_quality(id1).unwrap().unwrap();
    assert_eq!(
        (q.completeness, q.verification, q.privacy),
        (
            Completeness::ReportedComplete,
            Verification::Unverified,
            Privacy::Unknown
        )
    );
    assert!(
        store.import_job_members(id, 0, 1).unwrap()[0].1,
        "provenance: the job inserted it"
    );

    // A tombstoned hash stays gone, even when the filter no longer holds
    // it: another source with the same torrent can't bring it back.
    let path = fixture(dir.path(), "b.sqlite3", &rows([7, 1].into_iter()));
    let mut b = adapter(&path);
    let id = create(&mut store, &b);
    let r = block_on(run(
        &mut store,
        id,
        &mut b,
        &opts(source, &self::engine(), &stop),
    ))
    .unwrap();
    let c = r.counters;
    assert_eq!((c.read, c.filtered, c.merged, c.new), (2, 1, 1, 0));
    assert!(
        store
            .find(&lodestar_metainfo::InfoHash(hash(7)))
            .unwrap()
            .is_none()
    );
}

/// A clean run's end state, to compare crashed runs with.
fn clean(path: &Path) -> (ImportCounters, u64) {
    let (mut store, source) = mem();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    (r.counters, store.count().unwrap())
}

#[test]
fn a_crash_anywhere_replays_without_counting_twice() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "a.sqlite3", &rows(1..=ROWS));
    let want = clean(&path);
    for fault in [
        Fault::AfterRead,
        Fault::MidBatch,
        Fault::BeforeCommit,
        Fault::AfterCommit,
    ] {
        let (mut store, source) = mem();
        let engine = engine();
        let stop = AtomicBool::new(false);
        let mut a = adapter(&path);
        let id = create(&mut store, &a);
        CRASH.with(|c| c.set(Some((fault, 2))));
        let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
        CRASH.with(|c| c.set(None));
        assert!(
            matches!(r, Err(DriverError::Crash(f)) if f == fault),
            "{r:?}"
        );
        let committed = if fault == Fault::AfterCommit { 2 } else { 1 };
        assert_eq!(job(&store, id).seq, committed, "{fault:?}");
        assert!(job(&store, id).counters.conserved());
        // The restart: running jobs are interrupted, then resumed after
        // their checks, from the committed cursor.
        assert_eq!(store.interrupt_import_jobs(NOW).unwrap(), 1);
        let mut a = adapter(&path);
        resume(&mut store, id, &a);
        let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
        assert_eq!(r.ended, Ended::Exhausted, "{fault:?}");
        assert_eq!((r.counters, store.count().unwrap()), want, "{fault:?}");
    }
}

#[test]
fn stopping_pauses_with_exactly_what_was_committed() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "a.sqlite3", &rows(1..=ROWS));
    let (mut store, source) = mem();
    let engine = engine();
    let stop = AtomicBool::new(true);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!((r.ended, r.batches), (Ended::Paused, 0));
    assert_eq!(job(&store, id).control, Control::Paused);

    // A paused job doesn't run until it's resumed.
    stop.store(false, Ordering::Relaxed);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!(r.ended, Ended::NotRunning(Control::Paused));

    // The limit pauses after the batch that reaches it.
    resume(&mut store, id, &a);
    let o = Options {
        limit: Some(400),
        ..opts(source, &engine, &stop)
    };
    let r = block_on(run(&mut store, id, &mut a, &o)).unwrap();
    assert_eq!((r.ended, r.records), (Ended::Paused, 500));
    let j = job(&store, id);
    assert_eq!(
        (j.control, j.counters.read, j.seq),
        (Control::Paused, 500, 1)
    );
    assert_eq!(store.count().unwrap(), 500);

    // Cancelling the adapter's read pauses too, with nothing half-done.
    resume(&mut store, id, &a);
    a.canceller().store(true, Ordering::Relaxed);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!(r.ended, Ended::Paused);
    assert_eq!(job(&store, id).counters.read, 500);

    // Resumed with a fresh adapter, it finishes from where it stopped.
    let mut a = adapter(&path);
    resume(&mut store, id, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!((r.ended, r.records), (Ended::Exhausted, 700));
    assert_eq!(r.counters.read, u64::from(ROWS));
    assert_eq!(store.count().unwrap(), u64::from(ROWS));

    // Cancel keeps what was committed; a cancelled job never runs again.
    let j = job(&store, id);
    store.cancel_import_job(id, j.revision, NOW).unwrap();
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
    assert!(matches!(r, Err(DriverError::Job(_))), "{r:?}");
    assert_eq!(store.count().unwrap(), u64::from(ROWS));
}

#[test]
fn another_source_blocks_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let a_path = fixture(dir.path(), "a.sqlite3", &rows(1..=3));
    let b_path = fixture(dir.path(), "b.sqlite3", &rows(4..=7));
    let (mut store, source) = mem();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let a = adapter(&a_path);
    let id = create(&mut store, &a);
    let mut b = adapter(&b_path);
    let r = block_on(run(&mut store, id, &mut b, &opts(source, &engine, &stop))).unwrap();
    assert_eq!(r.ended, Ended::Blocked(Reason::SourceChanged));
    let j = job(&store, id);
    assert_eq!(
        (j.control, j.reason),
        (Control::Blocked, Some(Reason::SourceChanged))
    );
    assert_eq!(store.count().unwrap(), 0, "nothing was read");
}

#[test]
fn an_import_never_overwrites_a_verified_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut source_rows = rows(1..=2);
    source_rows[0].name = Value::Text("Imported name".into());
    let path = fixture(dir.path(), "a.sqlite3", &source_rows);
    let (mut store, source) = mem();
    // The crawler fetched torrent 1 from the network first, verified.
    let info = lodestar_metainfo::Info {
        name: lodestar_metainfo::Text {
            raw: b"Network name".to_vec(),
            encoding: lodestar_metainfo::TextEncoding::UTF8,
        },
        ..magnetico::TorrentRecord {
            hash: hash(1),
            name: Vec::new(),
            size: 7,
            source_at: None,
            files: Vec::new(),
            completeness: Completeness::ReportedComplete,
            repair: 0,
            files_dropped: 0,
        }
        .info()
    };
    let p = store.preparer().prepare(&info, None).unwrap();
    let dht = SourceId::new(0).unwrap();
    store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: NOW - 10,
                metadata_at: NOW - 10,
                extra_flags: 0,
                source: dht,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!((r.counters.merged, r.counters.new), (1, 1));
    let tid = store
        .find(&lodestar_metainfo::InfoHash(hash(1)))
        .unwrap()
        .unwrap();
    let row = store.torrent(tid).unwrap().unwrap();
    assert_eq!(row.name, "Network name");
    assert_eq!(row.flags & flags::IMPORTED, 0);
    assert_eq!(row.discovered_at, NOW - 10);
    let q = store.import_quality(tid).unwrap().unwrap();
    assert_eq!(q.verification, Verification::Verified);
    assert_eq!(
        store.import_job_members(id, 0, 10).unwrap()[0],
        (tid, false),
        "provenance only"
    );
}

#[test]
fn a_pilot_can_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "a.sqlite3", &rows(1..=ROWS));
    let (mut store, source) = mem();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let o = Options {
        limit: Some(1),
        ..opts(source, &engine, &stop)
    };
    let r = block_on(run(&mut store, id, &mut a, &o)).unwrap();
    assert_eq!((r.ended, store.count().unwrap()), (Ended::Paused, 500));
    let j = job(&store, id);
    store.cancel_import_job(id, j.revision, NOW).unwrap();
    let mut removed = Removed::default();
    while !removed.done {
        removed.add(store.remove_import_job_records(id, 200).unwrap());
    }
    assert_eq!(removed.torrents, 500);
    assert_eq!(store.count().unwrap(), 0);
}
