//! `mem-titles` (X-import-drain-memory): the title-suggestion build's peak
//! anonymous memory must not grow with the store.
//!
//! Before X-import-drain-memory the build held every distinct title in
//! memory, several times over (a map, two vectors and the FST), and the
//! set kept the FST: about 125 MB of build and 87 B per title kept, per
//! million titles. An import's drain at 10M peaked at 4.45 GB on the
//! test VM. The build now spills sorted runs and writes a mapped file
//! (`lodestar_search::suggest::titles`).
//!
//! It fills a store with N synthetic torrents and builds the set in a
//! folder in a fresh child process, sampling the child's `Anonymous`
//! (smaps_rollup) every millisecond, then does the same at 2N. It also
//! measures the in-memory build (a RAM index's), which still grows, so
//! the row shows what it would catch. Exit status 1 if the folder
//! build's peak at 2N exceeds its peak at N by more than [`MARGIN`].
//!
//! ```sh
//! # as shipped (static musl, mimalloc):
//! cargo zigbuild --release --target x86_64-unknown-linux-musl -p lodestar-bench --example mem-titles
//! target/x86_64-unknown-linux-musl/release/examples/mem-titles 200000
//! ```
//!
//! Linux only for the figures (0 elsewhere, so the check passes vacuously).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::suggest::TitleSet;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The runs this bench builds with: small, so N = 200k (about 140k
/// titles, some 14 MiB of spellings) already spans a dozen and the peak
/// is the bounded build's, not one run's. Production uses 32 MiB: the
/// same shape, 32 times higher.
const RUN_BYTES: usize = 1 << 20;

/// How much more the build may hold at 2N than at N. What may grow with
/// the store is only the precomputed rankings (one per prefix shared by
/// more than 4,096 titles, about 1 KB each: well under 1 MiB at 2N =
/// 200k) and the merge's read buffers (8 KiB per run, about 1 MiB more
/// at 2N). The rest is the allocator's granularity: mimalloc commits in
/// 64 KiB slices of 32 MiB segments, so a run's worth of churn can land in
/// one more segment. 8 MiB covers that. Measured (4 vCPU): the folder
/// build peaked at 12.4 MiB at 200k torrents and 13.4 MiB at 400k, the
/// in-memory build at 58.4 and 96.4 MiB, so a build that grows with the
/// store fails by several times the margin.
const MARGIN: u64 = 8 << 20;

/// `Anonymous` from smaps_rollup, in bytes (0 off Linux).
fn anon() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Anonymous:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

/// Runs `f`, sampling `Anonymous` every millisecond on another thread;
/// returns its result and the highest `Anonymous` seen.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let peak = Arc::new(AtomicU64::new(anon()));
    let done = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, done) = (peak.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max(anon(), Ordering::Relaxed);
                // Paces the sampler; nothing waits on it.
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let out = f();
    peak.fetch_max(anon(), Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler");
    (out, peak.load(Ordering::Relaxed))
}

fn add(store: &mut Store, generator: &mut Generator, n: u64) {
    let source = SourceId::new(0).expect("source 0");
    let mut batch = Vec::new();
    let mut added = 0;
    while added < n {
        let t = generator.next_torrent();
        let Ok(info) = Info::parse(&t.info) else {
            continue;
        };
        batch.push((info, t.discovered_at));
        added += 1;
        if batch.len() == 1000 || added == n {
            let rows: Vec<NewTorrent> = batch
                .iter()
                .map(|(info, at)| NewTorrent {
                    info,
                    raw_info: None,
                    discovered_at: *at,
                    metadata_at: *at,
                    extra_flags: 0,
                    source,
                })
                .collect();
            store.insert(&rows).expect("insert");
            batch.clear();
        }
    }
    while lodestar_pipeline::enrich::enrich_stored(store, 0, 5000)
        .expect("enrich")
        .is_some()
    {}
}

/// The child: one build of the store at `db`, in the folder `dir` or in
/// memory, in a fresh process, so no earlier build's freed memory hides
/// this one's. Prints `titles,peak_bytes,seconds`.
fn child(db: &str, how: &str, dir: &str) {
    // As the server's builder reads it: no memory map.
    let config = StoreConfig {
        mmap_size: 0,
        ..StoreConfig::default()
    };
    let store = Store::open_reader(std::path::Path::new(db), config).expect("store");
    let started = Instant::now();
    let (set, peak) = peak_of(|| match how {
        "folder" => TitleSet::build_with_runs(&store, Some(std::path::Path::new(dir)), RUN_BYTES),
        _ => TitleSet::build(&store),
    });
    let set = set.expect("build");
    println!(
        "{},{peak},{:.2}",
        set.len(),
        started.elapsed().as_secs_f64()
    );
}

/// Runs one build in a child; returns its title count, peak and seconds.
fn measure(db: &std::path::Path, how: &str, dir: &std::path::Path) -> (u64, u64, f64) {
    let out = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args(["child", &db.to_string_lossy(), how, &dir.to_string_lossy()])
        .output()
        .expect("child");
    assert!(
        out.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8(out.stdout).expect("utf-8");
    let f: Vec<&str> = line.trim().split(',').collect();
    (
        f[0].parse().unwrap(),
        f[1].parse().unwrap(),
        f[2].parse().unwrap(),
    )
}

fn mib(b: u64) -> f64 {
    b as f64 / 1048576.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("child") {
        return child(&args[1], &args[2], &args[3]);
    }
    let n: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(200_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("s.db");
    let mut store = Store::open(&db, StoreConfig::default()).expect("store");
    let titles = dir.path().join("titles");
    let mut generator = Generator::new(20261127);
    println!("torrents,titles,folder_peak_mib,memory_peak_mib,folder_build_s");
    let mut folder = Vec::new();
    for torrents in [n, 2 * n] {
        add(&mut store, &mut generator, n);
        let (t, peak, secs) = measure(&db, "folder", &titles);
        let (_, memory, _) = measure(&db, "memory", &titles);
        println!(
            "{torrents},{t},{:.1},{:.1},{secs:.2}",
            mib(peak),
            mib(memory)
        );
        folder.push(peak);
    }
    let grew = folder[1].saturating_sub(folder[0]);
    println!(
        "folder build: {:.1} MiB at 2N against {:.1} MiB at N (margin {:.0} MiB)",
        mib(folder[1]),
        mib(folder[0]),
        mib(MARGIN)
    );
    if grew > MARGIN {
        eprintln!("FAIL: the title build's peak grew with the store");
        std::process::exit(1);
    }
}
