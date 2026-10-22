//! Search latency benchmark (architecture §3, §14 M2 exit criteria).
//!
//! Builds (or reuses) a synthetic store and index, derives a realistic
//! query mix from the synthetic names, and reports latency percentiles per
//! query class, end to end: parse, compile, search a page of 50 and load
//! the hits' rows from SQLite, as the API does. With `--gate` it exits
//! non-zero when a p95 target is missed.
//!
//! A second table runs the same classes the way `/search` does since
//! B5a: a searcher lease (snapshot, count, stem tier) and its first page,
//! then a page deep into the results through cursors. It also checks
//! the sampled count estimate (what `total_approx` reports at scale)
//! against the exact count on every query of at least
//! [`ESTIMATE_FROM`] matches, gated at [`estimate_tolerance`].
//!
//! Last, it times new torrent → searchable (§3) through the live indexer
//! at its production settings, gated at [`NRT_MAX`]. The unit tests only
//! check that the indexer follows the store: a wall-clock budget there
//! failed whenever a busy CI runner starved it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::{
    CancelToken, Lease, LiveConfig, LiveIndexer, SearchIndex, Sort, Start, index_dir, parse,
};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// §3: keyword search top 50, p95.
const KEYWORD_P95: Duration = Duration::from_millis(30);
/// §3: filter-only browse over the whole corpus, p95.
const BROWSE_P95: Duration = Duration::from_millis(50);
/// The deep-cursor class reads this many pages of 50 before timing one.
const DEEP_PAGES: usize = 20;
/// The estimate samples a sixteenth of the documents, so its relative
/// error has a standard deviation of about `1 / sqrt(matches / 16)`
/// (`lodestar_search` `count.rs`). Each query is gated at four of them:
/// 11% at 20,000 matches, 3.6% at 200,000 (the least the API reports as
/// estimated), 1.1% at 2M.
fn estimate_tolerance(exact: usize) -> f64 {
    4.0 / (exact as f64 / 16.0).sqrt()
}
/// Counts checked: smaller ones are always counted exactly.
const ESTIMATE_FROM: usize = 20_000;
/// §3: new torrent → searchable.
const NRT_MAX: Duration = Duration::from_secs(5);
/// Torrents stored one at a time, each timed until it's searchable.
const NRT_ROUNDS: usize = 10;
/// A torrent never indexed fails the run here rather than hanging it.
const NRT_GIVE_UP: Duration = Duration::from_secs(120);

const USAGE: &str = "\
Usage: search-bench [--torrents N] [--queries N] [--dir PATH] [--seed S] [--gate]

  --torrents N   corpus size (default 1000000)
  --queries N    queries per class (default 300)
  --dir PATH     keep the store and index here and reuse them next run
  --seed S       generator seed (default 1)
  --gate         exit 1 if a p95 target or the NRT budget is missed

The NRT check stores 10 more torrents, so a reused --dir grows by 10.";

struct Args {
    torrents: usize,
    queries: usize,
    dir: Option<PathBuf>,
    seed: u64,
    gate: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        torrents: 1_000_000,
        queries: 300,
        dir: None,
        seed: 1,
        gate: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--torrents" => {
                a.torrents = value("--torrents")?.parse().map_err(|_| "bad --torrents")?
            }
            "--queries" => a.queries = value("--queries")?.parse().map_err(|_| "bad --queries")?,
            "--dir" => a.dir = Some(PathBuf::from(value("--dir")?)),
            "--seed" => a.seed = value("--seed")?.parse().map_err(|_| "bad --seed")?,
            "--gate" => a.gate = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

