//! Rebuilding the index for a new version while the old one serves, the
//! swap, resuming an interrupted build, and the root's lock.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{
    Error, IndexVersion, LiveConfig, Rebuild, RebuildPhase, SearchIndex, Serving, ServingConfig,
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

/// A config whose rebuild waits, once caught up, until the flag clears.
fn held(version: IndexVersion) -> (ServingConfig, Arc<AtomicBool>) {
    let hold = Arc::new(AtomicBool::new(true));
    let config = ServingConfig {
        hold_before_finish: Some(hold.clone()),
        ..config(version)
    };
    (config, hold)
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

    let (config, hold) = held(NEW);
    let serving =
        Arc::new(Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config).unwrap());
    let p = serving.progress().expect("rebuilding");
    assert_eq!((p.from, p.to, p.total), (OLD, NEW, n as u64));
    assert!(p.unavailable.is_empty(), "same schema");

    let stop = Arc::new(AtomicBool::new(false));
    // Searches each thread ran on the old index and on the new one.
    let counts: Arc<[(AtomicUsize, AtomicUsize); 3]> = Arc::new(Default::default());
    let searchers: Vec<_> = (0..3)
        .map(|i| {
            let (serving, stop, counts) = (serving.clone(), stop.clone(), counts.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let rebuilding = serving.rebuilding();
                    // Every answer, from either index, has every torrent.
                    assert_eq!(total(&serving.index(), "common movie"), n);
                    let c = if rebuilding {
                        &counts[i].0
                    } else {
                        &counts[i].1
                    };
                    c.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    let all = |pick: fn(&(AtomicUsize, AtomicUsize)) -> &AtomicUsize| {
        counts.iter().all(|c| pick(c).load(Ordering::Relaxed) > 0)
    };
    wait("searches run during the rebuild", || all(|c| &c.0));
    assert!(serving.rebuilding());
    hold.store(false, Ordering::Relaxed);
    // Fails at once, saying why, if the rebuild stopped on an error
    // rather than running out the wait.
    wait("the rebuild finishes", || match serving.progress() {
        None => true,
        Some(p) => {
            assert_ne!(p.phase, RebuildPhase::Failed, "the rebuild failed: {p:?}");
            false
        }
    });
    wait("searches run after the swap", || all(|c| &c.1));
    stop.store(true, Ordering::Relaxed);
    for s in searchers {
        s.join().unwrap();
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
fn no_torrent_visible_before_the_swap_is_missing_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 1000;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);
    let (config, hold) = held(NEW);
    let serving =
        Arc::new(Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config).unwrap());

    // Torrents keep arriving through the swap; each search must see at
    // least what the one before it saw.
    let stop = Arc::new(AtomicBool::new(false));
    let adder = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut i = n;
            while !stop.load(Ordering::Relaxed) {
                add(&mut writer, movies(i..i + 5));
                i += 5;
                std::thread::sleep(Duration::from_millis(2));
            }
            i
        })
    };
    let searcher = {
        let (serving, stop) = (serving.clone(), stop.clone());
        std::thread::spawn(move || {
            let (mut seen, mut searches) = (0, 0);
            while !stop.load(Ordering::Relaxed) {
                let now = total(&serving.index(), "common movie");
                assert!(now >= seen, "{now} visible after {seen}");
                seen = now;
                searches += 1;
            }
            searches
        })
    };
    // The old index takes new torrents while it serves.
    wait("the old index follows the store", || {
        total(&serving.index(), "common movie") > n
    });
    hold.store(false, Ordering::Relaxed);
    wait("the rebuild finishes", || !serving.rebuilding());
    let at_swap = total(&serving.index(), "common movie");
    wait("the new index follows the store", || {
        total(&serving.index(), "common movie") > at_swap
    });
    stop.store(true, Ordering::Relaxed);
    let added = adder.join().unwrap();
    assert!(searcher.join().unwrap() > 0);
    wait("everything is indexed", || {
        total(&serving.index(), "common movie") == added
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
        assert_eq!(b.step(&mut reader, 100).unwrap().added, 100);
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
fn a_stopped_rebuild_resumes_at_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 800;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);
    let db = dir.path().join("s.db");

    let (config, _hold) = held(NEW);
    let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
    wait("the build catches up", || {
        serving.progress().is_some_and(|p| p.done == n as u64)
    });
    // Stopping commits the build; the old index still serves.
    serving.shutdown().unwrap();
    assert!(root.join(OLD.dir_name()).exists());

    let (config, hold) = held(NEW);
    let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
    let p = serving.progress().unwrap();
    assert_eq!(
        (p.done, p.total),
        (n as u64, n as u64),
        "nothing rebuilt twice"
    );
    hold.store(false, Ordering::Relaxed);
    wait("the rebuild finishes", || !serving.rebuilding());
    assert_eq!(serving.index().num_docs(), n as u64);
}

