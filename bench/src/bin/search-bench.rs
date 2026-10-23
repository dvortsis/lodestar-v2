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
//! A third table times `/facets` (B6): a lease, then every facet group
//! and histogram for it, for a broad word alone and with selections that
//! need disjunctive passes of their own, gated at [`FACETS_P95`].
//!
//! A fourth times `/suggest` (B7) for each kind of item, under the base
//! query `/search` runs (hidden and Adult torrents left out), gated at
//! [`SUGGEST_P95`], after building the title FST and reporting its size.
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

use lodestar_bench::synth::{self, Generator};
use lodestar_metainfo::Info;
use lodestar_search::query::TagKey;
use lodestar_search::suggest::{self, Shown};
use lodestar_search::{
    CancelToken, FacetParams, Lease, LiveConfig, LiveIndexer, Node, SearchIndex, Sort, Start,
    facets, index_dir, parse, with_filters,
};
use lodestar_store::{HotColumns, NewTorrent, SourceId, Store, StoreConfig};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// §3: keyword search top 50, p95.
const KEYWORD_P95: Duration = Duration::from_millis(30);
/// §3: filter-only browse over the whole corpus, p95.
const BROWSE_P95: Duration = Duration::from_millis(50);
/// §3: facets for the same query (a separate request), p95.
const FACETS_P95: Duration = Duration::from_millis(100);
/// §3: typeahead / suggest, p95.
const SUGGEST_P95: Duration = Duration::from_millis(15);
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
Usage: search-bench [--torrents N] [--queries N] [--dir PATH] [--seed S] [--no-hot] [--gate]
                    [--only TEXT] [--commit-every N]

  --torrents N   corpus size (default 1000000)
  --queries N    queries per class (default 300)
  --commit-every N
                 index commits every N torrents (default 200000): fewer,
                 larger segments, as a live index's merges make them
  --dir PATH     keep the store and index here and reuse them next run
  --seed S       generator seed (default 1)
  --no-hot       search without hot columns (no swarm data, text-only
                 ranking, no swarm classes)
  --gate         exit 1 if a p95 target or the NRT budget is missed
  --only TEXT    run only the classes whose name contains TEXT

The NRT check stores 10 more torrents, so a reused --dir grows by 10.";

