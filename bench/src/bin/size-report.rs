//! Size report (architecture §3, §14).
//!
//! Generates synthetic torrents, stores them the way the pipeline will
//! (a first slice without a dictionary, then a trained zstd dictionary,
//! recompression, and the rest with the dictionary), builds the torrent
//! index from the store, and reports bytes per torrent against the budgets.
//! With `--gate` it exits non-zero when a budget is exceeded, which is how CI
//! enforces it (`ci-ok` needs the size-budget job, GS-gate). The gate fails
//! closed: a measurement that is missing, zero or not finite, or an index
//! that doesn't hold every generated torrent, fails it too, so a broken
//! measurement can never pass as "under budget". `--summary PATH` appends
//! the budgets and the measured values as a Markdown table (CI passes
//! `$RUNNER_TEMP/size-report.md`, which a later step copies into the job
//! summary).
//!
//! **10M mode** (`--dataset DIR`, M8-PERF-1) measures a persisted
//! [`dataset`] instead of building a temporary corpus, and adds the §3
//! figures the CI gate doesn't check: store plus torrent index (1.5 KB),
//! the all-in figure (about 2.3 KB, a target, reported), and whether every
//! field the 1.0 index defines is populated. `--json PATH` writes every
//! figure against its budget (`bench/README.md`). Without `--dataset` the
//! run is the CI gate's, unchanged.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use lodestar_bench::dataset::{self, Row, Status, Totals, load, scrape_all};
use lodestar_bench::synth::Generator;
use lodestar_store::{Store, StoreConfig};

/// Budgets in bytes per torrent (owner-set, §3; the runner tunes them under
/// owner-decisions §2.26, a slice never does) at a ~30-file average. §3 allows 1.5 KB for
/// the canonical store plus the torrent index; M0 reserves 0.5 KB of that
/// for the index (built in M2), leaving 1 KB for the whole SQLite file, of
/// which file-tree blobs may use 0.6 KB.
const STORE_BUDGET: f64 = 1024.0;
const BLOB_BUDGET: f64 = 600.0;
const INDEX_BUDGET: f64 = 512.0;
/// The file index in smart mode (§3).
const FILE_INDEX_BUDGET: f64 = 800.0;
/// §3: the canonical store plus the torrent index.
const STORE_AND_INDEX_BUDGET: f64 = 1536.0;
/// §3: the honest all-in figure, a target (reported, never gated).
const ALL_IN_TARGET: f64 = 2.3 * 1024.0;

const USAGE: &str = "usage: size-report [--torrents N] [--seed S] [--train N] \
[--dict-size BYTES] [--level L] [--db PATH] [--gate] [--summary PATH]

  --torrents N      torrents to generate (default 100000)
  --seed S          generator seed (default 1)
  --train N         torrents stored before training the dictionary and
                    used as training samples (default 20000)
  --dict-size B     zstd dictionary size (default 114688)
  --level L         zstd level (default: the store default)
  --db PATH         keep the database at PATH instead of a temp dir
  --gate            exit 1 if a budget is exceeded or a value can't be measured
  --summary PATH    append the budget table (Markdown) to PATH

10M mode (bench/README.md):
  --dataset DIR     measure (or build once) the persisted dataset for --seed
                    and --torrents in DIR; another key is refused
  --low-resource    the low-resource figures (plan RQ8)
  --json PATH       write every figure against its §3 budget";

