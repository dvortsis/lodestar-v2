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
    fixture_in(dir, name, Layout::V3, rows)
}

fn fixture_in(dir: &Path, name: &str, layout: Layout, rows: &[TorrentRow]) -> PathBuf {
    let path = dir.join(name);
    build(&path, layout, rows);
    path
}

fn adapter(path: &Path) -> Adapter {
    Adapter::Magnetico(open(path, &OpenPolicy::default()).unwrap())
}

fn mem() -> (Store, SourceId) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
    assert_eq!(
        store.class_gen().unwrap(),
        0,
        "the generation opts() passes"
    );
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
            class_gen: 0,
        },
        stop,
        limit: None,
        now,
        progress: None,
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
fn clean(path: &Path) -> (ImportCounters, u64, Vec<String>) {
    let (mut store, source) = mem();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    (r.counters, store.count().unwrap(), provenance_rows(&store))
}

/// Every provenance and quality row, in order: a replay must leave the
/// same ones as a clean run.
fn provenance_rows(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT torrent_id || ',' || job || ',' || created FROM import_membership
         ORDER BY torrent_id, job",
        "SELECT torrent_id || ',' || completeness || ',' || verification || ',' || privacy
             || ',' || repair || ',' || coalesce(source_at, '') FROM import_quality
         ORDER BY torrent_id",
    ] {
        let mut stmt = store.connection().prepare(sql).unwrap();
        out.extend(
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap),
        );
    }
    out
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
        assert_eq!(
            (r.counters, store.count().unwrap(), provenance_rows(&store)),
            want,
            "{fault:?}"
        );
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

fn named(seed: u32, name: &str) -> TorrentRow {
    TorrentRow {
        name: Value::Text(name.into()),
        ..TorrentRow::good(seed, &[("d/a", 3)])
    }
}

/// Your rules judge each record's name, as at ingest: a drop leaves a
/// tombstone, a quarantine a sealed name, a flag a mark, a shadow rule a
/// logged hit only; a candidate is judged by its name too.
#[test]
fn rules_judge_names_and_candidates_as_at_ingest() {
    use lodestar_filter::{Action, Sealer};
    let dir = tempfile::tempdir().unwrap();
    let mut nameless_size = named(6, "dropme candidate");
    nameless_size.total_size = Value::Integer(0);
    let path = fixture_in(
        dir.path(),
        "a.sqlite3",
        Layout::Lax,
        &[
            named(1, "alpha dropme"),
            named(2, "beta quarantineme"),
            named(3, "gamma flagme"),
            named(4, "delta shadowme"),
            named(5, "epsilon"),
            nameless_size,
        ],
    );
    let (mut store, source) = mem();
    for (word, action, shadow) in [
        ("dropme", Action::Drop, None),
        ("quarantineme", Action::Quarantine, None),
        ("flagme", Action::Flag, None),
        ("shadowme", Action::Drop, Some(NOW + 3_600)),
    ] {
        store.add_rule(word, action.code(), shadow, None).unwrap();
    }
    let mut engine = engine();
    assert_eq!(
        engine.load_rules(&store.rule_set().unwrap()),
        Vec::<String>::new()
    );
    let sealer = Sealer::new(&[7; 32]);
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let mut o = opts(source, &engine, &stop);
    o.filter.sealer = Some(&sealer);
    let r = block_on(run(&mut store, id, &mut a, &o)).unwrap();
    let c = r.counters;
    assert_eq!(
        (c.read, c.new, c.filtered, c.quarantined, c.candidates),
        (6, 3, 2, 1, 0),
        "{c:?}"
    );
    let stored = |seed| {
        store
            .find(&lodestar_metainfo::InfoHash(hash(seed)))
            .unwrap()
    };
    assert!(stored(1).is_none() && stored(2).is_none() && stored(6).is_none());
    let tombstoned: Vec<[u8; 20]> = store
        .tombstones(10)
        .unwrap()
        .iter()
        .map(|t| t.hash)
        .collect();
    assert!(tombstoned.contains(&hash(1)) && tombstoned.contains(&hash(6)));
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].hash, hash(2));
    assert_eq!(
        sealer.open(q[0].sealed.as_deref().unwrap()).unwrap(),
        "beta quarantineme"
    );
    let flagged = store.torrent(stored(3).unwrap()).unwrap().unwrap();
    assert_ne!(flagged.flags & flags::FLAGGED, 0);
    let shadowed = store.torrent(stored(4).unwrap()).unwrap().unwrap();
    assert_eq!(
        shadowed.flags & (flags::FLAGGED | flags::HIDDEN),
        0,
        "shadow only logs"
    );
    let hits = store.filter_hits(0).unwrap();
    let shadow_hit = hits.iter().find(|h| h.hash == hash(4)).unwrap();
    assert_ne!(shadow_hit.action & lodestar_store::SHADOW, 0);
    assert!(
        hits.iter()
            .any(|h| h.hash == hash(3) && h.action == Action::Flag.code())
    );
    assert!(store.import_quality(stored(5).unwrap()).unwrap().is_some());
}

