//! `mem-merge` (X-merge-policy, Phase 1): what tantivy's merge policy
//! leaves behind when every commit holds about one document.
//!
//! The production writers (`SearchIndex::writer`) set no merge policy, so
//! both indexes run tantivy's default `LogMergePolicy`: every segment under
//! 10,000 documents sits in one level, and a level holding at least eight
//! segments not already merging is merged whole. At one document per commit
//! that re-merges the whole corpus about every seven commits until it passes
//! ~16.8k documents (10,000 × 2^0.75). This harness builds
//! the two writers itself, exactly as `Side::new` does (one indexing thread,
//! the same heap split, the real schemas, the name and trigram tokenizers and
//! the file-index settings, the same commit order and payload, a manual reader reload and
//! #406's mmap-cache prune after every commit), so the policy can be set.
//!
//! Arms (one per process):
//! - `default`: tantivy's default `LogMergePolicy` (production today);
//! - `tuned`: `LogMergePolicy` with `min_layer_size` lowered on both
//!   indexes and `max_docs_before_merge` capped on the file index.
//!
//! ```sh
//! # anonymous memory, as shipped (static musl, mimalloc):
//! cargo zigbuild --release --target x86_64-unknown-linux-musl -p lodestar-bench --example mem-merge
//! target/x86_64-unknown-linux-musl/release/examples/mem-merge ingest default 20000 --docs 1
//! # dhat (gnu): retained bytes and the heap's high-water per merge:
//! cargo run --release -p lodestar-bench --features dhat-heap --example mem-merge -- ingest tuned 20000
//! # search p50/p95 while ingesting:
//! cargo run --release -p lodestar-bench --example mem-merge -- latency tuned 5000
//! # discovery-to-searchable at a pace, committing every 30 s:
//! cargo run --release -p lodestar-bench --example mem-merge -- fresh tuned 1200 --per-hour 665 --every 30
//! ```
//!
//! Flags: `--docs K` documents per commit (default 1); `--seed N`
//! torrents indexed first in 5,000-document commits (`ingest`, default 0); `--min-layer N`
//! (tuned, default 300); `--files-cap N` (tuned, default 100,000);
//! `--main-cap N` (tuned, default tantivy's 10M).
//!
//! `ingest` prints one CSV row every 250 torrents: torrents, commits, the
//! measure (dhat current bytes, or `Anonymous` from smaps_rollup), the least
//! of the measure sampled after each commit since the last row (so a merge
//! in flight at the sampling instant doesn't count as retained), dhat's
//! high-water since the last row (0 without dhat), segments and the largest
//! segment of both indexes, merges seen, and seconds.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
use lodestar_search::{SearchIndex, TorrentDoc, files, parse, schema};
use lodestar_store::{NewTorrent, Store, StoreConfig};
use tantivy::directory::MmapDirectory;
use tantivy::indexer::{LogMergePolicy, MergePolicy};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(not(any(feature = "dhat-heap", feature = "system-alloc")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `serve`'s writer heap (`LiveConfig::default`).
const HEAP: usize = 256 << 20;

/// The policy knobs; `None` for tantivy's default.
#[derive(Clone, Copy, Debug)]
struct Tuned {
    min_layer: u32,
    main_cap: usize,
    files_cap: usize,
}

fn policy(tuned: Option<Tuned>, cap: impl Fn(&Tuned) -> usize) -> Box<dyn MergePolicy> {
    let mut p = LogMergePolicy::default();
    if let Some(t) = tuned {
        p.set_min_layer_size(t.min_layer);
        p.set_max_docs_before_merge(cap(&t));
    }
    Box::new(p)
}

/// One index as `Side` drives it, plus its reader and directory.
struct Half {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    dir: MmapDirectory,
    last_id: i64,
}

impl Half {
    fn new(index: Index, dir: MmapDirectory, heap: usize, p: Box<dyn MergePolicy>) -> Half {
        index
            .tokenizers()
            .register(NAME_TOKENIZER, NameTokenizer::default());
        index
            .tokenizers()
            .register(schema::TRIGRAM_TOKENIZER, schema::trigram_analyzer());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .expect("reader");
        let writer = index.writer_with_num_threads(1, heap).expect("writer");
        writer.set_merge_policy(p);
        Half {
            index,
            writer,
            reader,
            dir,
            last_id: 0,
        }
    }

    fn commit(&mut self) {
        let mut prepared = self.writer.prepare_commit().expect("prepare");
        // `Side::commit`'s watermark payload, without the population part,
        // which `payload_of` treats as optional.
        prepared.set_payload(&format!("lodestar-watermark:{};reindex:0", self.last_id));
        prepared.commit().expect("commit");
        self.reader.reload().expect("reload");
    }

    /// Segments and the largest one's documents.
    fn segments(&self) -> (usize, u32) {
        let metas = self.index.searchable_segment_metas().expect("metas");
        let max = metas.iter().map(|m| m.max_doc()).max().unwrap_or(0);
        (metas.len(), max)
    }
}

/// Both writers, as `SearchIndex::writer` builds them.
struct Writers {
    main: Half,
    files: Half,
    fields: schema::Fields,
    file_fields: files::FileFields,
    pending: usize,
}

impl Writers {
    fn open(dir: &Path, tuned: Option<Tuned>) -> Writers {
        let files_dir = dir.join("files");
        std::fs::create_dir_all(&files_dir).expect("dir");
        let (s, fields) = schema::build();
        let (fs, file_fields) = files::schema();
        let (md, fd) = (
            MmapDirectory::open(dir).expect("mmap"),
            MmapDirectory::open(&files_dir).expect("mmap"),
        );
        let main = Index::open_or_create(md.clone(), s).expect("index");
        let file = Index::builder()
            .schema(fs)
            .settings(files::settings())
            .open_or_create(fd.clone())
            .expect("files index");
        let file_heap = (HEAP / 4).max(15 << 20);
        let main_heap = HEAP.saturating_sub(file_heap).max(15 << 20);
        Writers {
            main: Half::new(main, md, main_heap, policy(tuned, |t| t.main_cap)),
            files: Half::new(file, fd, file_heap, policy(tuned, |t| t.files_cap)),
            fields,
            file_fields,
            pending: 0,
        }
    }

    /// `Indexer::catch_up_new` for torrents without enrichment (the
    /// generator stores none, so `doc_enrichment` is the default).
    fn catch_up(&mut self, store: &mut Store, limit: usize) -> usize {
        let ids = store.ids_after(self.main.last_id, limit).expect("ids");
        for &id in &ids {
            let (Some(row), Some(tree)) = (
                store.torrent(id).expect("row"),
                store.files(id).expect("files"),
            ) else {
                continue;
            };
            let doc = TorrentDoc::new(&row, &tree);
            self.main
                .writer
                .add_document(doc.to_document(&self.fields))
                .expect("add");
            for d in files::file_docs(id as u64, &tree) {
                self.files
                    .writer
                    .add_document(d.to_document(&self.file_fields))
                    .expect("add file");
            }
            self.main.last_id = id;
            self.files.last_id = id;
            self.pending += 1;
        }
        ids.len()
    }

    /// `Indexer::commit`: files first, then torrents, then the prune.
    fn commit(&mut self) {
        self.files.commit();
        self.main.commit();
        self.pending = 0;
        let _ = self.main.dir.get_cache_info();
        let _ = self.files.dir.get_cache_info();
    }
}

/// Writes `n` generated torrents, one transaction each.
fn insert(store: &mut Store, generator: &mut Generator, n: usize) {
    let source = store.register_source("merge", "1", 0).expect("source");
    let mut left = n;
    while left > 0 {
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
        left -= 1;
    }
}

/// dhat's current bytes, or `Anonymous` from smaps_rollup (bytes).
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

/// The heap's high-water since the last call, sampled every 2 ms by its
/// own thread (dhat only: its `max_bytes` never resets).
struct Peak {
    max: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Peak {
    fn start() -> Peak {
        let max = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "dhat-heap")]
        {
            let (max, stop) = (max.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    max.fetch_max(measure(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
        }
        Peak { max, stop }
    }

    fn take(&self) -> u64 {
        self.max.swap(0, Ordering::Relaxed)
    }
}

impl Drop for Peak {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn flag<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1)?.parse().ok()
}

/// `n` torrents, `docs` per commit, as fast as commits complete, after
/// `seed` torrents indexed in bulk.
fn ingest(tuned: Option<Tuned>, n: usize, docs: usize, seed: usize) {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::builder().testing().build();
    let started = Instant::now();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut store =
        Store::open(tmp.path().join("lodestar.db"), StoreConfig::default()).expect("store");
    let mut w = Writers::open(&tmp.path().join("index"), tuned);
    let mut generator = Generator::new(7);
    // `--seed N`: a corpus of N torrents first, in 5,000-document commits
    // under the arm's own policy, so the drip measured after it runs
    // against soak-sized segments.
    let mut indexed = 0usize;
    while indexed < seed {
        let k = (seed - indexed).min(5000);
        insert(&mut store, &mut generator, k);
        indexed += w.catch_up(&mut store, k);
        w.commit();
    }
    println!(
        "# seeded {indexed} torrents in {:.0} s",
        started.elapsed().as_secs_f64()
    );
    let peak = Peak::start();
    println!(
        "torrents,commits,measure_bytes,min_bytes,peak_bytes,segs_t,max_seg_t,segs_f,max_seg_f,merges_t,merges_f,secs"
    );
    let (mut commits, mut next_row) = (0u64, indexed + 250);
    let (mut least, mut merges, mut last_segs) = (u64::MAX, [0u64; 2], [0usize; 2]);
    while indexed < seed + n {
        insert(&mut store, &mut generator, docs);
        indexed += w.catch_up(&mut store, docs);
        w.commit();
        commits += 1;
        least = least.min(measure());
        // A merge shows as fewer searchable segments after a reload.
        for (i, half) in [&w.main, &w.files].into_iter().enumerate() {
            let (segs, _) = half.segments();
            if segs < last_segs[i] {
                merges[i] += 1;
            }
            last_segs[i] = segs;
        }
        if indexed >= next_row {
            while next_row <= indexed {
                next_row += 250;
            }
            let ((st, mt), (sf, mf)) = (w.main.segments(), w.files.segments());
            println!(
                "{indexed},{commits},{},{least},{},{st},{mt},{sf},{mf},{},{},{:.1}",
                measure(),
                peak.take(),
                merges[0],
                merges[1],
                started.elapsed().as_secs_f64()
            );
            least = u64::MAX;
        }
    }
}

/// p50 and p95 of `d`, in milliseconds.
fn percentiles(d: &mut [Duration]) -> (f64, f64) {
    if d.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    d.sort();
    let at = |q: f64| d[((d.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    (at(0.50), at(0.95))
}

/// `n` one-document commits while another thread runs title and file
/// searches through the real search path, reloading after each batch.
fn latency(tuned: Option<Tuned>, n: usize, docs: usize) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("index");
    let mut store =
        Store::open(tmp.path().join("lodestar.db"), StoreConfig::default()).expect("store");
    let mut w = Writers::open(&dir, tuned);
    let mut generator = Generator::new(5);
    // A few documents first, so the searcher opens a non-empty index.
    insert(&mut store, &mut generator, 50);
    w.catch_up(&mut store, 50);
    w.commit();
    let stop = Arc::new(AtomicBool::new(false));
    let searcher = {
        let (stop, dir) = (stop.clone(), dir.clone());
        std::thread::spawn(move || {
            let queries = [
                "season",
                "1080p remux",
                "file:(ext:mkv)",
                "file:(size:>1GB) season",
            ];
            let mut took = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                // Reopened each round: `refresh` reloads only the torrent
                // reader, and file clauses must see the file index's
                // current segments too. Only the searches are timed.
                let search = SearchIndex::open(&dir).expect("search index");
                for q in queries {
                    let ast = parse(q).expect("query");
                    let t = Instant::now();
                    search
                        .search(&search.query(&ast, 1_767_225_600), 50)
                        .expect("search");
                    took.push(t.elapsed());
                }
            }
            took
        })
    };
    let mut commits = Vec::new();
    let mut indexed = 0;
    while indexed < n {
        insert(&mut store, &mut generator, docs);
        indexed += w.catch_up(&mut store, docs);
        let t = Instant::now();
        w.commit();
        commits.push(t.elapsed());
    }
    stop.store(true, Ordering::Relaxed);
    let mut searches = searcher.join().expect("searcher");
    let (c50, c95) = percentiles(&mut commits);
    let (s50, s95) = percentiles(&mut searches);
    let ((st, _), (sf, _)) = (w.main.segments(), w.files.segments());
    println!(
        "torrents={n} docs_per_commit={docs}: commit p50 {c50:.2} ms p95 {c95:.2} ms; \
         search ({} runs) p50 {s50:.2} ms p95 {s95:.2} ms; segments {st}/{sf}",
        searches.len()
    );
}

/// Discovery-to-searchable: `secs` of torrents at `per_hour`, committed
/// once `every` seconds have passed with documents pending or 5,000 are
/// pending (`live.rs`'s rule), polling every 500 ms as `serve` does.
fn fresh(tuned: Option<Tuned>, secs: u64, per_hour: f64, every: f64) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut store =
        Store::open(tmp.path().join("lodestar.db"), StoreConfig::default()).expect("store");
    let mut w = Writers::open(&tmp.path().join("index"), tuned);
    let mut generator = Generator::new(3);
    let step = Duration::from_secs_f64(3600.0 / per_hour);
    let (every, poll) = (Duration::from_secs_f64(every), Duration::from_millis(500));
    let started = Instant::now();
    let (mut next_insert, mut last_commit) = (started, started);
    // `follow`'s `since` moves only on a commit.
    let mut waiting: VecDeque<Instant> = VecDeque::new();
    let mut seen = VecDeque::new();
    let mut lat = Vec::new();
    while started.elapsed() < Duration::from_secs(secs) {
        let now = Instant::now();
        while next_insert <= now {
            insert(&mut store, &mut generator, 1);
            waiting.push_back(next_insert);
            next_insert += step;
        }
        let got = w.catch_up(&mut store, 1000);
        seen.extend(waiting.drain(..got));
        if w.pending >= 5000 || (w.pending > 0 && last_commit.elapsed() >= every) {
            w.commit();
            last_commit = Instant::now();
            lat.extend(seen.drain(..).map(|t| last_commit - t));
        }
        if got == 0 {
            std::thread::sleep(poll);
        }
    }
    assert!(!lat.is_empty(), "no commit in {secs} s: run longer");
    lat.sort();
    let at = |q: f64| lat[((lat.len() - 1) as f64 * q) as usize].as_secs_f64();
    println!(
        "fresh: {} torrents at {per_hour}/h, commit every {:?}: discovery-to-searchable p50 {:.1} s p95 {:.1} s max {:.1} s",
        lat.len(),
        every,
        at(0.5),
        at(0.95),
        at(1.0)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("", String::as_str);
    let tuned = match args.get(1).map(String::as_str) {
        Some("default") => None,
        Some("tuned") => Some(Tuned {
            min_layer: flag(&args, "--min-layer").unwrap_or(300),
            main_cap: flag(&args, "--main-cap").unwrap_or(10_000_000),
            files_cap: flag(&args, "--files-cap").unwrap_or(100_000),
        }),
        _ => panic!("usage: mem-merge ingest|latency|fresh default|tuned N [flags]"),
    };
    let n: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let docs = flag(&args, "--docs").unwrap_or(1usize).max(1);
    println!("# mode={mode} policy={tuned:?} n={n} docs_per_commit={docs}");
    match mode {
        "ingest" => ingest(tuned, n, docs, flag(&args, "--seed").unwrap_or(0)),
        "latency" => latency(tuned, n, docs),
        "fresh" => fresh(
            tuned,
            n as u64,
            flag(&args, "--per-hour").unwrap_or(665.0),
            flag(&args, "--every").unwrap_or(30.0),
        ),
        _ => panic!("mode: ingest | latency | fresh"),
    }
}