struct Args {
    torrents: usize,
    seed: u64,
    train: usize,
    dict_size: usize,
    level: Option<i32>,
    db: Option<PathBuf>,
    gate: bool,
    summary: Option<PathBuf>,
    dataset: Option<PathBuf>,
    low_resource: bool,
    json: Option<PathBuf>,
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
        summary: None,
        dataset: None,
        low_resource: false,
        json: None,
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
            "--summary" => args.summary = Some(value()?.into()),
            "--dataset" => args.dataset = Some(value()?.into()),
            "--low-resource" => args.low_resource = true,
            "--json" => args.json = Some(value()?.into()),
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    if args.torrents == 0 {
        return Err("--torrents must be positive".to_owned());
    }
    if args.dataset.is_some() && (args.db.is_some() || args.level.is_some()) {
        return Err("--dataset is built with the store defaults: no --db or --level".to_owned());
    }
    if args.dataset.is_some() && (args.train != 20_000 || args.dict_size != 112 << 10) {
        return Err("--dataset is built with the default --train and --dict-size".to_owned());
    }
    if args.low_resource && args.dataset.is_none() {
        return Err("--low-resource needs --dataset".to_owned());
    }
    Ok(args)
}

/// One budget line: what was measured, its value and its budget, in bytes
/// per torrent.
struct Check {
    what: &'static str,
    got: f64,
    budget: f64,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Pass,
    Over,
    /// The value can't be trusted: zero, negative or not finite.
    Unmeasured,
}

impl Check {
    fn verdict(&self) -> Verdict {
        if !self.got.is_finite() || self.got <= 0.0 {
            Verdict::Unmeasured
        } else if self.got <= self.budget {
            Verdict::Pass
        } else {
            Verdict::Over
        }
    }
}

/// The outcome of a run: the budget lines plus any reason the measurement
/// itself is invalid. It passes only when both are clean.
struct Report {
    torrents: usize,
    checks: Vec<Check>,
    invalid: Vec<String>,
    /// 10M mode's rows beyond the gate's checks (reported figures, fields
    /// not populated, low-resource switches).
    extra: Vec<Row>,
}

impl Report {
    fn passed(&self) -> bool {
        self.trustworthy() && self.checks.iter().all(|c| c.verdict() == Verdict::Pass)
    }

    /// Every value was measured and the indexes cover the whole corpus.
    fn trustworthy(&self) -> bool {
        self.invalid.is_empty()
            && self
                .checks
                .iter()
                .all(|c| c.verdict() != Verdict::Unmeasured)
    }

    /// Every check as a report row, then the extra ones.
    fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .checks
            .iter()
            .map(|c| {
                let mut row = Row::measured(
                    format!("size.{}", dataset::slug(c.what)),
                    c.what,
                    c.got,
                    "B/torrent",
                    Some(c.budget),
                );
                if c.verdict() == Verdict::Unmeasured {
                    row.status = Status::Unmeasured(format!("{} is not a measurement", c.got));
                }
                row
            })
            .collect();
        rows.extend(self.invalid.iter().enumerate().map(|(i, reason)| {
            Row::without(
                format!("size.coverage.{}", i + 1),
                "The indexes hold the whole corpus",
                "",
                None,
                Status::Unmeasured(reason.clone()),
            )
        }));
        rows.extend(self.extra.iter().cloned());
        rows
    }

    fn markdown(&self) -> String {
        let mut md = format!(
            "### Size budget (§3): {}\n\n{} synthetic torrents, bytes per torrent.\n\n\
             | Part | Measured | Budget | Result |\n|---|---:|---:|---|\n",
            if self.passed() { "pass" } else { "FAIL" },
            group(self.torrents as u64)
        );
        for c in &self.checks {
            let result = match c.verdict() {
                Verdict::Pass => "pass",
                Verdict::Over => "**FAIL: over budget**",
                Verdict::Unmeasured => "**FAIL: not measured**",
            };
            let _ = writeln!(
                md,
                "| {} | {:.1} | {:.0} | {result} |",
                c.what, c.got, c.budget
            );
        }
        for reason in &self.invalid {
            let _ = writeln!(md, "\n**FAIL: invalid measurement.** {reason}.");
        }
        md
    }
}

/// Appends `text` to the summary file, if one was asked for. A failure here
/// fails the run: CI must never lose the reason it went red.
fn write_summary(path: Option<&Path>, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let Some(path) = path else { return Ok(()) };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{text}")
}