/// Changes made elsewhere between batches: a job paused by another
/// connection commits nothing more, and a source written mid-import
/// blocks the job, each with exactly the committed batches kept.
#[test]
fn changes_between_batches_stop_the_run_truthfully() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "a.sqlite3", &rows(1..=ROWS));
    let db = dir.path().join("lodestar.db");
    let engine = engine();
    let stop = AtomicBool::new(false);

    // Paused by another connection while batch 2 is judged.
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let other = db.clone();
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |at, n| {
            if (at, n) == (Fault::BeforeCommit, 2) {
                let mut s = Store::open(&other, StoreConfig::default()).unwrap();
                let j = s.import_job(id).unwrap().unwrap();
                s.pause_import_job(id, j.revision, NOW).unwrap();
            }
        }))
    });
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
    HOOK.with(|h| *h.borrow_mut() = None);
    let r = r.unwrap();
    assert_eq!(r.ended, Ended::NotRunning(Control::Paused));
    assert_eq!((r.batches, r.counters.read), (1, 500));
    assert_eq!(job(&store, id).seq, 1);
    assert_eq!(store.count().unwrap(), 500);

    // The source is written after batch 1 commits: blocked, not fed a
    // moving source.
    resume(&mut store, id, &a);
    let src = path.clone();
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |at, n| {
            if (at, n) == (Fault::AfterCommit, 1) {
                let mut conn = rusqlite::Connection::open(&src).unwrap();
                crate::magnetico::fixtures::insert(&mut conn, &rows(ROWS + 1..=ROWS + 1));
                conn.close().unwrap();
            }
        }))
    });
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
    HOOK.with(|h| *h.borrow_mut() = None);
    let r = r.unwrap();
    assert!(matches!(r.ended, Ended::Blocked(_)), "{:?}", r.ended);
    assert!(r.refusal.is_some());
    let j = job(&store, id);
    assert_eq!(
        (j.control, j.seq, j.counters.read),
        (Control::Blocked, 2, 1_000)
    );
    assert!(j.counters.conserved());
}

/// The pilots' pass line (IMP-4b decisions file): 100,000 synthetic
/// Magnetico torrents, three files each, into a file-backed store through
/// the full filter. Run it in release, on its own:
/// `cargo test --release -p lodestar-sources-importers -- --ignored throughput --nocapture`.
#[test]
#[ignore = "a measurement: run in release, see the decisions file"]
fn throughput_on_a_synthetic_100k_source() {
    const N: u32 = 100_000;
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<TorrentRow> = (1..=N)
        .map(|i| TorrentRow {
            name: Value::Text(format!(
                "Some.Show.S01E{:02}.1080p.WEB.h264-GROUP {i}",
                i % 24
            )),
            ..TorrentRow::good(
                i,
                &[
                    ("Show/ep.mkv", 1_000_000),
                    ("Show/ep.nfo", 900),
                    ("Show/Sample/s.mkv", 50_000),
                ],
            )
        })
        .collect();
    let path = fixture(dir.path(), "a.sqlite3", &rows);
    let source_bytes = std::fs::metadata(&path).unwrap().len();
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let started = std::time::Instant::now();
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    let secs = started.elapsed().as_secs_f64();
    store.checkpoint().unwrap();
    assert_eq!((r.ended, r.counters.new), (Ended::Exhausted, u64::from(N)));
    let store_bytes = std::fs::metadata(&db).unwrap().len();
    println!(
        "throughput: {N} torrents in {secs:.1} s = {:.0}/s; source {} MiB, store {} MiB = {} B a torrent",
        f64::from(N) / secs,
        source_bytes >> 20,
        store_bytes >> 20,
        store_bytes / u64::from(N)
    );
}