/// Fills the store with `n` synthetic torrents and indexes them.
fn build(
    store: &mut Store,
    index: &SearchIndex,
    seed: u64,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let have = (store.count()? as usize).min(n);
    let mut generator = Generator::new(seed);
    if have < n {
        for _ in 0..have {
            generator.next_torrent();
        }
    }
    let started = Instant::now();
    let mut left = n - have;
    while left > 0 {
        let take = left.min(2000);
        left -= take;
        let synth: Vec<_> = (0..take).map(|_| generator.next_torrent()).collect();
        let infos: Vec<Info> = synth
            .iter()
            .map(|t| Info::parse(&t.info))
            .collect::<Result<_, _>>()?;
        let batch: Vec<NewTorrent<'_>> = synth
            .iter()
            .zip(&infos)
            .map(|(t, info)| NewTorrent {
                info,
                raw_info: None,
                discovered_at: t.discovered_at,
                metadata_at: t.discovered_at + 5,
                extra_flags: 0,
                source: SourceId::new(0).expect("valid"),
            })
            .collect();
        store.insert(&batch)?;
        let done = n - left;
        if done % 100_000 < 2000 {
            eprintln!(
                "  stored {done} ({:.0}/s)",
                (done - have) as f64 / started.elapsed().as_secs_f64()
            );
        }
    }
    // Enrichment (§6), as the writer's background pass stores it. Only
    // missing or outdated records are derived, so a stored corpus is
    // enriched once per parser version.
    let started = Instant::now();
    let mut after = 0;
    let mut enriched = 0;
    while let Some((last, n)) = lodestar_pipeline::enrich::enrich_stored(store, after, 5000)? {
        after = last;
        enriched += n;
    }
    if enriched > 0 {
        eprintln!(
            "  enriched {enriched} ({:.0}/s)",
            enriched as f64 / started.elapsed().as_secs_f64()
        );
    }
    // The index catches up from its watermark, so a new schema version
    // reuses the stored corpus.
    let started = Instant::now();
    let mut indexer = index.writer(512 << 20)?;
    loop {
        let added = indexer.catch_up(store, 20_000)?;
        if added == 0 {
            break;
        }
        if indexer.pending() >= 200_000 {
            indexer.commit()?;
            eprintln!(
                "  indexed up to id {} ({:.0}/s)",
                indexer.last_id(),
                indexer.last_id() as f64 / started.elapsed().as_secs_f64()
            );
        }
    }
    indexer.commit()?;
    indexer.finish()?;
    Ok(())
}

/// Words from synthetic names, with how often they occur.
fn vocabulary(seed: u64, sample: usize) -> (Vec<String>, Vec<Vec<String>>) {
    let mut generator = Generator::new(seed);
    let mut freq: HashMap<String, usize> = HashMap::new();
    let mut names = Vec::new();
    for _ in 0..sample {
        let t = generator.next_torrent();
        let Ok(info) = Info::parse(&t.info) else {
            continue;
        };
        let words: Vec<String> = info
            .name
            .decode()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect();
        for w in &words {
            *freq.entry(w.clone()).or_default() += 1;
        }
        if words.len() >= 3 {
            names.push(words);
        }
    }
    let mut by_freq: Vec<(String, usize)> = freq.into_iter().collect();
    by_freq.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    (by_freq.into_iter().map(|(w, _)| w).collect(), names)
}

struct Class {
    name: &'static str,
    /// `None`: reported, not gated.
    target: Option<Duration>,
    /// Empty the browse cache before each query.
    cold: bool,
    sort: Sort,
    queries: Vec<String>,
}

