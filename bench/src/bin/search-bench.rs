//! Search latency benchmark (architecture §3, §14 M2 exit criteria).
//!
//! Builds (or reuses) a synthetic store and index, derives a realistic
//! query mix from the synthetic names, and reports latency percentiles per
//! query class, end to end: parse, compile, search a page of 50 and load
//! the hits' rows from SQLite, as the API does. With `--gate` it exits
//! non-zero when a p95 target is missed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use std::sync::Arc;

use lodestar_bench::synth::{self, Generator};
use lodestar_metainfo::Info;
use lodestar_search::{SearchIndex, Sort, index_dir, parse};
use lodestar_store::{HotColumns, NewTorrent, SourceId, Store, StoreConfig};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// §3: keyword search top 50, p95.
const KEYWORD_P95: Duration = Duration::from_millis(30);
/// §3: filter-only browse over the whole corpus, p95.
const BROWSE_P95: Duration = Duration::from_millis(50);

const USAGE: &str = "\
Usage: search-bench [--torrents N] [--queries N] [--dir PATH] [--seed S] [--no-hot] [--gate]
                    [--only TEXT]

  --torrents N   corpus size (default 1000000)
  --queries N    queries per class (default 300)
  --dir PATH     keep the store and index here and reuse them next run
  --seed S       generator seed (default 1)
  --no-hot       search without hot columns (no swarm data, text-only
                 ranking, no swarm classes)
  --gate         exit 1 if a p95 target is missed
  --only TEXT    run only the classes whose name contains TEXT";

struct Args {
    torrents: usize,
    queries: usize,
    dir: Option<PathBuf>,
    seed: u64,
    hot: bool,
    gate: bool,
    only: Option<String>,
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
        build(&mut store, &index, a.seed, a.torrents)?;
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
