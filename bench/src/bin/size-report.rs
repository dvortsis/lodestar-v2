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
//!
//! **Identification answers** (ENR-11, X-enr11-size-measure): before the
//! index is built, `--identified P` of the torrents a plugin can identify
//! (by stored kind, ENR-10) get a served answer (`synth::answer`): a
//! version 3 claims row with a match confidence, from a source whose terms
//! let it be served. [`synth::REALISTIC_IDENTIFIED_PERCENT`] is the
//! realistic share; the default, 0, is the report as it was before, line
//! for line, until the runner decides the budgets at that share. A list
//! (`0,60,100`) builds one corpus per share and ends with a table of every
//! budget line by share.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use lodestar_bench::dataset::{self, Row, Status, Totals, load, scrape_all};
use lodestar_bench::synth::{self, Generator, Work};
use lodestar_store::identify_sweep::TERMS_KEY;
use lodestar_store::{Store, StoreConfig};

/// Budgets in bytes per torrent (owner-set, §3; the runner tunes them under
/// owner-decisions §2.26, a slice never does) at a ~30-file average. §3 allows 1.5 KB for
/// the canonical store plus the torrent index. The runner re-split it for
/// RX-1's trigrams (2026-11-23, §2.26): 560 B for the index and 976 B for
/// the whole SQLite file, of which file-tree blobs may use 0.6 KB.
const STORE_BUDGET: f64 = 976.0;
const BLOB_BUDGET: f64 = 600.0;
const INDEX_BUDGET: f64 = 560.0;
/// The file index in smart mode (§3).
const FILE_INDEX_BUDGET: f64 = 800.0;
/// §3: the canonical store plus the torrent index.
const STORE_AND_INDEX_BUDGET: f64 = 1536.0;
/// §3: the honest all-in figure, a target (reported, never gated).
const ALL_IN_TARGET: f64 = 2.3 * 1024.0;

const USAGE: &str = "usage: size-report [--torrents N] [--seed S] [--train N] \
[--dict-size BYTES] [--level L] [--db PATH] [--identified P[,P...]] [--gate] \
[--summary PATH]

  --torrents N      torrents to generate (default 100000)
  --seed S          generator seed (default 1)
  --train N         torrents stored before training the dictionary and
                    used as training samples (default 20000)
  --dict-size B     zstd dictionary size (default 114688)
  --level L         zstd level (default: the store default)
  --db PATH         keep the database at PATH instead of a temp dir
  --identified P    percent of identifiable torrents holding a served
                    identification answer (default 0, the report before
                    identification; 60 is the realistic share); a list
                    (0,60,100) builds one corpus per share and compares them
  --gate            exit 1 if a budget is exceeded or a value can't be measured
  --summary PATH    append the budget table (Markdown) to PATH

10M mode (bench/README.md):
  --dataset DIR     measure (or build once) the persisted dataset for --seed
                    and --torrents in DIR; another key is refused
  --low-resource    the low-resource figures (plan RQ8)
  --json PATH       write every figure against its §3 budget";

#[derive(Clone)]
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
    /// The shares to measure, in percent of identifiable torrents.
    identified: Vec<u32>,
    /// The share this run measures (one of `identified`).
    share: u32,
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
        // The CI gate's run stays at 0 % until the runner decides the
        // budgets (docs/decisions/2026-11-27-X-enr11-size-measure.md).
        identified: vec![0],
        share: 0,
    };
    let mut identified_given = false;
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
            "--identified" => {
                args.identified = shares(&value()?)?;
                identified_given = true;
            }
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
    if args.dataset.is_some() {
        // A persisted dataset holds no answers (bench/README.md).
        if identified_given && args.identified != [0] {
            return Err(
                "--dataset holds no identification answers: --identified 0 only".to_owned(),
            );
        }
        args.identified = vec![0];
    }
    if args.identified.len() > 1 && (args.db.is_some() || args.json.is_some()) {
        return Err("--db and --json take one --identified share".to_owned());
    }
    Ok(args)
}