/// Build-phase timings, for the throughput line (none for a dataset).
struct Timings {
    train: std::time::Duration,
    recompressed: u64,
    enrich: std::time::Duration,
    scrape: std::time::Duration,
    index: std::time::Duration,
}

fn run(args: &Args, ds: Option<&dataset::Dataset>) -> Result<Report, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let level = args
        .level
        .unwrap_or(StoreConfig::default().compression_level);
    let (path, index_path, totals, timings) = match ds {
        Some(ds) => (
            ds.db(),
            lodestar_search::index_dir(&ds.index_root()),
            ds.totals.clone(),
            None,
        ),
        None => {
            let path = args
                .db
                .clone()
                .unwrap_or_else(|| tmp.path().join("lodestar.db"));
            let index_path = lodestar_search::index_dir(&tmp.path().join("index"));
            let (totals, timings) = build(args, &path, &index_path)?;
            (path, index_path, totals, Some(timings))
        }
    };
    let store = Store::open_reader(&path, StoreConfig::default())?;
    let index = lodestar_search::SearchIndex::open(&index_path)?;
    measure(
        args,
        &store,
        &path,
        &index,
        &index_path,
        &totals,
        timings,
        level,
        ds.is_some(),
    )
}

/// Builds the temporary corpus the CI gate measures.
fn build(
    args: &Args,
    path: &Path,
    index_path: &Path,
) -> Result<(Totals, Timings), Box<dyn std::error::Error>> {
    let mut config = StoreConfig::default();
    if let Some(level) = args.level {
        config.compression_level = level;
    }
    let mut store = Store::open(path, config)?;
    let mut generator = Generator::new(args.seed);
    let mut totals = Totals::default();

    let first = args.train.min(args.torrents);
    load(&mut store, &mut generator, first, &mut totals)?;
    let t0 = Instant::now();
    store.train_file_dict(first, args.dict_size)?;
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
    // Swarm health (§7): every torrent scraped once, as the scheduler
    // leaves the store after its first pass.
    let t4 = Instant::now();
    scrape_all(&mut store, args.seed)?;
    let scrape_time = t4.elapsed();
    store.checkpoint()?;

    // The torrent index (§8), built by the same catch-up path the server
    // uses, then merged as it would be at rest.
    let t2 = Instant::now();
    let index = lodestar_search::SearchIndex::open(index_path)?;
    let mut indexer = index.writer(256 << 20)?;
    while indexer.catch_up(&mut store, 5000)? > 0 {
        indexer.commit()?;
    }
    indexer.finish()?;
    let index_time = t2.elapsed();
    Ok((
        totals,
        Timings {
            train: train_time,
            recompressed,
            enrich: enrich_time,
            scrape: scrape_time,
            index: index_time,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn measure(
    args: &Args,
    store: &Store,
    path: &Path,
    index: &lodestar_search::SearchIndex,
    index_path: &Path,
    totals: &Totals,
    timings: Option<Timings>,
    level: i32,
    full: bool,
) -> Result<Report, Box<dyn std::error::Error>> {
    let index_bytes = dir_size(index_path)? as f64;
    let file_index_bytes = dir_size(&index_path.join("files"))? as f64;

    let n = args.torrents as f64;
    let conn = store.connection();
    let dict: bool = conn.query_row("SELECT count(*) > 0 FROM zstd_dicts", [], |r| r.get(0))?;
    let file_bytes = std::fs::metadata(path)?.len() as f64;
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
        if dict {
            format!("{} KiB", args.dict_size >> 10)
        } else {
            "no".to_owned()
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

    println!(
        "  file index          {:.1} MB = {:.0} B per torrent ({} files, {:.0} B each)",
        file_index_bytes / 1e6,
        file_index_bytes / n,
        group(index.num_files()),
        file_index_bytes / index.num_files().max(1) as f64
    );

    println!("\n  torrent index by part (bytes per torrent)");
    for (part, bytes) in index.space_by_field()? {
        if bytes as f64 / n >= 0.5 {
            println!("    {part:<28} {:>7.1}", bytes as f64 / n);
        }
    }

    println!("\n  file index by part (bytes per torrent)");
    for (part, bytes) in index.files_space_by_field()? {
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

    let swarm_bytes: f64 = conn.query_row(
        "SELECT total(pgsize) FROM dbstat WHERE name IN \
         ('swarm', 'swarm_due', 'swarm_trackers', 'swarm_history', 'trackers')",
        [],
        |r| r.get(0),
    )?;
    let (live, with_history): (f64, f64) = conn.query_row(
        "SELECT (SELECT count(*) FROM swarm_trackers), (SELECT count(*) FROM swarm_history)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    println!(
        "\n  swarm health        {:.0} B per torrent ({:.0}% reporting, {:.0}% with history)",
        swarm_bytes / n,
        100.0 * live / n,
        100.0 * with_history / n
    );

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

    if let Some(t) = &timings {
        println!(
            "\n  throughput          generate+parse {:.0}/s, store {:.0}/s; \
             training + recompressing {} blobs took {:.1} s; enriching {:.0}/s; indexing {:.0}/s; \
             swarm updates {:.0}/s",
            n / totals.generate_parse.as_secs_f64(),
            n / totals.store.as_secs_f64(),
            group(t.recompressed),
            t.train.as_secs_f64(),
            n / t.enrich.as_secs_f64(),
            n / t.index.as_secs_f64(),
            n / t.scrape.as_secs_f64()
        );
    }

    let check = |what, got, budget| Check { what, got, budget };
    let mut checks = vec![
        check("file-tree blob mean", blob_bytes / n, BLOB_BUDGET),
        check("canonical store", file_bytes / n, STORE_BUDGET),
        check("torrent index", index_bytes / n, INDEX_BUDGET),
        check(
            "file index (smart)",
            file_index_bytes / n,
            FILE_INDEX_BUDGET,
        ),
    ];
    let extra = if full {
        checks.push(check(
            "store + torrent index",
            (file_bytes + index_bytes) / n,
            STORE_AND_INDEX_BUDGET,
        ));
        full_rows(
            args,
            index,
            (file_bytes + index_bytes + file_index_bytes) / n,
        )
    } else {
        Vec::new()
    };
    if args.low_resource {
        // RQ8: no file index in low-resource mode, so its budget doesn't apply.
        checks.retain(|c| c.what != "file index (smart)");
    }
    let report = Report {
        torrents: args.torrents,
        checks,
        extra,
        invalid: coverage(
            args.torrents as u64,
            index.num_docs(),
            totals.files,
            index.num_files(),
        ),
    };
    if full {
        println!(
            "    {:<22} {:>7.1} (target about {ALL_IN_TARGET:.0}, reported)",
            "all-in",
            (file_bytes + index_bytes + file_index_bytes) / n
        );
    }
    println!("\n  budgets (bytes per torrent)");
    for c in &report.checks {
        let verdict = match c.verdict() {
            Verdict::Pass => "PASS",
            Verdict::Over => "FAIL (over budget)",
            Verdict::Unmeasured => "FAIL (not measured)",
        };
        println!(
            "    {:<22} {:>7.1} <= {:<6.0} {verdict}",
            c.what, c.got, c.budget
        );
    }
    for reason in &report.invalid {
        println!("    FAIL (invalid measurement): {reason}");
    }
    for row in &report.extra {
        if let Status::Unavailable(why) | Status::Unmeasured(why) | Status::Off(why) = &row.status {
            println!("    {:<22} {why}", row.id);
        }
    }
    Ok(report)
}

/// 10M mode's rows beyond the gate's checks: the all-in figure, every
/// index field the 1.0 build defines that isn't populated, and in
/// low-resource mode the switches this build lacks.
fn full_rows(args: &Args, index: &lodestar_search::SearchIndex, all_in: f64) -> Vec<Row> {
    let mut rows = vec![Row::measured(
        "size.all-in",
        "All-in (target about 2.3 KB per torrent, reported)",
        all_in,
        "B/torrent",
        None,
    )];
    let unpopulated = index.unpopulated();
    if unpopulated.is_empty() {
        rows.push(Row::measured(
            "size.fields.unpopulated",
            "Every 1.0 index field populated",
            0.0,
            "fields",
            Some(0.0),
        ));
    }
    for f in unpopulated {
        rows.push(Row::without(
            format!("size.fields.{}", f.field),
            "Every 1.0 index field populated",
            "",
            None,
            Status::Unavailable(format!("{} is {:?} in this index", f.field, f.availability)),
        ));
    }
    if args.low_resource {
        rows.push(Row::without(
            "size.file-index-smart",
            "file index (smart)",
            "B/torrent",
            Some(FILE_INDEX_BUDGET),
            Status::Off("low-resource mode has no file index".into()),
        ));
        for (id, what) in [
            ("low_resource.switch.trigram_off", "the trigram field"),
            ("low_resource.switch.file_index_off", "the file index"),
        ] {
            rows.push(Row::without(
                id,
                "Low-resource mode (§3, plan RQ8)",
                "",
                None,
                Status::Unavailable(format!(
                    "this build has no low-resource switch for {what}: the sizes above include it"
                )),
            ));
        }
    }
    rows
}

/// Reasons the index sizes don't describe the whole generated corpus: an
/// index missing torrents or files would read as small and pass the budget.
fn coverage(torrents: u64, docs: u64, files: u64, indexed_files: u64) -> Vec<String> {
    let mut invalid = Vec::new();
    if docs != torrents {
        invalid.push(format!(
            "the torrent index holds {docs} documents for {torrents} generated torrents"
        ));
    }
    if indexed_files == 0 || indexed_files > files {
        invalid.push(format!(
            "the file index holds {indexed_files} files for {files} generated files"
        ));
    }
    invalid
}

/// Sum of the sizes of the files directly in `dir`. It does not recurse, on
/// purpose: the torrent index is measured on `index_path` and the file index
/// on its `files` subdirectory, so recursing would count the file index twice.
/// A new subdirectory under a measured folder is therefore not counted.
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
        if i > 0 && (s.len() - i).is_multiple_of(3) {
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
    let ds = match &args.dataset {
        Some(dir) => {
            let key = dataset::Key::new(args.seed, args.torrents as u64);
            eprintln!("dataset {} ({key})", dir.display());
            match dataset::open_or_build(dir, key, &mut |l| eprintln!("  {l}")) {
                Ok(ds) => Some(ds),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };
    let result = run(&args, ds.as_ref());
    let text = match &result {
        Ok(report) => report.markdown(),
        Err(e) => {
            eprintln!("error: {e}");
            format!("### Size budget (§3): FAIL\n\n**FAIL: size-report could not measure.** {e}")
        }
    };
    let written = write_summary(args.summary.as_deref(), &text);
    if let Err(e) = &written {
        eprintln!("error: writing the summary: {e}");
    }
    let mut json_ok = true;
    if let Some(path) = &args.json {
        let rows = match &result {
            Ok(report) => report.rows(),
            Err(e) => vec![Row::without(
                "run",
                "The whole run",
                "",
                None,
                Status::Unmeasured(format!("size-report could not measure: {e}")),
            )],
        };
        let mode = if args.low_resource {
            "low-resource"
        } else {
            "default"
        };
        let report = dataset::report(
            "size-report",
            mode,
            ds.as_ref().map(|d| d.key),
            ds.as_ref().map(|d| d.dir.as_path()),
            &rows,
        );
        if let Err(e) = dataset::write_report(path, &report) {
            eprintln!("error: writing {}: {e}", path.display());
            json_ok = false;
        }
    }
    // 10M mode passes only when every row does, the extra ones included.
    let full_ok = match (&result, &ds) {
        (Ok(report), Some(_)) => !args.gate || dataset::verdict(&report.rows()) == "pass",
        _ => true,
    };
    if json_ok && full_ok && succeeds(args.gate, &result, written.is_ok()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Whether the run exits 0. It fails closed: an error, a value that wasn't
/// measured, an index that doesn't cover the corpus or a summary that
/// couldn't be written fail with or without `--gate`; only being over a
/// budget needs `--gate` to fail.
fn succeeds<E>(gate: bool, result: &Result<Report, E>, summary_written: bool) -> bool {
    match result {
        Err(_) => false,
        Ok(_) if !summary_written => false,
        Ok(report) if !report.trustworthy() => false,
        Ok(report) => report.passed() || !gate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(got: f64, invalid: Vec<String>) -> Report {
        Report {
            torrents: 50_000,
            checks: vec![Check {
                what: "torrent index",
                got,
                budget: INDEX_BUDGET,
            }],
            invalid,
            extra: Vec::new(),
        }
    }

    #[test]
    fn verdicts() {
        assert!(report(391.0, vec![]).passed());
        assert!(
            report(INDEX_BUDGET, vec![]).passed(),
            "the budget itself passes"
        );
        assert!(!report(INDEX_BUDGET + 0.1, vec![]).passed());
    }

    #[test]
    fn fails_closed_on_values_it_cannot_trust() {
        for got in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let r = report(got, vec![]);
            assert!(!r.passed(), "{got} passed");
            assert!(r.markdown().contains("not measured"), "{got}");
        }
        let r = report(391.0, vec!["the torrent index holds 0 documents".into()]);
        assert!(!r.passed());
        assert!(r.markdown().contains("invalid measurement"));
    }

    #[test]
    fn coverage_flags_a_partial_index() {
        assert!(coverage(100, 100, 3000, 2900).is_empty());
        assert_eq!(coverage(100, 99, 3000, 2900).len(), 1);
        assert_eq!(coverage(100, 100, 3000, 0).len(), 1);
        assert_eq!(coverage(100, 100, 3000, 3001).len(), 1);
    }

    #[test]
    fn summary_shows_budget_and_measured_value() {
        let md = report(600.4, vec![]).markdown();
        assert!(md.starts_with("### Size budget (§3): FAIL"), "{md}");
        let line = format!("| torrent index | 600.4 | {INDEX_BUDGET:.0} | **FAIL: over budget** |");
        assert!(md.contains(&line), "{md}");
        assert!(md.contains("50,000"), "{md}");
    }

    #[test]
    fn exit_codes_fail_closed() {
        let ok = || Ok::<_, ()>(report(391.0, vec![]));
        let over = || Ok::<_, ()>(report(600.0, vec![]));
        let unmeasured = || Ok::<_, ()>(report(0.0, vec![]));
        let partial = || Ok::<_, ()>(report(391.0, vec!["partial".into()]));
        for gate in [false, true] {
            assert!(succeeds(gate, &ok(), true), "gate {gate}");
            assert!(
                !succeeds(gate, &ok(), false),
                "unwritten summary, gate {gate}"
            );
            assert!(!succeeds(gate, &unmeasured(), true), "gate {gate}");
            assert!(!succeeds(gate, &partial(), true), "gate {gate}");
            assert!(!succeeds(gate, &Err::<Report, _>(()), true), "gate {gate}");
        }
        assert!(succeeds(false, &over(), true), "a report without --gate");
        assert!(!succeeds(true, &over(), true));
    }

    #[test]
    fn summary_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.md");
        write_summary(Some(&path), "one").unwrap();
        write_summary(Some(&path), "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        write_summary(None, "ignored").unwrap();
    }
}