fn classes(seed: u64, per_class: usize) -> Vec<Class> {
    let (words, names) = vocabulary(seed ^ 0x5eed, 20_000);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let common = &words[..words.len().min(100)];
    let rare = &words[words.len() / 2..];
    let mut make = |f: &mut dyn FnMut(&mut ChaCha8Rng) -> String| {
        (0..per_class).map(|_| f(&mut rng)).collect::<Vec<_>>()
    };
    let pick = |rng: &mut ChaCha8Rng, v: &[String]| v.choose(rng).expect("vocabulary").clone();
    let adjacent = |rng: &mut ChaCha8Rng, n: usize| {
        let name = names.choose(rng).expect("names");
        let start = rng.random_range(0..=name.len() - n);
        name[start..start + n].join(" ")
    };
    let browse = [
        "",
        "size:>4GB",
        "ext:mkv",
        "files:>=10 size:<500MB",
        "ext:flac,mp3",
        "private:no size:1GB..8GB",
    ];
    let file_clauses = [
        "file:(ext:mkv size:>1GB)",
        "file:(ext:flac size:>20MB)",
        "file:sample",
        "file:(ext:mp4,mkv -path:sample)",
    ];
    let facets = [
        "type:movie",
        "res:1080p",
        "codec:h265 res:2160p",
        "cat:video>=80",
        "type:music bytes:audio>=50",
        "type:tv junk:<20",
        "year:2015..2020",
        "has:nfo -type:software",
    ];
    vec![
        Class {
            name: "1 common word",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| pick(r, common)),
        },
        Class {
            name: "1 rare word",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| pick(r, rare)),
        },
        Class {
            name: "2 words",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| adjacent(r, 2)),
        },
        Class {
            name: "3 words",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| adjacent(r, 3)),
        },
        Class {
            name: "phrase",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| format!("\"{}\"", adjacent(r, 2))),
        },
        Class {
            name: "word + filter",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| {
                format!(
                    "{} {}",
                    pick(r, common),
                    browse[r.random_range(1..browse.len())]
                )
            }),
        },
        // Enrichment facets (§6): keyword tags, multi-valued numbers,
        // junk and category shares.
        Class {
            name: "word + facets",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| {
                format!(
                    "{} {}",
                    pick(r, common),
                    facets[r.random_range(0..facets.len())]
                )
            }),
        },
        // File clauses (§8): the file index first, then the torrent query
        // filtered by the matching torrents.
        Class {
            name: "word + file clause",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| {
                format!(
                    "{} {}",
                    pick(r, common),
                    file_clauses[r.random_range(0..file_clauses.len())]
                )
            }),
        },
        // §3's browse target is for results cached per index generation;
        // the cold figure shows what a cache miss costs.
        Class {
            name: "browse (cached)",
            target: Some(BROWSE_P95),
            cold: false,
            sort: Sort::Discovered { desc: true },
            queries: make(&mut |r| browse[r.random_range(0..browse.len())].to_string()),
        },
        Class {
            name: "browse (cold)",
            target: None,
            cold: true,
            sort: Sort::Discovered { desc: true },
            queries: make(&mut |r| browse[r.random_range(0..browse.len())].to_string()),
        },
    ]
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run(a: &Args) -> Result<bool, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let dir = a.dir.clone().unwrap_or_else(|| tmp.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("bench.db");
    let index = SearchIndex::open(&index_dir(&dir.join("index")))?;
    {
        let mut store = Store::open(&db, StoreConfig::default())?;
        build(&mut store, &index, a.seed, a.torrents)?;
        store.checkpoint()?;
    }
    let reader = Store::open_reader(&db, StoreConfig::default())?;
    let docs = index.num_docs();
    let now = 2_000_000_000;
    println!("Lodestar search benchmark");
    println!(
        "  corpus      {docs} torrents, {} index segments",
        index.segments()
    );
    println!(
        "  per class   {} queries, top 50, rows loaded from SQLite\n",
        a.queries
    );
    println!(
        "  {:<24} {:>8} {:>8} {:>8} {:>8} {:>10}  target",
        "class", "p50", "p95", "p99", "max", "mean hits"
    );

    let mut ok = true;
    for class in classes(a.seed, a.queries) {
        // Warm caches the way a running server would be.
        for q in class.queries.iter().take(20) {
            let ast = parse(q)?;
            index.search_with_fallback(&ast, now, class.sort, 50, 0)?;
        }
        let mut times = Vec::with_capacity(class.queries.len());
        let mut total_hits = 0usize;
        let mut fallbacks = 0usize;
        for q in &class.queries {
            if class.cold {
                index.clear_cache();
            }
            let started = Instant::now();
            let ast = parse(q)?;
            // As the API searches: with the stem tier when few match.
            let (results, stemmed) = index.search_with_fallback(&ast, now, class.sort, 50, 0)?;
            fallbacks += usize::from(stemmed);
            for h in &results.hits {
                reader.torrent(h.id as i64)?;
            }
            times.push(started.elapsed());
            total_hits += results.total;
        }
        times.sort();
        let p95 = pct(&times, 0.95);
        let pass = class.target.is_none_or(|t| p95 <= t);
        ok &= pass;
        let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
        println!(
            "  {:<24} {:>8} {:>8} {:>8} {:>8} {:>10}  {}",
            class.name,
            ms(pct(&times, 0.5)),
            ms(p95),
            ms(pct(&times, 0.99)),
            ms(*times.last().expect("queries")),
            total_hits / class.queries.len(),
            match class.target {
                Some(t) => format!(
                    "p95 < {} ms {}",
                    t.as_millis(),
                    if pass { "PASS" } else { "FAIL" }
                ),
                None => "(not gated)".to_string(),
            }
        );
        if fallbacks > 0 {
            println!("  {:<24} {fallbacks} widened by the stem tier", "");
        }
    }
    ok &= api_path(a, &index, &reader, now)?;
    ok &= nrt(a, &index, &db)?;
    Ok(ok)
}