/// `--identified`'s list: percents 0–100, no repeats.
fn shares(v: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for p in v.split(',') {
        let p: u32 = p
            .trim()
            .trim_end_matches('%')
            .parse()
            .map_err(|e| format!("--identified {v}: {e}"))?;
        if p > 100 || out.contains(&p) {
            return Err(format!("--identified {v}: percents 0-100, each once"));
        }
        out.push(p);
    }
    Ok(out)
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
    /// The identified share measured, percent of identifiable torrents.
    share: u32,
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
            "### Size budget (§3): {}\n\n{} synthetic torrents{}, bytes per torrent.\n\n\
             | Part | Measured | Budget | Result |\n|---|---:|---:|---|\n",
            if self.passed() { "pass" } else { "FAIL" },
            group(self.torrents as u64),
            if self.share > 0 {
                format!(
                    ", {} % of identifiable torrents identified (ENR-11)",
                    self.share
                )
            } else {
                String::new()
            }
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
    identify: std::time::Duration,
    /// Torrents of a kind a plugin can identify, and those given an answer.
    identifiable: u64,
    identified: u64,
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
    // Identification answers (ENR-11), as ENR-10's runtime stores them on
    // classified torrents; the index reads them below.
    let t5 = Instant::now();
    let (identifiable, identified) = identify(&mut store, args.seed, args.share)?;
    let identify_time = t5.elapsed();
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
            identify: identify_time,
            identifiable,
            identified,
            scrape: scrape_time,
            index: index_time,
        },
    ))
}

/// A terms record (`lodestar_sources::terms::Terms::record`) that lets
/// answers be passed on: version 1, the redistribute flag, 180 days'
/// retention.
const TERMS_REDISTRIBUTE: [u8; 10] = {
    let r = (180u64 * 86_400).to_be_bytes();
    [1, 0b10, r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]
};

/// What a plugin looks up for a stored content type
/// (`lodestar_classify::ContentType::code`: Movie 0, Tv 1, Music 2,
/// Audiobook 3, Ebook 4, Comic 5, Software 6, Game 7). ENR-10 never sends
/// Images, Video, Other or Adult (8–11), nor an unclassified torrent.
fn work_of(content_type: Option<i64>, name: &str) -> Option<Work> {
    Some(match content_type? {
        0 => Work::Film,
        1 if name.starts_with('[') => Work::Anime,
        1 => Work::Show,
        2 => Work::Music,
        3..=5 => Work::Book,
        6 => Work::Software,
        7 => Work::Game,
        _ => return None,
    })
}

