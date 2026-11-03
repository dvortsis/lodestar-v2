//! X-mmap-cache-prune: one-document commits must not leave memory behind.
//!
//! Tantivy's `MmapDirectory` keeps a cache entry for every file it ever
//! mapped and drops dead ones only when asked (`get_cache_info()`); the
//! indexer prunes after each commit. Under dhat's testing mode this samples
//! the live heap at commit 500 and at commit 2,500, with the index alive
//! and merges on, and bounds the growth per commit between them. Each
//! sample is the least of the 100 commits before it, so a background merge
//! in flight at one moment doesn't count. Without the prune the dead
//! entries cost about 1–3 KB a commit.
//!
//! Run alone, as CI's own job does (dhat and parallel tests interfere),
//! about 3 min in release with pruning on:
//!
//! ```sh
//! cargo test --release -p lodestar-bench --features dhat-heap --test mmap_retention -- --test-threads=1
//! MMAP_RETENTION_PRUNE=off cargo test --release -p lodestar-bench --features dhat-heap --test mmap_retention -- --test-threads=1  # fails
//! ```
//!
//! The assertion never calls the pruning API: only the indexer's own commit
//! path prunes.

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, Store, StoreConfig};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Retained bytes per commit allowed between the samples: well under the
/// leak's 1–3 KB, above what the live index legitimately grows by.
const MAX_PER_COMMIT: f64 = 300.0;
const FIRST: usize = 500;
const LAST: usize = 2_500;
/// Commits each sample takes the least of.
const WINDOW: usize = 100;

#[test]
fn one_document_commits_retain_no_memory_per_commit() {
    let _profiler = dhat::Profiler::builder().testing().build();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
    let source = store.register_source("drip", "1", 0).unwrap();
    let index = SearchIndex::open(&dir.path().join("index")).unwrap();
    let off = std::env::var("MMAP_RETENTION_PRUNE").is_ok_and(|v| v == "off");
    index.set_mmap_cache_pruning(!off);
    let mut indexer = index.writer(15 << 20).unwrap();
    let mut generator = Generator::new(3);
    let mut samples = [f64::MAX; 2];
    for commit in 1..=LAST {
        // One stored torrent, read by the indexer and committed alone, as
        // the live indexer does when torrents arrive one at a time.
        let t = generator.next_torrent();
        let info = Info::parse(&t.info).unwrap();
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: t.discovered_at,
                metadata_at: t.discovered_at,
                extra_flags: 0,
                source,
            }])
            .unwrap();
        assert_eq!(indexer.catch_up(&mut store, 1).unwrap(), 1);
        indexer.commit().unwrap();
        let at = if commit > FIRST - WINDOW && commit <= FIRST {
            Some(0)
        } else if commit > LAST - WINDOW {
            Some(1)
        } else {
            None
        };
        if let Some(i) = at {
            samples[i] = samples[i].min(dhat::HeapStats::get().curr_bytes as f64);
        }
    }
    let per_commit = (samples[1] - samples[0]) / (LAST - FIRST) as f64;
    // Sampled while the index (and its caches) is alive.
    drop(indexer);
    eprintln!(
        "retained {per_commit:.0} B per commit (pruning {})",
        if off { "off" } else { "on" }
    );
    assert!(
        per_commit < MAX_PER_COMMIT,
        "the live heap grew {per_commit:.0} B per commit between commits {FIRST} and {LAST} \
         (bound {MAX_PER_COMMIT}); is the mmap cache pruned after each commit?"
    );
}
