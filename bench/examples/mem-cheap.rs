//! `mem-cheap` (X-mimalloc-override): where the C heap's memory goes when
//! SQLite and zstd run on many threads, as `serve` runs them.
//!
//! Torrents from the bench generator are written one transaction each (the
//! store's zstd-compressed file tree included), then indexed and committed
//! one document at a time, each step on a tokio `spawn_blocking` thread, so
//! the thread that runs SQLite keeps changing. `READERS` tasks meanwhile
//! search the index and read each hit's row and file tree (SQLite plus zstd
//! decompression) on their own `spawn_blocking` threads, from a pool of
//! read-only connections.
//!
//! Built for musl with cargo-zigbuild, as the release is, the C allocator is
//! zig's libc `malloc`; with `--features mimalloc-override` it is mimalloc's,
//! as in the release build since X-mimalloc-override:
//!
//! ```sh
//! cargo zigbuild --release -p lodestar-bench --example mem-cheap --target x86_64-unknown-linux-musl
//! cargo zigbuild --release -p lodestar-bench --example mem-cheap --target x86_64-unknown-linux-musl \
//!     --features mimalloc-override
//! target/x86_64-unknown-linux-musl/release/examples/mem-cheap [TORRENTS] [READERS]
//! ```
//!
//! Prints a CSV row every 1,000 torrents: torrents, `RssAnon`, mimalloc's
//! committed bytes, SQLite's own count, the anonymous memory outside
//! mimalloc's arena, and the count of those outside mappings by size. Then
//! commit and search latency, p50 and p99. Linux only (it reads `/proc`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::{Indexer, SearchIndex, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// An anonymous region at least this big (adjacent mappings merged) is
/// taken to be a mimalloc arena; mimalloc v3 reserves 1 GiB at a time.
const ARENA_MIN: u64 = 256 << 20;

/// Upper bounds of the size buckets for mappings outside the arena.
const BUCKETS: [(u64, &str); 5] = [
    (128 << 10, "<128K"),
    (512 << 10, "128K-512K"),
    (2 << 20, "512K-2M"),
    (16 << 20, "2M-16M"),
    (u64::MAX, ">=16M"),
];

/// What `/proc/self/smaps` says about anonymous memory.
#[derive(Debug, Default)]
struct Maps {
    /// `Anonymous` in arena regions, bytes.
    arena: u64,
    /// `Anonymous` in every other anonymous region, bytes.
    outside: u64,
    /// Outside regions by size bucket (those holding any anonymous memory).
    counts: [u64; BUCKETS.len()],
    /// `Anonymous` of the outside regions per bucket, bytes.
    bytes: [u64; BUCKETS.len()],
}

/// Reads `/proc/self/smaps`. Anonymous mappings (no path, inode 0) that
/// touch are merged first: madvise and guard pages split one region into
/// several mappings.
fn maps() -> Maps {
    let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps") else {
        return Maps::default();
    };
    // (start, end, anonymous bytes) of each anonymous mapping, in order.
    let mut regions: Vec<(u64, u64, u64)> = Vec::new();
    let mut current: Option<(u64, u64)> = None;
    for line in smaps.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let Some(range) = fields
            .first()
            .filter(|f| f.contains('-') && !f.ends_with(':'))
        {
            let parsed = range.split_once('-').and_then(|(a, b)| {
                Some((
                    u64::from_str_radix(a, 16).ok()?,
                    u64::from_str_radix(b, 16).ok()?,
                ))
            });
            let anon = fields.len() == 5 && fields.get(4) == Some(&"0");
            current = parsed.filter(|_| anon);
        } else if let (Some((start, end)), Some(&"Anonymous:")) = (current, fields.first()) {
            let kb: u64 = fields.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
            match regions.last_mut() {
                Some(last) if last.1 == start => {
                    last.1 = end;
                    last.2 += kb * 1024;
                }
                _ => regions.push((start, end, kb * 1024)),
            }
        }
    }
    let mut m = Maps::default();
    for (start, end, anon) in regions {
        let size = end - start;
        if size >= ARENA_MIN {
            m.arena += anon;
        } else if anon > 0 {
            m.outside += anon;
            let b = BUCKETS.iter().position(|(max, _)| size < *max).unwrap_or(0);
            m.counts[b] += 1;
            m.bytes[b] += anon;
        }
    }
    m
}