/// The job paused or cancelled elsewhere just as the source ends or is
/// refused: the run reports it as not running, not as a store fault
/// (runner, #253 review).
#[test]
fn a_job_stopped_elsewhere_at_the_end_is_reported_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine();
    let stop = AtomicBool::new(false);
    for refused in [false, true] {
        let path = fixture(dir.path(), &format!("s{refused}.sqlite3"), &rows_n(600));
        let db = dir.path().join(format!("l{refused}.db"));
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
        let mut a = adapter(&path);
        let id = create(&mut store, &a);
        let (other, src) = (db.clone(), path.clone());
        // After the last batch (no more rows), or after the first one with
        // the source then written to (refused at the next read).
        let at = if refused { 1 } else { 2 };
        HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |f, n| {
                if (f, n) == (Fault::AfterCommit, at) {
                    let mut s = Store::open(&other, StoreConfig::default()).unwrap();
                    let j = s.import_job(id).unwrap().unwrap();
                    if refused {
                        s.pause_import_job(id, j.revision, NOW).unwrap();
                        let mut conn = rusqlite::Connection::open(&src).unwrap();
                        crate::magnetico::fixtures::insert(&mut conn, &rows_n(601)[600..]);
                        conn.close().unwrap();
                    } else {
                        s.cancel_import_job(id, j.revision, NOW).unwrap();
                    }
                }
            }))
        });
        let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
        HOOK.with(|h| *h.borrow_mut() = None);
        let r = r.unwrap();
        let want = if refused {
            Control::Paused
        } else {
            Control::Cancelled
        };
        assert_eq!(r.ended, Ended::NotRunning(want), "refused={refused}");
        assert_eq!(r.refusal.is_some(), refused, "the path taken");
        assert_eq!(r.batches, at);
        assert_eq!(r.counters, job(&store, id).counters);
    }
}

fn rows_n(n: u32) -> Vec<TorrentRow> {
    rows(1..=n)
}

/// A child-role and a sexual stand-in ten words apart: only the Adult-only
/// CS-5a (window 12) catches them, and only on an adult item.
const SPREAD: &str = "Studiox Rolea one two three four five six seven eight nine Lexa";

fn stand_in() -> Engine {
    Engine::new(Core::new(
        Lists {
            sexual: lodestar_filter::TermSet::of(&["lexa"]),
            child_roles: lodestar_filter::TermSet::of(&["rolea"]),
            codes: lodestar_filter::TermSet::of(&["codex"]),
        },
        CsList::default(),
        Switches::default(),
    ))
}

/// An always-adult source ("sukebei") registered in `store`, with its mark
/// set on the connection as the CLI sets it.
fn adult_source(store: &mut Store) -> (SourceId, lodestar_pipeline::source_adult::AdultSources) {
    let s = store.register_source("sukebei", "1", NOW).unwrap();
    let mask = lodestar_pipeline::source_adult::AdultSources(1 << s.get());
    store.set_adult_mark(mask.store_mark());
    (s, mask)
}

/// The always-adult source's claims under `hash(seed)`, as an unverified
/// item.
fn claim(store: &mut Store, source: SourceId, seed: u32) {
    let c = lodestar_store::claims::Claims {
        name: Some(format!("claimed {seed}")),
        ..Default::default()
    };
    let put = store
        .put_unverified(&[lodestar_store::unverified::NewUnverified {
            network: lodestar_store::unverified::Network::Bt,
            key: &hash(seed),
            source,
            private_source: false,
            claims: Some(&c),
            at: NOW,
        }])
        .unwrap();
    assert!(matches!(put[0], lodestar_store::unverified::Put::New(_)));
}

fn adult_code() -> Option<u8> {
    Some(lodestar_classify::ContentType::Adult.code())
}

fn stored(store: &Store, seed: u32) -> Option<(i64, u64, Option<u8>)> {
    let id = store
        .find(&lodestar_metainfo::InfoHash(hash(seed)))
        .unwrap()?;
    let row = store.torrent(id).unwrap().unwrap();
    Some((id, row.sources, store.content_type(id).unwrap()))
}

