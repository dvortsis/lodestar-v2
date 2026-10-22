//! Size report (architecture §3, §14).
//!
//! Generates synthetic torrents, stores them the way the pipeline will
//! (a first slice without a dictionary, then a trained zstd dictionary,
//! recompression, and the rest with the dictionary), builds the torrent
//! index from the store, and reports bytes per torrent against the budgets. With `--gate` it exits non-zero when a budget
//! is exceeded, which is how CI enforces it.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use lodestar_bench::synth::Generator;
use lodestar_metainfo::Info;
use lodestar_store::{NewTorrent, Store, StoreConfig, filetree};

/// Budgets in bytes per torrent at a ~30-file average. §3 allows 1.5 KB for
/// the canonical store plus the torrent index; M0 reserves 0.5 KB of that
/// for the index (built in M2), leaving 1 KB for the whole SQLite file, of
/// which file-tree blobs may use 0.6 KB.
const STORE_BUDGET: f64 = 1024.0;
const BLOB_BUDGET: f64 = 600.0;
const INDEX_BUDGET: f64 = 512.0;

const USAGE: &str = "usage: size-report [--torrents N] [--seed S] [--train N] \
[--dict-size BYTES] [--level L] [--db PATH] [--gate]

  --torrents N      torrents to generate (default 100000)
  --seed S          generator seed (default 1)
  --train N         torrents stored before training the dictionary and
                    used as training samples (default 20000)
  --dict-size B     zstd dictionary size (default 114688)
  --level L         zstd level (default: the store default)
  --db PATH         keep the database at PATH instead of a temp dir
  --gate            exit 1 if a budget is exceeded";

struct Args {
    torrents: usize,
    seed: u64,
    train: usize,
    dict_size: usize,
    level: Option<i32>,
    db: Option<PathBuf>,
    gate: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        torrents: 100_000,
        seed: 1,
        train: 20_000,
        dict_size: 112 << 10,
        level: None,
        db: None,
        gate: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        let num = |v: String| {
            v.replace('_', "")
                .parse::<u64>()
                .map_err(|e| format!("{flag}: {e}"))
        };
        match flag.as_str() {
            "--torrents" => args.torrents = num(value()?)? as usize,
            "--seed" => args.seed = num(value()?)?,
            "--train" => args.train = num(value()?)? as usize,
            "--dict-size" => args.dict_size = num(value()?)? as usize,
            "--level" => args.level = Some(value()?.parse().map_err(|e| format!("--level: {e}"))?),
            "--db" => args.db = Some(value()?.into()),
            "--gate" => args.gate = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    if args.torrents == 0 {
        return Err("--torrents must be positive".to_owned());
    }
    Ok(args)
}

#[derive(Default)]
struct Totals {
    files: u64,
    info_bytes: u64,
    raw_tree_bytes: u64,
    generate_parse: std::time::Duration,
    store: std::time::Duration,
}

/// Generates and inserts `n` torrents in batches.
fn load(
    store: &mut Store,
    generator: &mut Generator,
    n: usize,
    totals: &mut Totals,
) -> Result<(), Box<dyn std::error::Error>> {
    const BATCH: usize = 1000;
    let mut left = n;
    while left > 0 {
        let take = left.min(BATCH);
        left -= take;
        let t0 = Instant::now();
        let synth: Vec<_> = generator.by_ref().take(take).collect();
        let infos: Vec<Info> = synth
            .iter()
            .map(|t| Info::parse(&t.info))
            .collect::<Result<_, _>>()?;
        totals.generate_parse += t0.elapsed();
        for (t, info) in synth.iter().zip(&infos) {
            totals.files += info.files.len() as u64;
            totals.info_bytes += t.info.len() as u64;
            totals.raw_tree_bytes +=
                filetree::encode(&info.files, info.path_encoding)?.len() as u64;
        }
        let batch: Vec<_> = synth
            .iter()
            .zip(&infos)
            .map(|(t, info)| NewTorrent {
                info,
                raw_info: Some(&t.info),
                discovered_at: t.discovered_at,
                metadata_at: t.discovered_at + 5,
                extra_flags: 0,
                source: lodestar_store::SourceId::new(0).expect("valid id"),
            })
            .collect();
        let t1 = Instant::now();
        store.insert(&batch)?;
        totals.store += t1.elapsed();
    }
    Ok(())
}

fn run(args: &Args) -> Result<bool, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let path = args
        .db
        .clone()
        .unwrap_or_else(|| tmp.path().join("lodestar.db"));
    let mut config = StoreConfig::default();
    if let Some(level) = args.level {
        config.compression_level = level;
    }
    let level = config.compression_level;
    let mut store = Store::open(&path, config)?;
    let mut generator = Generator::new(args.seed);
    let mut totals = Totals::default();

