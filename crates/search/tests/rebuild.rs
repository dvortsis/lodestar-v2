//! Rebuilding the index for a new version while the old one serves, the
//! swap, resuming an interrupted build, and the root's lock.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{
    Error, IndexVersion, LiveConfig, Rebuild, SearchIndex, Serving, ServingConfig,
};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

const OLD: IndexVersion = IndexVersion {
    schema: 1,
    tokenizer: 1,
};
const NEW: IndexVersion = IndexVersion {
    schema: 2,
    tokenizer: 1,
};
const NOW: i64 = 1_700_000_100;
const HEAP: usize = 15 << 20;

fn d(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &str) -> OwnedValue {
    OwnedValue::Bytes(s.as_bytes().to_vec())
}

fn torrent(name: &str) -> Vec<u8> {
    let file = d(vec![
        ("length", OwnedValue::Int(2_000_000_000)),
        ("path", OwnedValue::List(vec![b(&format!("{name}.mkv"))])),
    ]);
    to_vec(&d(vec![
        ("files", OwnedValue::List(vec![file])),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

fn add(store: &mut Store, names: impl IntoIterator<Item = String>) {
    let raws: Vec<Vec<u8>> = names.into_iter().map(|n| torrent(&n)).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
}

fn movies(range: std::ops::Range<usize>) -> impl Iterator<Item = String> {
    range.map(|i| format!("Common.Movie.{i}.2010.1080p.BluRay.x264-GRP"))
}

/// A store of `n` torrents, and its writer connection.
fn store(dir: &Path, n: usize) -> Store {
    let mut store = Store::open(dir.join("s.db"), StoreConfig::default()).unwrap();
    add(&mut store, movies(0..n));
    store
}

/// A complete index of the store under `root`, at `version`.
fn build_complete(root: &Path, version: IndexVersion, store: &mut Store) {
    let index = SearchIndex::open(&root.join(version.dir_name())).unwrap();
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
}

fn config(version: IndexVersion) -> ServingConfig {
    ServingConfig {
        live: LiveConfig {
            heap_bytes: HEAP,
            poll: Duration::from_millis(20),
            commit_every: Duration::from_millis(100),
            batch: 50,
            ..LiveConfig::default()
        },
        rebuild_commit_docs: 200,
        version,
        ..ServingConfig::default()
    }
}

fn total(index: &SearchIndex, q: &str) -> usize {
    let node = lodestar_search::parse(q).unwrap();
    index.search(&index.query(&node, NOW), 10).unwrap().total
}

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn names_in(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".lock")
        .collect();
    v.sort();
    v
}

#[test]
fn searches_keep_working_through_a_rebuild_and_the_swap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 3000;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);

    let serving =
        Arc::new(Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config(NEW)).unwrap());
    let p = serving.progress().expect("rebuilding");
    assert_eq!((p.from, p.to, p.total), (OLD, NEW, n as u64));

    let stop = Arc::new(AtomicBool::new(false));
    let searchers: Vec<_> = (0..3)
        .map(|_| {
            let (serving, stop) = (serving.clone(), stop.clone());
            std::thread::spawn(move || {
                let (mut during, mut after) = (0, 0);
                while !stop.load(Ordering::Relaxed) {
                    let rebuilding = serving.rebuilding();
                    // Every answer, from either index, has every torrent.
                    assert_eq!(total(&serving.index(), "common movie"), n);
                    if rebuilding { during += 1 } else { after += 1 }
                }
                (during, after)
            })
        })
        .collect();
    wait("the rebuild finishes", || !serving.rebuilding());
    // Some searches run against the new index too.
    std::thread::sleep(Duration::from_millis(50));
    stop.store(true, Ordering::Relaxed);
    for s in searchers {
        let (during, after) = s.join().unwrap();
        assert!(during > 0 && after > 0, "{during} during, {after} after");
    }
    let index = serving.index();
    assert_eq!(index.num_docs(), n as u64);
    assert_eq!(index.watermark().unwrap(), n as i64);
    assert_eq!(index.files_watermark().unwrap(), n as i64);
    wait("the old index is deleted", || {
        names_in(&root) == vec![NEW.dir_name()]
    });
}

#[test]
fn an_interrupted_rebuild_resumes_from_its_watermarks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 2000;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);

    let mut reader = Store::open_reader(dir.path().join("s.db"), StoreConfig::default()).unwrap();
    let mut b = Rebuild::open(&root, NEW, HEAP).unwrap();
    for _ in 0..10 {
        b.step(&mut reader, 100).unwrap();
    }
    b.commit().unwrap();
    // Uncommitted work is lost in the crash.
    b.step(&mut reader, 100).unwrap();
    drop(b);
    let building = root.join(format!("{}.building", NEW.dir_name()));
    assert!(building.exists());

    let serving = Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config(NEW)).unwrap();
    let p = serving.progress().unwrap();
    assert_eq!(p.done, 1000, "resumed, not started over");
    wait("the rebuild finishes", || !serving.rebuilding());
    let index = serving.index();
    assert_eq!(index.num_docs(), reader.count().unwrap());
    assert_eq!(total(&index, "common movie"), n);
    assert!(!building.exists());
    assert!(root.join(NEW.dir_name()).exists());
}