/// A dump holding a hash only an always-adult source has claimed: the
/// record is stored Adult, with that source's bit, so the backfill keeps
/// it adult; it's judged as adult too, so the Adult-only core holds a name
/// only that reading meets. An unclaimed record isn't affected
/// (X-source-adult-2).
#[test]
fn a_dump_holding_a_hash_an_always_adult_source_claims_is_stored_adult() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        named(1, "Garden.Tour.2022.1080p"),
        named(2, SPREAD),
        named(3, SPREAD),
        named(4, "Lake.Walk.2021.1080p"),
    ];
    let path = fixture(dir.path(), "a.sqlite3", &rows);
    let (mut store, source) = mem();
    let (adult, mask) = adult_source(&mut store);
    claim(&mut store, adult, 1);
    claim(&mut store, adult, 2);
    let engine = stand_in();
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop))).unwrap();
    assert_eq!(r.ended, Ended::Exhausted);

    // Claimed: Adult with the source's bit, and not left for the backfill.
    let (t1, sources, class) = stored(&store, 1).expect("stored");
    assert_eq!(class, adult_code());
    assert!(mask.any(sources) && sources & (1 << source.get()) != 0);
    assert!(
        !store
            .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
            .unwrap()
            .contains(&t1)
    );
    // Claimed, and only the adult reading meets CS-5a: held.
    assert!(stored(&store, 2).is_none());
    assert_eq!(r.counters.quarantined, 1);
    // Unclaimed: the dump's own classification.
    let (_, sources, class) = stored(&store, 3).expect("unclaimed is stored");
    assert!(!mask.any(sources));
    assert_ne!(class, adult_code());
    assert_ne!(stored(&store, 4).unwrap().2, adult_code());

    // The backfill keeps it adult.
    let filter = lodestar_filter::Handle::new(stand_in());
    let ctx = lodestar_pipeline::enrich::Backfill {
        filter: Some(&filter),
        adult: mask,
        ..Default::default()
    };
    store.mark_unenriched(&[t1]).unwrap();
    while lodestar_pipeline::enrich::enrich_stored_with(&mut store, 0, 10, &ctx)
        .unwrap()
        .is_some()
    {}
    assert_eq!(stored(&store, 1).unwrap().2, adult_code());
}

/// The claim lands between the driver's read and the commit (another
/// connection, mid-run): the driver judged the record plain, but the
/// commit's own transaction gives the row the source's bit and marks it
/// for the backfill, which then classifies it Adult. Nothing between the
/// two steps can leave it un-adult with no retry.
#[test]
fn a_claim_landing_before_the_commit_is_marked_and_the_backfill_makes_it_adult() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(
        dir.path(),
        "a.sqlite3",
        &[named(1, "Garden.Tour.2022.1080p")],
    );
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let source = store.register_source(magnetico::ADAPTER, "1", NOW).unwrap();
    let (adult, mask) = adult_source(&mut store);
    let engine = engine();
    let stop = AtomicBool::new(false);
    let mut a = adapter(&path);
    let id = create(&mut store, &a);
    let other = db.clone();
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |at, n| {
            if (at, n) == (Fault::BeforeCommit, 1) {
                let mut s = Store::open(&other, StoreConfig::default()).unwrap();
                claim(&mut s, adult, 1);
            }
        }))
    });
    let r = block_on(run(&mut store, id, &mut a, &opts(source, &engine, &stop)));
    HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(r.unwrap().ended, Ended::Exhausted);

    // Stored as the driver judged it, with the bit and the retry mark.
    let (t, sources, class) = stored(&store, 1).expect("stored");
    assert_ne!(class, adult_code());
    assert!(mask.any(sources));
    assert!(
        store
            .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
            .unwrap()
            .contains(&t)
    );

    // The backfill (after a restart, say) finds the mark.
    let filter = lodestar_filter::Handle::new(engine);
    let ctx = lodestar_pipeline::enrich::Backfill {
        filter: Some(&filter),
        adult: mask,
        ..Default::default()
    };
    while lodestar_pipeline::enrich::enrich_stored_with(&mut store, 0, 10, &ctx)
        .unwrap()
        .is_some()
    {}
    assert_eq!(stored(&store, 1).unwrap().2, adult_code());
    assert!(
        !store
            .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
            .unwrap()
            .contains(&t)
    );
}