/// A `<key>: <n> kB` field of `/proc/self/status`, in bytes.
fn status(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

/// mimalloc's committed bytes (`process.commit_current`, the only field of
/// that name in `mi_stats_get_json`'s output).
fn mimalloc_committed() -> u64 {
    mimalloc::MiMalloc::stats_json()
        .ok()
        .and_then(|j| {
            let text = j.to_str().ok()?;
            let rest = &text[text.find("\"commit_current\"")? + 16..];
            let digits = rest.trim_start_matches([':', ' ']);
            let end = digits.find(|c: char| !c.is_ascii_digit())?;
            digits[..end].parse().ok()
        })
        .unwrap_or(0)
}

fn mib(b: u64) -> f64 {
    b as f64 / 1048576.0
}

fn row(n: u64) {
    let m = maps();
    let counts: Vec<String> = m.counts.iter().map(u64::to_string).collect();
    println!(
        "{n},{:.1},{:.1},{:.1},{:.1},{:.1},{}",
        mib(status("RssAnon")),
        mib(mimalloc_committed()),
        mib(lodestar_store::memstats::sqlite_memory_used()),
        mib(m.arena),
        mib(m.outside),
        counts.join(","),
    );
}

/// p50 and p99 of `d`, in milliseconds.
fn percentiles(d: &mut [Duration]) -> (f64, f64) {
    if d.is_empty() {
        return (0.0, 0.0);
    }
    d.sort();
    let at = |q: f64| d[((d.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    (at(0.50), at(0.99))
}

/// The writer's side: the store, the indexer, the generator and its source.
struct Writer {
    store: Store,
    indexer: Indexer,
    generator: Generator,
    source: SourceId,
}

impl Writer {
    /// One torrent in one transaction; returns how long the write took.
    fn write_one(&mut self) -> Duration {
        let t = self.generator.next_torrent();
        let Ok(info) = Info::parse(&t.info) else {
            return Duration::ZERO;
        };
        let started = Instant::now();
        self.store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: t.discovered_at,
                metadata_at: t.discovered_at,
                extra_flags: 0,
                source: self.source,
            }])
            .expect("insert");
        started.elapsed()
    }

    /// Indexes what's new and commits; returns how long the commit took.
    fn index_one(&mut self) -> Duration {
        self.indexer.catch_up(&mut self.store, 1).expect("catch up");
        let started = Instant::now();
        self.indexer.commit().expect("commit");
        started.elapsed()
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut args = std::env::args().skip(1);
    let total: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let readers: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(6);

    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).expect("store");
    let source = store.register_source("cheap", "1", 0).expect("source");
    let index = SearchIndex::open(&tmp.path().join("index")).expect("index");
    let writer = Arc::new(Mutex::new(Writer {
        store,
        indexer: index.writer(64 << 20).expect("writer"),
        generator: Generator::new(11),
        source,
    }));
    let pool: Arc<Mutex<Vec<Store>>> = Arc::new(Mutex::new(
        (0..readers)
            .map(|_| Store::open_reader(&db, StoreConfig::default()).expect("reader"))
            .collect(),
    ));

    let stop = Arc::new(AtomicBool::new(false));
    let rows_read = Arc::new(AtomicU64::new(0));
    let mut searchers = Vec::new();
    for r in 0..readers {
        let (index, pool, stop, rows_read) =
            (index.clone(), pool.clone(), stop.clone(), rows_read.clone());
        searchers.push(tokio::spawn(async move {
            let queries = ["season", "1080p remux", "file:(ext:mkv)", "x264 2019"];
            let mut took = Vec::new();
            let mut i = r;
            while !stop.load(Ordering::Relaxed) {
                let q = queries[i % queries.len()];
                i += 1;
                let (index, pool, rows_read) = (index.clone(), pool.clone(), rows_read.clone());
                let t = tokio::task::spawn_blocking(move || {
                    let started = Instant::now();
                    let ast = parse(q).expect("query");
                    let hits = index
                        .search(&index.query(&ast, 1_767_225_600), 20)
                        .expect("search")
                        .hits;
                    let mut conn = pool.lock().expect("pool").pop().expect("a reader");
                    for hit in &hits {
                        let id = hit.id as i64;
                        let _ = conn.torrent(id).expect("row");
                        let _ = conn.files(id).expect("files");
                    }
                    pool.lock().expect("pool").push(conn);
                    rows_read.fetch_add(hits.len() as u64, Ordering::Relaxed);
                    started.elapsed()
                })
                .await
                .expect("search task");
                took.push(t);
                // Paces the readers to a busy server's rate; nothing waits on it.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            took
        }));
    }

    println!(
        "torrents,rss_anon_mib,mimalloc_committed_mib,sqlite_mib,arena_anon_mib,outside_anon_mib,{}",
        BUCKETS.map(|(_, name)| name).join(",")
    );
    row(0);
    let started = Instant::now();
    let mut writes = Vec::with_capacity(total as usize);
    let mut commits = Vec::with_capacity(total as usize);
    for n in 1..=total {
        // Write and index on separate blocking tasks, as the pipeline's
        // writer and the live indexer are separate.
        let w = writer.clone();
        writes.push(
            tokio::task::spawn_blocking(move || w.lock().expect("writer").write_one())
                .await
                .expect("write"),
        );
        let w = writer.clone();
        commits.push(
            tokio::task::spawn_blocking(move || w.lock().expect("writer").index_one())
                .await
                .expect("index"),
        );
        if n % 1000 == 0 {
            row(n);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let mut searches = Vec::new();
    for s in searchers {
        searches.extend(s.await.expect("searcher"));
    }
    let secs = started.elapsed().as_secs_f64();
    let (w50, w99) = percentiles(&mut writes);
    let (c50, c99) = percentiles(&mut commits);
    let (s50, s99) = percentiles(&mut searches);
    println!(
        "# {total} torrents in {secs:.0} s; store write p50 {w50:.2} ms p99 {w99:.2} ms; \
         index commit p50 {c50:.2} ms p99 {c99:.2} ms; search+read ({} runs, {} rows) \
         p50 {s50:.2} ms p99 {s99:.2} ms; threads {}",
        searches.len(),
        rows_read.load(Ordering::Relaxed),
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s
                .lines()
                .find_map(|l| l.strip_prefix("Threads:").map(|v| v.trim().to_owned())))
            .unwrap_or_default(),
    );
    let m = maps();
    for (i, (_, name)) in BUCKETS.iter().enumerate() {
        println!(
            "# outside the arena, {name}: {} mappings, {:.1} MiB",
            m.counts[i],
            mib(m.bytes[i])
        );
    }
}