#[test]
fn a_build_marked_complete_only_needs_its_rename() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let mut writer = store(dir.path(), 300);
    build_complete(&root, OLD, &mut writer);
    // Built and marked, then the crash before the rename.
    let building = root.join(format!("{}.building", NEW.dir_name()));
    build_complete(
        &root,
        IndexVersion {
            schema: 2,
            tokenizer: 0,
        },
        &mut writer,
    );
    std::fs::rename(root.join("v2-t0"), &building).unwrap();
    std::fs::write(building.join("COMPLETE"), "x").unwrap();

    let serving = Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config(NEW)).unwrap();
    assert!(!serving.rebuilding());
    assert_eq!(serving.index().num_docs(), 300);
    drop(serving);
    assert_eq!(names_in(&root), vec![NEW.dir_name()]);
}

#[test]
fn after_the_swap_new_torrents_go_to_the_new_index_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 500;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);

    let serving = Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config(NEW)).unwrap();
    let old = serving.index();
    // The old index follows the store while it serves.
    add(&mut writer, ["During.The.Rebuild.2011".to_string()]);
    wait("the old index takes new torrents", || {
        total(&old, "during rebuild") == 1
    });
    wait("the rebuild finishes", || !serving.rebuilding());
    let new = serving.index();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(total(&new, "during rebuild"), 1);

    add(&mut writer, ["After.The.Swap.2012".to_string()]);
    wait("the new index takes new torrents", || {
        total(&serving.index(), "after swap") == 1
    });
    // Two more commit intervals: the old index would have it by now.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(old.num_docs(), n as u64 + 1);
    assert_eq!(total(&old, "after swap"), 0);
    assert_eq!(
        serving.stats().watermark.load(Ordering::Relaxed),
        n as i64 + 2
    );
    serving.shutdown().unwrap();
    assert_eq!(names_in(&root), vec![NEW.dir_name()]);
}

#[test]
fn a_second_handle_on_the_same_root_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let mut writer = store(dir.path(), 300);
    build_complete(&root, OLD, &mut writer);
    let db = dir.path().join("s.db");
    let first = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
    match Serving::open_or_rebuild(&root, &db, config(NEW)) {
        Err(Error::Locked(r)) => assert_eq!(r, root),
        other => panic!("expected Locked, got {other:?}"),
    }
    // The first is untouched by the attempt.
    wait("the rebuild finishes", || !first.rebuilding());
    assert_eq!(first.index().num_docs(), 300);
    first.shutdown().unwrap();
    let again = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
    assert!(!again.rebuilding());
    assert_eq!(again.index().num_docs(), 300);
}

#[test]
fn reindex_entries_logged_during_the_rebuild_are_never_lost() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 600;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);
    let enriched = |index: &SearchIndex| total(index, "res:1080p");

    // Half built, then the backfill enriches torrents on both sides of
    // the build's watermark and logs them for reindexing.
    let mut reader = Store::open_reader(dir.path().join("s.db"), StoreConfig::default()).unwrap();
    let mut b = Rebuild::open(&root, NEW, HEAP).unwrap();
    for _ in 0..3 {
        b.step(&mut reader, 100).unwrap();
    }
    b.commit().unwrap();
    drop(b);
    let (_, done) = lodestar_pipeline::enrich::enrich_stored(&mut writer, 0, 400)
        .unwrap()
        .unwrap();
    assert_eq!(done, 400);

    let serving = Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config(NEW)).unwrap();
    // Logged before the swap: the old index applies them while serving,
    // and the new one before it's swapped in.
    wait("the old index applies the log", || {
        enriched(&serving.index()) == 400
    });
    wait("the rebuild finishes", || !serving.rebuilding());
    assert_eq!(enriched(&serving.index()), 400);
    assert_eq!(
        serving.index().num_docs(),
        n as u64,
        "replaced, not duplicated"
    );

    // Logged after the swap: the live indexer applies them.
    lodestar_pipeline::enrich::enrich_stored(&mut writer, 400, 1000)
        .unwrap()
        .unwrap();
    wait("the live indexer applies the log", || {
        enriched(&serving.index()) == n
    });
    assert_eq!(serving.index().num_docs(), n as u64);
}

#[test]
fn a_new_install_or_a_current_index_needs_no_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let mut writer = store(dir.path(), 100);
    let db = dir.path().join("s.db");
    {
        // Nothing to serve meanwhile: this version fills live.
        let serving = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
        assert!(!serving.rebuilding());
        wait("the live indexer fills it", || {
            serving.index().num_docs() == 100
        });
    }
    // Stale directories go: an older index, another version's partial
    // build, trash. A newer version stays for a downgrade's sake.
    build_complete(&root, OLD, &mut writer);
    for stale in ["v1-t1.building", "v1-t0.trash", "v9-t9"] {
        std::fs::create_dir_all(root.join(stale)).unwrap();
    }
    let serving = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
    assert!(!serving.rebuilding());
    assert_eq!(serving.index().num_docs(), 100);
    wait("stale directories are deleted", || {
        names_in(&root) == vec![NEW.dir_name(), "v9-t9".to_string()]
    });
}
