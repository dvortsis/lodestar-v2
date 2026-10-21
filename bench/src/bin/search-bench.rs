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

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_search::{SearchIndex, Sort, index_dir, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// §3: keyword search top 50, p95.
const KEYWORD_P95: Duration = Duration::from_millis(30);
/// §3: filter-only browse over the whole corpus, p95.
const BROWSE_P95: Duration = Duration::from_millis(50);

const USAGE: &str = "\
Usage: search-bench [--torrents N] [--queries N] [--dir PATH] [--seed S] [--gate]

  --torrents N   corpus size (default 1000000)
  --queries N    queries per class (default 300)
  --dir PATH     keep the store and index here and reuse them next run
  --seed S       generator seed (default 1)
  --gate         exit 1 if a p95 target is missed";

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
    let have = store.count()? as usize;
    if have >= n {
        return Ok(());
    }
    let mut generator = Generator::new(seed);
    for _ in 0..have {
        generator.next_torrent();
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
            index.search_page(&index.query(&ast, now), class.sort, 50, 0)?;
        }
        let mut times = Vec::with_capacity(class.queries.len());
        let mut total_hits = 0usize;
        for q in &class.queries {
            if class.cold {
                index.clear_cache();
            }
            let started = Instant::now();
            let ast = parse(q)?;
            let results = index.search_page(&index.query(&ast, now), class.sort, 50, 0)?;
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