#[test]
fn a_failed_rebuild_keeps_the_old_index_serving() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let n = 300;
    let mut writer = store(dir.path(), n);
    build_complete(&root, OLD, &mut writer);
    let db = dir.path().join("s.db");

    let (cfg, hold) = held(NEW);
    let serving = Serving::open_or_rebuild(&root, &db, cfg).unwrap();
    wait("the build catches up", || {
        serving.progress().is_some_and(|p| p.done == n as u64)
    });
    // Something in the way of the rename.
    // A non-empty directory: every platform refuses to rename onto one
    // (Windows replaces a plain file).
    let blocker = root.join(NEW.dir_name());
    std::fs::create_dir_all(blocker.join("in-the-way")).unwrap();
    hold.store(false, Ordering::Relaxed);
    wait("the rebuild fails", || {
        serving
            .progress()
            .is_some_and(|p| p.phase == RebuildPhase::Failed)
    });
    // The failure says why, for the banner and for tests.
    let failed = serving.progress().expect("still rebuilding");
    assert!(
        failed.error.as_deref().is_some_and(|e| !e.is_empty()),
        "a failed rebuild carries its error: {failed:?}"
    );
    let old = serving.index();
    assert_eq!(total(&old, "common movie"), n);
    // The old index still follows the store.
    add(&mut writer, ["Still.Followed.2013".to_string()]);
    wait("the old index takes new torrents", || {
        total(&serving.index(), "still followed") == 1
    });
    drop(old);
    serving.shutdown().unwrap();

    // Out of the way, the next start only needs the rename: the build
    // was marked complete.
    std::fs::remove_dir_all(&blocker).unwrap();
    let serving = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
    assert!(!serving.rebuilding());
    wait("it catches up", || {
        total(&serving.index(), "common movie | still followed") == n + 1
    });
}

#[test]
fn an_older_index_that_fails_to_open_fails_the_start_and_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let mut writer = store(dir.path(), 100);
    build_complete(&root, OLD, &mut writer);
    let meta = root.join(OLD.dir_name()).join("meta.json");
    let good = std::fs::read(&meta).unwrap();
    std::fs::write(&meta, b"{ not json").unwrap();

    let db = dir.path().join("s.db");
    assert!(Serving::open_or_rebuild(&root, &db, config(NEW)).is_err());
    assert_eq!(names_in(&root), vec![OLD.dir_name()], "nothing discarded");
    // Readable again, it serves.
    std::fs::write(&meta, good).unwrap();
    let serving = Serving::open_or_rebuild(&root, &db, config(NEW)).unwrap();
    assert_eq!(serving.progress().unwrap().from, OLD);
    assert_eq!(total(&serving.index(), "common movie"), 100);
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

    let (config, hold) = held(NEW);
    let serving = Serving::open_or_rebuild(&root, &dir.path().join("s.db"), config).unwrap();
    let old = serving.index();
    // The old index follows the store while it serves.
    add(&mut writer, ["During.The.Rebuild.2011".to_string()]);
    wait("the old index takes new torrents", || {
        total(&old, "during rebuild") == 1
    });
    hold.store(false, Ordering::Relaxed);
    wait("the rebuild finishes", || !serving.rebuilding());
    let new = serving.index();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(total(&new, "during rebuild"), 1);
    drop(new);

    // The old indexer stopped before the swap, so this can only reach
    // the new index.
    add(&mut writer, ["After.The.Swap.2012".to_string()]);
    wait("the new index takes new torrents", || {
        total(&serving.index(), "after swap") == 1
    });
    assert_eq!(old.num_docs(), n as u64 + 1);
    assert_eq!(total(&old, "after swap"), 0);
    assert_eq!(
        serving.stats().watermark.load(Ordering::Relaxed),
        n as i64 + 2
    );
    // Unmapped, the old directory can go (Windows needs that).
    drop(old);
    wait("the old index is deleted", || {
        names_in(&root) == vec![NEW.dir_name()]
    });
    serving.shutdown().unwrap();
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