    let first = args.train.min(args.torrents);
    load(&mut store, &mut generator, first, &mut totals)?;
    let t0 = Instant::now();
    let dict = store.train_file_dict(first, args.dict_size)?;
    let recompressed = store.recompress_files(1000)?;
    let train_time = t0.elapsed();
    load(
        &mut store,
        &mut generator,
        args.torrents - first,
        &mut totals,
    )?;
    // Enrichment (§6), as the writer's background pass stores it.
    let t3 = Instant::now();
    let mut after = 0;
    while let Some((last, _)) = lodestar_pipeline::enrich::enrich_stored(&mut store, after, 5000)? {
        after = last;
    }
    let enrich_time = t3.elapsed();
    store.checkpoint()?;

    // The torrent index (§8), built by the same catch-up path the server
    // uses, then merged as it would be at rest.
    let index_path = lodestar_search::index_dir(&tmp.path().join("index"));
    let t2 = Instant::now();
    let index = lodestar_search::SearchIndex::open(&index_path)?;
    let mut indexer = index.writer(256 << 20)?;
    while indexer.catch_up(&mut store, 5000)? > 0 {
        indexer.commit()?;
    }
    indexer.finish()?;
    let index_time = t2.elapsed();
    let index_bytes = dir_size(&index_path)? as f64;

    let n = args.torrents as f64;
    let conn = store.connection();
    let file_bytes = std::fs::metadata(&path)?.len() as f64;
    let blob_bytes: f64 =
        conn.query_row("SELECT total(length(data)) FROM torrent_files", [], |r| {
            r.get(0)
        })?;

    println!("Lodestar size report");
    println!(
        "  torrents            {} (seed {})",
        group(args.torrents as u64),
        args.seed
    );
    println!(
        "  files               {} ({:.1} per torrent)",
        group(totals.files),
        totals.files as f64 / n
    );
    println!(
        "  info dicts          {:.0} B mean incl. pieces (never stored)",
        totals.info_bytes as f64 / n
    );
    println!(
        "  file tree, raw      {:.0} B mean",
        totals.raw_tree_bytes as f64 / n
    );
    println!(
        "  file tree, stored   {:.0} B mean (zstd {level}, {} dictionary; {:.1}x)",
        blob_bytes / n,
        match dict {
            Some(_) => format!("{} KiB", args.dict_size >> 10),
            None => "no".to_owned(),
        },
        totals.raw_tree_bytes as f64 / blob_bytes
    );
    println!(
        "  SQLite file         {:.1} MB = {:.0} B per torrent",
        file_bytes / 1e6,
        file_bytes / n
    );

    println!(
        "  torrent index       {:.1} MB = {:.0} B per torrent ({} docs, {} segments)",
        index_bytes / 1e6,
        index_bytes / n,
        group(index.num_docs()),
        index.segments()
    );

    println!("\n  torrent index by part (bytes per torrent)");
    for (part, bytes) in index.space_by_field()? {
        if bytes as f64 / n >= 0.5 {
            println!("    {part:<28} {:>7.1}", bytes as f64 / n);
        }
    }

    println!("\n  by table/index (bytes per torrent, incl. page overhead)");
    let mut stmt =
        conn.prepare("SELECT name, sum(pgsize) FROM dbstat GROUP BY name ORDER BY 2 DESC")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
    for row in rows {
        let (name, bytes) = row?;
        if bytes / n >= 0.5 {
            println!("    {name:<28} {:>7.1}", bytes / n);
        }
    }

    println!("\n  by file count       torrents   stored blob mean");
    let mut stmt = conn.prepare(
        "SELECT CASE WHEN t.file_count <= 1 THEN 0 WHEN t.file_count <= 10 THEN 1
                     WHEN t.file_count <= 100 THEN 2 WHEN t.file_count <= 1000 THEN 3
                     ELSE 4 END AS b,
                count(*), avg(length(f.data))
         FROM torrents t JOIN torrent_files f ON f.torrent_id = t.id
         GROUP BY b ORDER BY b",
    )?;
    let labels = ["1", "2-10", "11-100", "101-1000", ">1000"];
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    for row in rows {
        let (b, count, mean) = row?;
        println!(
            "    {:<16} {:>10} {:>12.0} B",
            labels[b as usize],
            group(count as u64),
            mean
        );
    }

    println!(
        "\n  throughput          generate+parse {:.0}/s, store {:.0}/s; \
         training + recompressing {} blobs took {:.1} s; enriching {:.0}/s; indexing {:.0}/s",
        n / totals.generate_parse.as_secs_f64(),
        n / totals.store.as_secs_f64(),
        group(recompressed),
        train_time.as_secs_f64(),
        n / enrich_time.as_secs_f64(),
        n / index_time.as_secs_f64()
    );

    let checks = [
        ("file-tree blob mean", blob_bytes / n, BLOB_BUDGET),
        ("canonical store", file_bytes / n, STORE_BUDGET),
        ("torrent index", index_bytes / n, INDEX_BUDGET),
    ];
    println!("\n  budgets (bytes per torrent)");
    let mut ok = true;
    for (what, got, budget) in checks {
        let pass = got <= budget;
        ok &= pass;
        println!(
            "    {what:<22} {got:>7.0} <= {budget:<6.0} {}",
            if pass { "PASS" } else { "FAIL" }
        );
    }
    Ok(ok)
}

fn dir_size(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let meta = entry?.metadata()?;
        if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) if args.gate => ExitCode::FAILURE,
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