/// Stores a served answer on `percent` of the identifiable torrents, from
/// one enrichment source whose terms allow passing answers on, the way
/// `lodestar_pipeline::identify` writes them (`put_claims_at`). Returns
/// the identifiable torrents and those answered. Nothing at all is
/// written at 0 %, so that report is the one before ENR-11. Private
/// torrents (`flags::PRIVACY`) are never looked up (ENR-10), so they
/// aren't identifiable.
fn identify(
    store: &mut Store,
    seed: u64,
    percent: u32,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    if percent == 0 {
        return Ok((0, 0));
    }
    // Source 0 found every torrent (`dataset::load`); it's the DHT, which
    // the server registers first. A finder can't enrich what it found.
    let finder = store.register_source("dht", "1", synth::EPOCH)?;
    let source = store.register_source("wikidata", "1", synth::EPOCH)?;
    if finder.get() != 0 || source == finder {
        return Err("the identification source must not be the finder".into());
    }
    store.set_plugin_state(source, TERMS_KEY, Some(&TERMS_REDISTRIBUTE))?;
    let rows: Vec<(i64, String, Option<i64>)> = store
        .connection()
        .prepare("SELECT id, name, content_type FROM torrents WHERE flags & ?1 = 0 ORDER BY id")?
        .query_map([lodestar_store::flags::PRIVACY], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let (mut identifiable, mut identified) = (0, 0);
    for (id, name, content_type) in rows {
        let Some(work) = work_of(content_type, &name) else {
            continue;
        };
        identifiable += 1;
        if !synth::identified(seed, id as u64, percent) {
            continue;
        }
        let claims = synth::answer(seed, id as u64, work, &synth::work_title(&name));
        if store.put_claims_at(id, source, &claims, false, synth::EPOCH)? {
            identified += 1;
        }
    }
    Ok((identifiable, identified))
}

/// Torrents holding an answer search may serve, and the ids and titles
/// those answers carry. Read back through `served_identifications`, the
/// index's own rule, so an answer the index wouldn't read can't count.
fn served(store: &Store) -> Result<(u64, u64, u64), Box<dyn std::error::Error>> {
    let ids: Vec<i64> = store
        .connection()
        .prepare("SELECT DISTINCT torrent_id FROM torrent_claims ORDER BY torrent_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let (mut torrents, mut terms, mut titles) = (0, 0, 0);
    for id in ids {
        let answers = store.served_identifications(id)?;
        if !answers.is_empty() {
            torrents += 1;
        }
        for a in &answers {
            terms += a.ids.len() as u64;
            titles += a.alt_titles.len() as u64 + u64::from(a.canonical_title.is_some());
        }
    }
    Ok((torrents, terms, titles))
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
    let mut invalid = Vec::new();
    if let Some(t) = timings.as_ref().filter(|_| args.share > 0) {
        let (answered, terms, titles) = served(store)?;
        let claims_bytes: f64 =
            conn.query_row("SELECT total(length(data)) FROM torrent_claims", [], |r| {
                r.get(0)
            })?;
        println!(
            "  identified          {} ({:.1}% of the corpus; {}% of {} identifiable); \
             {:.1} ids and {:.1} titles each, {:.0} B claims blob mean",
            group(answered),
            100.0 * answered as f64 / n,
            args.share,
            group(t.identifiable),
            terms as f64 / answered.max(1) as f64,
            titles as f64 / answered.max(1) as f64,
            claims_bytes / answered.max(1) as f64
        );
        if answered != t.identified || answered == 0 {
            invalid.push(format!(
                "{answered} torrents hold a served answer for {} answered",
                t.identified
            ));
        }
    }
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
    // What a `du` of the same folder would add (index-stats explains it).
    let disk = lodestar_search::DiskStats::read(index_path)?;
    println!(
        "    live segments     {:.0} B/torrent; {} deleted docs; {:.0} B/torrent stale; {:.0} B/torrent with subfolders (du)",
        disk.live_file_bytes as f64 / n,
        group(disk.deleted),
        disk.stale_bytes as f64 / n,
        disk.recursive_bytes as f64 / n
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

    // Names past the trigram prefix: every regex verifies them (RX-1).
    let long = index.long_names()?;
    println!(
        "    long names (>{} B, always verified)  {} ({:.1}%)",
        lodestar_search::TRIGRAM_PREFIX_BYTES,
        group(long),
        100.0 * long as f64 / n
    );
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
        if args.share > 0 {
            println!(
                "                      identifying {:.0}/s ({:.1} s)",
                t.identified as f64 / t.identify.as_secs_f64(),
                t.identify.as_secs_f64()
            );
        }
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
    invalid.extend(coverage(
        args.torrents as u64,
        index.num_docs(),
        totals.files,
        index.num_files(),
    ));
    let report = Report {
        torrents: args.torrents,
        share: args.share,
        checks,
        extra,
        invalid,
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
    let mut results = Vec::new();
    for (i, &share) in args.identified.iter().enumerate() {
        if i > 0 {
            println!();
        }
        if args.identified.len() > 1 {
            println!("== identified share {share} % ==");
        }
        let mut one = args.clone();
        one.share = share;
        results.push(run(&one, ds.as_ref()));
    }
    let mut text = results
        .iter()
        .map(|result| match result {
            Ok(report) => report.markdown(),
            Err(e) => {
                eprintln!("error: {e}");
                format!(
                    "### Size budget (§3): FAIL\n\n**FAIL: size-report could not measure.** {e}"
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let measured: Vec<&Report> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    if measured.len() > 1 {
        let (plain, md) = compare(&measured);
        println!("\n{plain}");
        text.push('\n');
        text.push_str(&md);
    }
    // `--json` and `--dataset` take one share (`parse_args`).
    let result = results.remove(0);
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
        let mode = match (args.low_resource, args.identified[0]) {
            (true, _) => "low-resource".to_owned(),
            (false, 0) => "default".to_owned(),
            // An identified run's figures aren't the 0 % gate's.
            (false, share) => format!("default+identified-{share}"),
        };
        let report = dataset::report(
            "size-report",
            &mode,
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
    let others_ok = results
        .iter()
        .all(|r| succeeds(args.gate, r, written.is_ok()));
    if json_ok && full_ok && others_ok && succeeds(args.gate, &result, written.is_ok()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Every budget line by identified share, as plain text and Markdown:
/// bytes per torrent, and the change from the first share listed.
fn compare(reports: &[&Report]) -> (String, String) {
    let first = reports[0];
    let mut plain =
        String::from("  identification (ENR-11), bytes per torrent by identified share\n");
    let _ = write!(plain, "    {:<22}", "part");
    let mut md =
        String::from("### Identification answers (ENR-11): bytes per torrent by share\n\n| Part |");
    for r in reports {
        let _ = write!(plain, " {:>16}", format!("{} %", r.share));
        let _ = write!(md, " {} % |", r.share);
    }
    let _ = writeln!(plain, " {:>7}", "budget");
    md.push_str(" Budget |\n|---|");
    md.push_str(&"---:|".repeat(reports.len() + 1));
    md.push('\n');
    for (i, c) in first.checks.iter().enumerate() {
        let _ = write!(plain, "    {:<22}", c.what);
        let _ = write!(md, "| {} |", c.what);
        for r in reports {
            let got = r.checks.get(i).map_or(f64::NAN, |c| c.got);
            let cell = if std::ptr::eq(*r, first) {
                format!("{got:.1}")
            } else {
                format!("{got:.1} ({:+.1})", got - c.got)
            };
            let over = if got > c.budget { " **over**" } else { "" };
            let mark = if over.is_empty() { ' ' } else { '*' };
            let _ = write!(plain, " {cell:>15}{mark}");
            let _ = write!(md, " {cell}{over} |");
        }
        let _ = writeln!(plain, " {:>7.0}", c.budget);
        let _ = writeln!(md, " {:.0} |", c.budget);
    }
    plain.push_str("    (* over budget)\n");
    (plain, md)
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
            share: 0,
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

    #[test]
    fn identified_shares_parse() {
        assert_eq!(shares("60").unwrap(), [60]);
        assert_eq!(shares("0, 60%,100").unwrap(), [0, 60, 100]);
        for bad in ["", "101", "60,60", "-1", "x"] {
            assert!(shares(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_terms_record_lets_answers_be_served() {
        assert!(lodestar_store::identify_sweep::record_redistributes(
            &TERMS_REDISTRIBUTE
        ));
    }

    #[test]
    fn only_the_kinds_enr10_sends_are_identifiable() {
        assert_eq!(work_of(Some(0), "A.2001.1080p"), Some(Work::Film));
        assert_eq!(work_of(Some(1), "[Grp] A - 01"), Some(Work::Anime));
        assert_eq!(work_of(Some(1), "A.S01E01"), Some(Work::Show));
        assert_eq!(work_of(Some(3), "A"), Some(Work::Book));
        assert_eq!(work_of(Some(7), "A"), Some(Work::Game));
        for code in [None, Some(8), Some(9), Some(10), Some(11), Some(12)] {
            assert_eq!(work_of(code, "A"), None, "{code:?}");
        }
    }

    #[test]
    fn the_summary_names_the_share_only_when_there_is_one() {
        let zero = report(391.0, vec![]).markdown();
        assert!(
            zero.contains("50,000 synthetic torrents, bytes per torrent."),
            "{zero}"
        );
        let mut r = report(391.0, vec![]);
        r.share = 60;
        let md = r.markdown();
        assert!(
            md.contains("50,000 synthetic torrents, 60 % of identifiable torrents identified"),
            "{md}"
        );
    }

    #[test]
    fn the_comparison_shows_each_share_and_the_change() {
        let a = report(391.0, vec![]);
        let mut b = report(600.0, vec![]);
        b.share = 100;
        let (plain, md) = compare(&[&a, &b]);
        assert!(plain.contains("0 %") && plain.contains("100 %"), "{plain}");
        assert!(plain.contains("600.0 (+209.0)"), "{plain}");
        let row =
            format!("| torrent index | 391.0 | 600.0 (+209.0) **over** | {INDEX_BUDGET:.0} |");
        assert!(md.contains(&row), "{md}");
    }

    /// A small corpus stored and enriched as `build` does, then given
    /// answers at `share`.
    fn corpus(dir: &Path, share: u32) -> (Store, u64, u64) {
        let mut store = Store::open(dir.join("s.db"), StoreConfig::default()).unwrap();
        let mut generator = Generator::new(3);
        let mut totals = Totals::default();
        load(&mut store, &mut generator, 400, &mut totals).unwrap();
        let mut after = 0;
        while let Some((last, _)) =
            lodestar_pipeline::enrich::enrich_stored(&mut store, after, 5000).unwrap()
        {
            after = last;
        }
        let (identifiable, identified) = identify(&mut store, 3, share).unwrap();
        (store, identifiable, identified)
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn no_answers_at_zero_percent() {
        let dir = tempfile::tempdir().unwrap();
        let (store, identifiable, identified) = corpus(dir.path(), 0);
        assert_eq!((identifiable, identified), (0, 0));
        for table in ["sources", "torrent_claims", "source_state"] {
            assert_eq!(
                count(&store, &format!("SELECT count(*) FROM {table}")),
                0,
                "{table}"
            );
        }
    }

    #[test]
    fn every_answer_is_served_and_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, identifiable, identified) = corpus(dir.path(), 100);
        let eligible = count(
            &store,
            &format!(
                "SELECT count(*) FROM torrents WHERE content_type BETWEEN 0 AND 7 AND flags & {} = 0",
                lodestar_store::flags::PRIVACY
            ),
        );
        assert!(eligible > 300, "{eligible}");
        assert_eq!(
            (identifiable, identified),
            (eligible as u64, eligible as u64)
        );
        assert_eq!(served(&store).unwrap().0, identified);
        let private = count(
            &store,
            &format!(
                "SELECT count(*) FROM torrents WHERE flags & {} != 0",
                lodestar_store::flags::PRIVACY
            ),
        );
        let private_answered = count(
            &store,
            &format!(
                "SELECT count(*) FROM torrent_claims c JOIN torrents t ON t.id = c.torrent_id \
                 WHERE t.flags & {} != 0",
                lodestar_store::flags::PRIVACY
            ),
        );
        assert_eq!(private_answered, 0, "of {private} private torrents");

        // The index reads them: id terms land in the tag field.
        let index_path = lodestar_search::index_dir(&dir.path().join("index"));
        let index = lodestar_search::SearchIndex::open(&index_path).unwrap();
        let mut indexer = index.writer(32 << 20).unwrap();
        while indexer.catch_up(&mut store, 5000).unwrap() > 0 {
            indexer.commit().unwrap();
        }
        indexer.finish().unwrap();
        let index = lodestar_search::SearchIndex::open(&index_path).unwrap();
        // Every answer's ids find its torrent through the query language.
        let mut checked = 0;
        for id in [1i64, 2, 3, 50, 200] {
            let Some(a) = store.served_identifications(id).unwrap().pop() else {
                continue;
            };
            for x in &a.ids {
                let q = format!("{}:{}", x.scheme, x.value);
                let hits = index
                    .search(&index.query(&lodestar_search::parse(&q).unwrap(), 0), 1000)
                    .unwrap()
                    .hits;
                assert!(
                    hits.iter().any(|h| h.id == id as u64),
                    "{q} misses torrent {id}"
                );
                checked += 1;
            }
        }
        assert!(checked >= 3, "only {checked} ids checked");
    }
}
