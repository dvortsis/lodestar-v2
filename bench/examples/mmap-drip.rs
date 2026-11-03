//! `mmap-drip` (X-mmap-cache-prune, M1-exp): one-document commits at a
//! fixed pace through the real live indexer, measuring what each commit
//! leaves behind.
//!
//! Torrents from the bench generator are written to the store at `PER_SEC`;
//! the live indexer follows with `batch = 1` and `commit_docs = 1`, so each
//! torrent is one commit of both indexes and a reader reload. Arm `default`
//! leaves tantivy's mmap caches unpruned (the control; nothing here ever
//! prunes them); arm `prune` is the production prune after each commit.
//!
//! ```sh
//! # retained live allocations (dhat current bytes), gnu:
//! cargo run --release -p lodestar-bench --features dhat-heap --example mmap-drip -- prune 10 3600 OUT
//! # anonymous memory with the allocator `serve` uses (build for musl as the image does):
//! cargo run --release -p lodestar-bench --example mmap-drip -- default 10 3600 OUT
//! # production cadence (`serve`'s LiveConfig), ~665 torrents an hour:
//! cargo run --release -p lodestar-bench --example mmap-drip -- prune 0.1847 23400 OUT --prod
//! # commit and search latency with searches running, and the reclamation check:
//! cargo run --release -p lodestar-bench --example mmap-drip -- latency prune 3000
//! # a frozen fixture to start from (copy it per run), then `--from DIR`:
//! cargo run --release -p lodestar-bench --example mmap-drip -- fixture 9900 DIR
//! ```
//!
//! Prints one CSV row every 100 commits: commits, torrents indexed,
//! documents per commit so far, commits per second, the measure (dhat
//! current bytes, or `Anonymous` from smaps_rollup), the live segment files
//! of both indexes and the prune's entry counts (0 for `default`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::{LiveConfig, LiveIndexer, SearchIndex, parse};
use lodestar_store::{NewTorrent, Store, StoreConfig};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(not(any(feature = "dhat-heap", feature = "system-alloc")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Writes `n` generated torrents in one transaction each.
fn insert(store: &mut Store, generator: &mut Generator, n: usize) {
    let source = store.register_source("drip", "1", 0).expect("source");
    for _ in 0..n {
        let t = generator.next_torrent();
        let Ok(info) = Info::parse(&t.info) else {
            continue;
        };
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: t.discovered_at,
                metadata_at: t.discovered_at,
                extra_flags: 0,
                source,
            }])
            .expect("insert");
    }
}

/// The measure: dhat's current bytes, or `Anonymous` (bytes).
fn measure() -> u64 {
    #[cfg(feature = "dhat-heap")]
    {
        dhat::HeapStats::get().curr_bytes as u64
    }
    #[cfg(not(feature = "dhat-heap"))]
    {
        std::fs::read_to_string("/proc/self/smaps_rollup")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("Anonymous:"))
                    .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            })
            .map_or(0, |kb| kb * 1024)
    }
}

/// Segment files (not meta.json, locks or the managed list) in `dir`.
fn segment_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                e.file_type().is_ok_and(|t| t.is_file())
                    && !name.starts_with('.')
                    && !name.ends_with(".json")
                    && !name.ends_with(".lock")
            })
            .count()
    })
}

fn config(batch: usize, commit_docs: usize) -> LiveConfig {
    LiveConfig {
        heap_bytes: 64 << 20,
        poll: Duration::from_millis(10),
        commit_every: Duration::from_secs(2),
        commit_docs,
        batch,
    }
}