struct Args {
    torrents: usize,
    queries: usize,
    dir: Option<PathBuf>,
    seed: u64,
    hot: bool,
    gate: bool,
    only: Option<String>,
    commit_every: usize,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        torrents: 1_000_000,
        queries: 300,
        dir: None,
        seed: 1,
        hot: true,
        gate: false,
        only: None,
        commit_every: 200_000,
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
            "--no-hot" => a.hot = false,
            "--gate" => a.gate = true,
            "--only" => a.only = Some(value("--only")?),
            "--commit-every" => {
                a.commit_every = value("--commit-every")?
                    .parse()
                    .map_err(|_| "bad --commit-every")?
            }
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
    commit_every: usize,
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
        if indexer.pending() >= commit_every {
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

/// Hot columns (§7) for every stored torrent, from the synthetic swarm
/// distribution, as the scraper would leave them.
fn hot_columns(store: &Store, seed: u64) -> Result<HotColumns, Box<dyn std::error::Error>> {
    let hot = HotColumns::new();
    let mut after = 0;
    loop {
        let ids = store.ids_after(after, 100_000)?;
        let Some(&last) = ids.last() else {
            break;
        };
        for id in ids {
            let id = id as u64;
            if let Some(s) = synth::swarm(seed, id) {
                hot.set_swarm(id, s);
            }
            hot.touch(id, synth::last_seen(seed, id));
        }
        after = last;
    }
    Ok(hot)
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

fn classes(seed: u64, per_class: usize, hot: bool) -> Vec<Class> {
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
    let swarm_filters = [
        "seeders:>=5",
        "seeders:>=1 trackers:>=2",
        "seeders:0",
        "dht:>100",
        "leechers:>=10",
        "-seeders:0",
    ];
    let mut out = vec![
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
    ];
    if !hot {
        return out;
    }
    // Swarm health (§7 hot columns, M4): filters and sorts read the hot
    // columns by torrent id for every match.
    out.extend([
        Class {
            name: "word + swarm filter",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| {
                format!(
                    "{} {}",
                    pick(r, common),
                    swarm_filters[r.random_range(0..swarm_filters.len())]
                )
            }),
        },
        Class {
            name: "word by seeders",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Seeders { desc: true },
            queries: make(&mut |r| pick(r, common)),
        },
        Class {
            name: "trackers:>=3 browse",
            target: Some(BROWSE_P95),
            cold: false,
            sort: Sort::Discovered { desc: true },
            queries: make(&mut |_| "trackers:>=3".to_string()),
        },
        Class {
            name: "browse by seeders",
            target: Some(BROWSE_P95),
            cold: false,
            sort: Sort::Seeders { desc: true },
            queries: make(&mut |r| browse[r.random_range(0..browse.len())].to_string()),
        },
        // A cache miss reads the hot column for every match: the path a
        // fresh epoch takes after each scrape write, so it's gated too.
        Class {
            name: "by seeders (cold)",
            target: Some(BROWSE_P95),
            cold: true,
            sort: Sort::Seeders { desc: true },
            queries: make(&mut |r| browse[r.random_range(0..browse.len())].to_string()),
        },
        Class {
            name: "by trackers (cold)",
            target: Some(BROWSE_P95),
            cold: true,
            sort: Sort::Trackers { desc: true },
            queries: make(&mut |r| browse[r.random_range(0..browse.len())].to_string()),
        },
        // Hot predicates under an OR can't be lifted: they run as a
        // constant-score query checking each candidate (`CheckQuery`).
        Class {
            name: "word + OR'd swarm",
            target: Some(KEYWORD_P95),
            cold: false,
            sort: Sort::Relevance,
            queries: make(&mut |r| {
                let a = swarm_filters[r.random_range(0..swarm_filters.len())];
                let b = swarm_filters[r.random_range(0..swarm_filters.len())];
                format!("{} ({a} | {b})", pick(r, common))
            }),
        },
    ]);
    out
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run(a: &Args) -> Result<bool, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let dir = a.dir.clone().unwrap_or_else(|| tmp.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("bench.db");
    let mut index = SearchIndex::open(&index_dir(&dir.join("index")))?;
    {
        let mut store = Store::open(&db, StoreConfig::default())?;
        build(&mut store, &index, a.seed, a.torrents, a.commit_every)?;
        store.checkpoint()?;
    }
    let reader = Store::open_reader(&db, StoreConfig::default())?;
    if a.hot {
        let hot = Arc::new(hot_columns(&reader, a.seed)?);
        // Settle the epoch the loading writes started.
        hot.epoch();
        index = index.with_hot(hot);
    }
    let docs = index.num_docs();
    let now = 2_000_000_000;
    println!("Lodestar search benchmark");
    println!(
        "  corpus      {docs} torrents, {} index segments",
        index.segments()
    );
    println!(
        "  swarm       {}",
        if a.hot {
            "hot columns from the synthetic swarm distribution"
        } else {
            "none (--no-hot)"
        }
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
    for class in classes(a.seed, a.queries, a.hot)
        .into_iter()
        .filter(|c| a.only.as_deref().is_none_or(|o| c.name.contains(o)))
    {
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
    ok &= facets_path(a, &index, now)?;
    ok &= suggest_path(a, &index, &reader, now)?;
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
    for class in classes(a.seed, a.queries, a.hot)
        .into_iter()
        .filter(|c| a.only.as_deref().is_none_or(|o| c.name.contains(o)))
    {
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

/// `/facets` as the API runs it: a lease for the query, then every term
/// facet and histogram over it. "broad word" is one pass for all groups;
/// the selections add one disjunctive pass each.
fn facets_path(
    a: &Args,
    index: &SearchIndex,
    now: i64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let index = Arc::new(index.clone());
    let cancel = CancelToken::new();
    let (words, _) = vocabulary(a.seed ^ 0x5eed, 20_000);
    let common = &words[..words.len().min(100)];
    let mut rng = ChaCha8Rng::seed_from_u64(a.seed ^ 0xface7);
    let selections = [
        "res:1080p,2160p",
        "type:movie,tv",
        "ext:mkv -ext:exe",
        "lang:en private:no",
        "res:1080p type:movie",
    ];
    let mut broad = Vec::new();
    let mut selected = Vec::new();
    for _ in 0..a.queries {
        let w = common.choose(&mut rng).expect("vocabulary");
        broad.push(w.clone());
        let s = selections[rng.random_range(0..selections.len())];
        selected.push(format!("{w} {s}"));
    }
    // The whole corpus: the shared pass at its widest (sampled at scale).
    let browse = vec![String::new(); a.queries.min(50)];
    // Browsing with selections: the widest disjunctive passes, each over
    // most of the corpus (near the sampling threshold at 1M torrents).
    let browse_selected: Vec<String> = (0..a.queries.min(50))
        .map(|i| selections[i % selections.len()].to_string())
        .collect();
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    println!("\n  /facets: a lease, then every term facet and histogram");
    println!(
        "  {:<24} {:>8} {:>8} {:>8} {:>10}  target",
        "class", "p50", "p95", "max", "mean hits"
    );
    let mut ok = true;
    for (name, queries) in [
        ("broad word", &broad),
        ("word + selections", &selected),
        ("browse everything", &browse),
        ("browse + selections", &browse_selected),
    ] {
        let mut times = Vec::with_capacity(queries.len());
        let mut hits = 0usize;
        for q in queries {
            let started = Instant::now();
            let ast = parse(q)?.without_hidden();
            let lease = Lease::new(index.clone(), &ast, now, Sort::Relevance, &cancel)?;
            let got = facets(&lease, &ast, &[], &FacetParams::default(), &cancel)?;
            times.push(started.elapsed());
            hits += got.matches as usize;
        }
        times.sort();
        let p95 = pct(&times, 0.95);
        let pass = p95 <= FACETS_P95;
        ok &= pass;
        println!(
            "  {:<24} {:>8} {:>8} {:>8} {:>10}  p95 < {} ms {}",
            name,
            ms(pct(&times, 0.5)),
            ms(p95),
            ms(*times.last().expect("queries")),
            hits / queries.len(),
            FACETS_P95.as_millis(),
            if pass { "PASS" } else { "FAIL" }
        );
    }
    Ok(ok)
}

/// The process's resident set (Linux; `None` elsewhere).
fn rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024)
}

/// Runs `f`, sampling the resident set every few milliseconds: its
/// result and the highest sample.
fn peak_rss<T>(f: impl FnOnce() -> T) -> (T, Option<u64>) {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let done = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(rss().unwrap_or(0)));
    let sampler = {
        let (done, peak) = (done.clone(), peak.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Some(r) = rss() {
                    peak.fetch_max(r, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let out = f();
    done.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    if let Some(r) = rss() {
        peak.fetch_max(r, Ordering::Relaxed);
    }
    let p = peak.load(Ordering::Relaxed);
    (out, (p > 0).then_some(p))
}

/// `/suggest` as the API runs it, per kind of item, after building the
/// title FST from the store (its build time and memory are reported).
fn suggest_path(
    a: &Args,
    index: &SearchIndex,
    store: &Store,
    now: i64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let base = rss();
    let started = Instant::now();
    let (built, build_peak) = peak_rss(|| index.titles().rebuild_from(store));
    built?;
    let built = started.elapsed();
    let steady = rss();
    let set = index.titles().current().ok_or("no title set")?;
    println!(
        "\n  /suggest: title FST of {} titles from {} torrents, built in {:.1} s, {:.1} MB in memory",
        set.len(),
        set.torrents(),
        built.as_secs_f64(),
        set.memory_bytes() as f64 / 1e6
    );
    // A swap: the old set serves while the new directory's set builds.
    let (second, swap_peak) = peak_rss(|| suggest::TitleSet::build(store));
    let second = second?;
    let mb = |b: Option<u64>, from: Option<u64>| match (b, from) {
        (Some(b), Some(f)) => format!("{:+.0} MB", (b as f64 - f as f64) / 1e6),
        _ => "—".to_string(),
    };
    println!(
        "  RSS over the process before: one set {}, building it peaks at {}; a swap (old set, new set and its build) peaks at {}",
        mb(steady, base),
        mb(build_peak, base),
        mb(swap_peak, base)
    );
    drop(second);
    let (words, names) = vocabulary(a.seed ^ 0x5eed, 20_000);
    let common = &words[..words.len().min(100)];
    // Common words of letters only, long enough to misspell.
    let letters: Vec<String> = common
        .iter()
        .filter(|w| w.len() >= 5 && w.bytes().all(|b| b.is_ascii_lowercase()))
        .cloned()
        .collect();
    let mut rng = ChaCha8Rng::seed_from_u64(a.seed ^ 0x0506_6357);
    let cut = |w: &str, n: usize| w.chars().take(n).collect::<String>();
    let mut make = |f: &mut dyn FnMut(&mut ChaCha8Rng) -> String| {
        (0..a.queries).map(|_| f(&mut rng)).collect::<Vec<_>>()
    };
    let values = [
        "res:", "res:10", "ext:m", "lang:", "group:", "group:r", "codec:h", "type:", "hdr:",
        "audio:a",
    ];
    let chips = [
        "2160p", "x265", "mkv", "1080p", "flac", "2019", "hevc", "french",
    ];
    let classes: Vec<(&str, Vec<String>)> = vec![
        (
            "title prefix",
            make(&mut |r| {
                let name = names.choose(r).expect("names");
                let n = r.random_range(1..=3usize);
                format!("{} {}", name[0], cut(&name[1], n))
            }),
        ),
        (
            "short prefix",
            make(&mut |r| cut(common.choose(r).expect("words"), 2)),
        ),
        (
            "field value",
            make(&mut |r| values.choose(r).expect("values").to_string()),
        ),
        (
            "smart chip",
            make(&mut |r| {
                format!(
                    "{} {}",
                    common.choose(r).expect("words"),
                    chips.choose(r).expect("chips")
                )
            }),
        ),
        (
            "did you mean",
            make(&mut |r| {
                // A common word with one letter changed (one edit): it
                // finds nothing, so the fuzzy pass runs.
                let mut w: Vec<char> = letters.choose(r).expect("words").chars().collect();
                let i = r.random_range(1..w.len());
                w[i] = if w[i] == 'z' {
                    'a'
                } else {
                    (w[i] as u8 + 1) as char
                };
                // Typed (a space after it), not being typed.
                let mut w: String = w.into_iter().collect();
                w.push(' ');
                w
            }),
        ),
        (
            "did you mean, 32 words",
            make(&mut |r| {
                // Misspellings everywhere: only those nearest the caret
                // are looked at.
                (0..32)
                    .map(|_| {
                        let mut w: Vec<char> = letters.choose(r).expect("words").chars().collect();
                        let i = r.random_range(1..w.len());
                        w[i] = if w[i] == 'z' {
                            'a'
                        } else {
                            (w[i] as u8 + 1) as char
                        };
                        w.into_iter().collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " "
            }),
        ),
        (
            "field name",
            make(&mut |r| {
                cut(
                    ["re", "ex", "si", "ty", "co", "la"]
                        .choose(r)
                        .expect("names"),
                    2,
                )
            }),
        ),
    ];
    // `/search`'s base query: hidden and Adult torrents left out.
    let no_adult = [Node::Not {
        of: Box::new(Node::Tag {
            key: TagKey::Type,
            any: vec!["adult".into()],
        }),
    }];
    let restrict = move |n: Node| with_filters(n.without_hidden(), &no_adult);
    let shown = Shown {
        adult: false,
        hidden: false,
    };
    let cancel = CancelToken::new();
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    println!(
        "  {:<24} {:>8} {:>8} {:>8} {:>10}  target",
        "class", "p50", "p95", "max", "mean items"
    );
    let mut ok = true;
    for (name, queries) in &classes {
        let run = |q: &str| {
            let req = suggest::Request {
                q,
                pos: None,
                limit: suggest::DEFAULT_LIMIT,
                shown,
                restrict: &restrict,
                now,
            };
            suggest::suggest(index, &req, &cancel)
        };
        for q in queries.iter().take(20) {
            run(q)?;
        }
        let mut times = Vec::with_capacity(queries.len());
        let mut items = 0usize;
        for q in queries {
            let started = Instant::now();
            items += run(q)?.len();
            times.push(started.elapsed());
        }
        times.sort();
        let p95 = pct(&times, 0.95);
        let pass = p95 <= SUGGEST_P95;
        ok &= pass;
        println!(
            "  {:<24} {:>8} {:>8} {:>8} {:>10}  p95 < {} ms {}",
            name,
            ms(pct(&times, 0.5)),
            ms(p95),
            ms(*times.last().expect("queries")),
            items / queries.len(),
            SUGGEST_P95.as_millis(),
            if pass { "PASS" } else { "FAIL" }
        );
    }
    Ok(ok)
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