/// New torrent → searchable (§3) over the whole corpus, through a live
/// indexer at its production settings ([`LiveConfig::default`]): each
/// round stores one torrent and times it until the index counts it.
/// Gated on the slowest round.
fn nrt(a: &Args, index: &SearchIndex, db: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let mut store = Store::open(db, StoreConfig::default())?;
    let live = LiveIndexer::spawn(
        index.clone(),
        Store::open_reader(db, StoreConfig::default())?,
        LiveConfig::default(),
    )?;
    // Another seed: torrents the corpus doesn't have.
    let mut generator = Generator::new(a.seed ^ 0x6e72_7400);
    let mut times = Vec::with_capacity(NRT_ROUNDS);
    for _ in 0..NRT_ROUNDS {
        let t = generator.next_torrent();
        let info = Info::parse(&t.info)?;
        let want = index.num_docs() + 1;
        store.insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: t.discovered_at,
            metadata_at: t.discovered_at + 5,
            extra_flags: 0,
            source: SourceId::new(0).expect("valid"),
        }])?;
        let started = Instant::now();
        while index.num_docs() < want {
            if started.elapsed() > NRT_GIVE_UP {
                return Err("the live indexer never indexed a new torrent".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        times.push(started.elapsed());
    }
    live.stop()?;
    times.sort();
    let worst = *times.last().expect("rounds");
    let pass = worst <= NRT_MAX;
    let secs = |d: Duration| format!("{:.2} s", d.as_secs_f64());
    println!(
        "\n  new torrent → searchable ({NRT_ROUNDS} rounds, production live indexer): p50 {}, max {}  max < {} s {}",
        secs(pct(&times, 0.5)),
        secs(worst),
        NRT_MAX.as_secs(),
        if pass { "PASS" } else { "FAIL" }
    );
    Ok(pass)
}

/// The classes as `/search` runs them: a lease and its first page (gated
/// like the table above), a page [`DEEP_PAGES`] deep through cursors
/// (reported), and the count estimates against exact counts (gated).
fn api_path(
    a: &Args,
    index: &SearchIndex,
    reader: &Store,
    now: i64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let index = Arc::new(index.clone());
    let cancel = CancelToken::new();
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    println!(
        "\n  as /search runs them: a searcher lease and its first page; then page {DEEP_PAGES} through cursors"
    );
    println!(
        "  {:<24} {:>8} {:>8} {:>8} {:>10} {:>10}  target",
        "class", "p50", "p95", "max", "deep p95", "checked"
    );
    let mut ok = true;
    // The worst error as a share of its query's tolerance, and the
    // worst error itself.
    let mut worst: f64 = 0.0;
    let mut worst_err: f64 = 0.0;
    for class in classes(a.seed, a.queries) {
        let mut times = Vec::with_capacity(class.queries.len());
        let mut deep = Vec::new();
        let mut estimated = 0usize;
        for (n, q) in class.queries.iter().enumerate() {
            if class.cold {
                index.clear_cache();
            }
            let started = Instant::now();
            let ast = parse(q)?.without_hidden();
            let lease = Lease::new(index.clone(), &ast, now, class.sort, &cancel)?;
            let page = lease.page(Start::Offset(0), 50, &cancel)?;
            for h in &page.hits {
                reader.torrent(h.id as i64)?;
            }
            times.push(started.elapsed());
            // The estimate's error, wherever it sampled and enough match
            // for the count to be one an index would estimate at scale.
            let sampled = lease.sampled_estimate(&cancel)?;
            if sampled.approx {
                let exact = lease.exact_total(&cancel)?.value;
                if exact >= ESTIMATE_FROM {
                    estimated += 1;
                    let err = (sampled.value as f64 - exact as f64).abs() / exact as f64;
                    worst = worst.max(err / estimate_tolerance(exact));
                    worst_err = worst_err.max(err);
                }
            }
            // Deep pages for a sample: each cursor page scans the
            // matches again, so they cost about a first page each.
            if n % 10 == 0 {
                let mut next = page.next;
                for _ in 1..DEEP_PAGES {
                    let Some(p) = next else { break };
                    next = lease.page(Start::After(p), 50, &cancel)?.next;
                }
                if let Some(p) = next {
                    let started = Instant::now();
                    let page = lease.page(Start::After(p), 50, &cancel)?;
                    for h in &page.hits {
                        reader.torrent(h.id as i64)?;
                    }
                    deep.push(started.elapsed());
                }
            }
        }
        times.sort();
        deep.sort();
        let p95 = pct(&times, 0.95);
        let pass = class.target.is_none_or(|t| p95 <= t);
        ok &= pass;
        println!(
            "  {:<24} {:>8} {:>8} {:>8} {:>10} {:>10}  {}",
            class.name,
            ms(pct(&times, 0.5)),
            ms(p95),
            ms(*times.last().expect("queries")),
            if deep.is_empty() {
                "—".to_string()
            } else {
                ms(pct(&deep, 0.95))
            },
            estimated,
            match class.target {
                Some(t) => format!(
                    "p95 < {} ms {}",
                    t.as_millis(),
                    if pass { "PASS" } else { "FAIL" }
                ),
                None => "(not gated)".to_string(),
            }
        );
    }
    let pass = worst <= 1.0;
    println!(
        "  count estimates: worst error {:.2}%, {:.0}% of its query's tolerance (4σ) {}",
        worst_err * 100.0,
        worst * 100.0,
        if pass { "PASS" } else { "FAIL" }
    );
    Ok(ok && pass)
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) if a.gate => ExitCode::FAILURE,
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