/// Waits for `done`, polling, for at most `limit`.
fn until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Builds a frozen fixture in `dir`: `n` torrents indexed in commits of
/// 1,000, merges finished.
fn fixture(n: usize, dir: &Path) {
    std::fs::create_dir_all(dir).expect("dir");
    let db = dir.join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).expect("store");
    insert(&mut store, &mut Generator::new(11), n);
    let index = SearchIndex::open(&dir.join("index")).expect("index");
    let live = LiveIndexer::spawn(
        index,
        Store::open_reader(&db, StoreConfig::default()).expect("reader"),
        config(1000, 1000),
    )
    .expect("indexer");
    let stats = live.stats();
    until("the fixture indexed", Duration::from_secs(600), || {
        stats.indexed.load(Ordering::Relaxed) as usize >= n
    });
    live.stop().expect("stop");
    // Reopening waits for nothing; a writer's drop waits for its merges.
    let index = SearchIndex::open(&dir.join("index")).expect("reopen");
    index
        .writer(64 << 20)
        .expect("writer")
        .finish()
        .expect("merges");
    println!(
        "# fixture: {n} torrents; segment files: torrents {}, files {}",
        segment_files(&dir.join("index")),
        segment_files(&dir.join("index").join("files"))
    );
}

/// p50 and p99 of `d`, in milliseconds.
fn percentiles(d: &mut [Duration]) -> (f64, f64) {
    d.sort();
    let at = |q: f64| d[((d.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    (at(0.50), at(0.99))
}

/// `n` one-document commits through the indexer while another thread runs
/// title and file searches; reports commit and search latency. With the
/// prune on it then checks reclamation: searches stop, the merges finish,
/// one more commit runs production's prune, and only then is the gauge
/// compared with the live segment files.
fn latency(prune: bool, n: usize) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("index");
    let mut store =
        Store::open(tmp.path().join("lodestar.db"), StoreConfig::default()).expect("store");
    let index = SearchIndex::open(&dir).expect("index");
    index.set_mmap_cache_pruning(prune);
    let mut generator = Generator::new(5);
    let stop = Arc::new(AtomicBool::new(false));
    let searcher = {
        let (index, stop) = (index.clone(), stop.clone());
        std::thread::spawn(move || {
            let queries = [
                "season",
                "1080p remux",
                "file:(ext:mkv)",
                "file:(size:>1GB) season",
            ];
            let mut took = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                for q in queries {
                    let ast = parse(q).expect("query");
                    let t = Instant::now();
                    index
                        .search(&index.query(&ast, 1_767_225_600), 50)
                        .expect("search");
                    took.push(t.elapsed());
                }
            }
            took
        })
    };
    let mut indexer = index.writer(64 << 20).expect("writer");
    let mut commits = Vec::with_capacity(n);
    for _ in 0..n {
        insert(&mut store, &mut generator, 1);
        indexer.catch_up(&mut store, 1).expect("catch up");
        let t = Instant::now();
        indexer.commit().expect("commit");
        commits.push(t.elapsed());
    }
    stop.store(true, Ordering::Relaxed);
    let mut searches = searcher.join().expect("searcher");
    let (c50, c99) = percentiles(&mut commits);
    let (s50, s99) = percentiles(&mut searches);
    println!(
        "arm={} commits={n}: commit p50 {c50:.2} ms p99 {c99:.2} ms; search ({} runs) p50 {s50:.2} ms p99 {s99:.2} ms",
        if prune { "prune" } else { "default" },
        searches.len()
    );
    if !prune {
        return;
    }
    // 1. the searches are done (they held no lease); 2. the merges finish
    // and their garbage collection runs; 3. one more document, committed,
    // reloads the readers and runs production's prune; 4. compare.
    indexer.finish().expect("merges");
    let mut indexer = index.writer(64 << 20).expect("writer");
    insert(&mut store, &mut generator, 1);
    indexer.catch_up(&mut store, 1).expect("catch up");
    indexer.commit().expect("commit");
    let [et, ef] = index.mmap_cache_entries().expect("on disk");
    let (ft, ff) = (segment_files(&dir), segment_files(&dir.join("files")));
    println!("reclamation: entries {et}/{ef}, live segment files {ft}/{ff}");
    assert!(
        et.abs_diff(ft as u64) <= 12 && ef.abs_diff(ff as u64) <= 12,
        "the gauge doesn't match the live files"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("latency") {
        let prune = args.get(1).map(String::as_str) == Some("prune");
        latency(
            prune,
            args.get(2).and_then(|a| a.parse().ok()).unwrap_or(3000),
        );
        return;
    }
    if args.first().map(String::as_str) == Some("fixture") {
        let n = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(9_900);
        fixture(n, Path::new(args.get(2).expect("fixture DIR")));
        return;
    }
    let prune = match args.first().map(String::as_str) {
        Some("prune") => true,
        Some("default") => false,
        _ => panic!("arm: default | prune | fixture"),
    };
    let per_sec: f64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(10.0);
    let secs: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(3600);
    let out = PathBuf::from(args.get(3).map_or("mmap-drip-out", String::as_str));
    let from = args
        .iter()
        .position(|a| a == "--from")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    std::fs::create_dir_all(&out).expect("out dir");

    // `--dump-at N`: write dhat's profile at commit N, with the index and
    // its caches alive, and stop there.
    let dump_at: Option<u64> = args
        .iter()
        .position(|a| a == "--dump-at")
        .and_then(|i| args.get(i + 1)?.parse().ok());
    #[cfg(feature = "dhat-heap")]
    let mut profiler = Some(
        dhat::Profiler::builder()
            .file_name(out.join("dhat-heap.json"))
            .build(),
    );

    // A fixture is used in place: the caller copies it per run.
    let (dir, _tmp) = match &from {
        Some(d) => (d.clone(), None),
        None => {
            let t = tempfile::tempdir().expect("tempdir");
            (t.path().to_owned(), Some(t))
        }
    };
    let db = dir.join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).expect("store");
    let index = SearchIndex::open(&dir.join("index")).expect("index");
    index.set_mmap_cache_pruning(prune);
    // `--prod`: the live indexer exactly as `serve` configures it (Part 2's
    // production cadence); otherwise one document per commit.
    let prod = args.iter().any(|a| a == "--prod");
    let live = LiveIndexer::spawn(
        index.clone(),
        Store::open_reader(&db, StoreConfig::default()).expect("reader"),
        if prod {
            LiveConfig::default()
        } else {
            config(1, 1)
        },
    )
    .expect("indexer");
    let stats = live.stats();
    let base_commits = stats.commits.load(Ordering::Relaxed);

    // The pace: one torrent per tick, written by its own thread.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut generator = Generator::new(7);
            let step = Duration::from_secs_f64(1.0 / per_sec);
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                insert(&mut store, &mut generator, 1);
                next += step;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
        })
    };

    let started = Instant::now();
    let (base_indexed, mut next_row) = (stats.indexed.load(Ordering::Relaxed), 100);
    println!(
        "arm={} per_sec={per_sec} secs={secs} prod={prod} from={}",
        if prune { "prune" } else { "default" },
        from.as_ref()
            .map_or("fresh".into(), |p| p.display().to_string())
    );
    // `min_bytes`: the least of the measure, sampled every 200 ms since the
    // last row, so an allocation spike (a merge, the writer's arena) at the
    // sampling instant doesn't count as retained.
    println!(
        "commits,indexed,docs_per_commit,commits_per_sec,measure_bytes,min_bytes,files_t,files_f,entries_t,entries_f,secs"
    );
    let mut least = u64::MAX;
    while started.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(200));
        least = least.min(measure());
        let commits = stats.commits.load(Ordering::Relaxed) - base_commits;
        if dump_at.is_some_and(|n| commits >= n) {
            #[cfg(feature = "dhat-heap")]
            drop(profiler.take());
            break;
        }
        if commits < next_row {
            continue;
        }
        next_row = commits + 100;
        let indexed = stats.indexed.load(Ordering::Relaxed) - base_indexed;
        let t = started.elapsed().as_secs_f64();
        let [et, ef] = index.mmap_cache_entries().unwrap_or_default();
        println!(
            "{commits},{indexed},{:.3},{:.2},{},{least},{},{},{et},{ef},{t:.0}",
            indexed as f64 / commits.max(1) as f64,
            commits as f64 / t,
            measure(),
            segment_files(&dir.join("index")),
            segment_files(&dir.join("index").join("files")),
        );
        least = u64::MAX;
    }
    // The profile, if still running, is written with the index alive.
    #[cfg(feature = "dhat-heap")]
    drop(profiler.take());
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer");
    live.stop().expect("stop");
}
