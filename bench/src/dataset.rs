//! The persisted benchmark dataset and the 10M report (M8-PERF-1,
//! architecture §3, plan 2026-11-06 §4.3).
//!
//! A dataset is a store and both search indexes built once from
//! [`synth::Generator`] into a folder the operator names, and reused by
//! `search-bench --dataset` and `size-report --dataset` after that. It is
//! keyed by [`Key`]: the dataset format, the generator's
//! [`synth::VERSION`], the seed and the torrent count. A folder whose key
//! differs is refused with a message naming both keys, so a number is
//! never reported against a corpus other than the one it names.
//!
//! **Never a user's data directory.** A folder is used only when it is new,
//! empty, or holds a `dataset.json`; anything else is refused before a
//! byte is written, and so is `$LODESTAR_DATA_DIR`.
//!
//! **Deterministic.** Nothing reads the clock: timestamps come from the
//! generator, scrapes from [`SCRAPED_AT`], and the year of Insights
//! rollups ends at [`ANCHOR_HOUR`]; the two bookkeeping columns the store
//! stamps with the clock are pinned to [`synth::EPOCH`] at the end. Two clean builds with the same key
//! give the same SQLite file, byte for byte but for the two header
//! counters SQLite bumps at each checkpoint, and the same [`digest`]
//! (`bench/tests/dataset.rs`). Tantivy names segments at random, so the
//! indexes are compared by their documents, not their bytes.
//!
//! **Read-only once built.** Rows that write (new torrent → searchable
//! under ingest) run on a [`scratch`] copy, so every run starts from the
//! same bytes.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestar_metainfo::Info;
use lodestar_pipeline::metric;
use lodestar_store::{
    CompactOptions, NewTorrent, SourceId, Store, StoreConfig, SwarmUpdate, TrackerCounts,
    WriteBatch, filetree,
};
use serde_json::{Value, json};

use crate::synth::{self, Generator};

/// The dataset layout and build recipe. Raise it when anything below
/// changes what a build writes (the constants, the phases, the rollups).
pub const FORMAT: u32 = 1;
/// The manifest beside the database.
pub const MANIFEST: &str = "dataset.json";
/// The store, as the search benchmark has always named it.
pub const DB: &str = "bench.db";
/// Torrents stored before the file-tree dictionary is trained, and its
/// samples (`size-report`'s defaults).
pub const TRAIN: usize = 20_000;
/// The trained dictionary's size (`size-report`'s default).
pub const DICT_SIZE: usize = 112 << 10;
/// The index writer's heap and commit interval (`search-bench`'s: fewer,
/// larger segments, as a live index's merges make them; X-search-files-5
/// measured 10M this way).
pub const INDEX_HEAP: usize = 512 << 20;
pub const COMMIT_EVERY: usize = 200_000;
/// When every synthetic scrape happened.
pub const SCRAPED_AT: i64 = 1_760_000_000;
/// The hour the year of rollups ends at: [`synth::EPOCH`]'s.
pub const ANCHOR_HOUR: i64 = synth::EPOCH / 3600;
/// Trackers in the synthetic scrape list, about a real default list.
const TRACKERS: usize = 20;

/// What a dataset was built from. Every field must match for reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub format: u32,
    pub generator: u32,
    pub seed: u64,
    pub torrents: u64,
}

impl Key {
    /// This build's key for `seed` and `torrents`.
    pub fn new(seed: u64, torrents: u64) -> Key {
        Key {
            format: FORMAT,
            generator: synth::VERSION,
            seed,
            torrents,
        }
    }

    pub fn to_json(self) -> Value {
        json!({
            "format": self.format,
            "generator": self.generator,
            "seed": self.seed,
            "torrents": self.torrents,
        })
    }

    fn from_json(v: &Value) -> Option<Key> {
        let n = |k: &str| v.get(k)?.as_u64();
        Some(Key {
            format: u32::try_from(n("format")?).ok()?,
            generator: u32::try_from(n("generator")?).ok()?,
            seed: n("seed")?,
            torrents: n("torrents")?,
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "format {}, generator {}, seed {}, {} torrents",
            self.format, self.generator, self.seed, self.torrents
        )
    }
}

/// Why a folder can't be used. Nothing was written to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Its manifest names another key.
    Mismatch {
        dir: PathBuf,
        found: Key,
        wanted: Key,
    },
    /// Not empty and no manifest: possibly someone's data.
    NotADataset(PathBuf),
    /// `$LODESTAR_DATA_DIR`.
    DataDir(PathBuf),
    /// A manifest that can't be read.
    BadManifest(PathBuf, String),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Mismatch { dir, found, wanted } => write!(
                f,
                "{} holds a dataset built with {found}; this run asks for {wanted}. \
                 Name another folder, or delete this one to rebuild",
                dir.display()
            ),
            Refused::NotADataset(dir) => write!(
                f,
                "{} is not empty and has no {MANIFEST}: refusing to write to it \
                 (a dataset folder must be new, empty, or one this tool built)",
                dir.display()
            ),
            Refused::DataDir(dir) => write!(
                f,
                "{} is $LODESTAR_DATA_DIR: a dataset is never built in a data directory",
                dir.display()
            ),
            Refused::BadManifest(dir, why) => {
                write!(
                    f,
                    "{}: unreadable {MANIFEST}: {why}",
                    dir.join(MANIFEST).display()
                )
            }
        }
    }
}

impl std::error::Error for Refused {}

/// What generating cost and contained, as `size-report` prints it.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub files: u64,
    pub info_bytes: u64,
    pub raw_tree_bytes: u64,
    pub generate_parse: Duration,
    pub store: Duration,
}

impl Totals {
    fn to_json(&self) -> Value {
        json!({
            "files": self.files,
            "info_bytes": self.info_bytes,
            "raw_tree_bytes": self.raw_tree_bytes,
        })
    }

    fn from_json(v: &Value) -> Totals {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Totals {
            files: n("files"),
            info_bytes: n("info_bytes"),
            raw_tree_bytes: n("raw_tree_bytes"),
            ..Totals::default()
        }
    }
}

/// A complete dataset.
#[derive(Debug, Clone)]
pub struct Dataset {
    pub dir: PathBuf,
    pub key: Key,
    pub totals: Totals,
    /// Built (or finished) by this process: its memory isn't a steady
    /// state's.
    pub built_now: bool,
}

impl Dataset {
    pub fn db(&self) -> PathBuf {
        self.dir.join(DB)
    }

    /// The index root (`lodestar_search::index_dir` adds the version).
    pub fn index_root(&self) -> PathBuf {
        self.dir.join("index")
    }
}

/// The build's phases, in order; the manifest records the last one done,
/// so an interrupted build resumes after it.
const PHASES: [&str; 6] = [
    "stored", "enriched", "scraped", "rollups", "indexed", "complete",
];

fn read_manifest(dir: &Path) -> Result<Option<Value>, Refused> {
    let path = dir.join(MANIFEST);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Refused::BadManifest(dir.into(), e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Refused::BadManifest(dir.into(), e.to_string())),
    }
}

fn write_manifest(dir: &Path, key: Key, phase: &str, totals: &Totals) -> std::io::Result<()> {
    let v = json!({
        "key": key.to_json(),
        "phase": phase,
        "totals": totals.to_json(),
        "store_schema": lodestar_store::latest_schema_version(),
        "search_schema": lodestar_search::schema::SCHEMA_VERSION,
    });
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).expect("json"))?;
    std::fs::rename(tmp, dir.join(MANIFEST))
}

/// Checks `dir` for `wanted` before anything is written: the manifest's
/// key and phase when it has one, `None` when it's new or empty.
pub fn check(dir: &Path, wanted: Key) -> Result<Option<(String, Totals)>, Refused> {
    if let Some(data) = std::env::var_os("LODESTAR_DATA_DIR") {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        if same(dir, Path::new(&data)) {
            return Err(Refused::DataDir(dir.into()));
        }
    }
    let Some(m) = read_manifest(dir)? else {
        let empty = match std::fs::read_dir(dir) {
            Ok(mut it) => it.next().is_none(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(Refused::BadManifest(dir.into(), e.to_string())),
        };
        return if empty {
            Ok(None)
        } else {
            Err(Refused::NotADataset(dir.into()))
        };
    };
    let found = m
        .get("key")
        .and_then(Key::from_json)
        .ok_or_else(|| Refused::BadManifest(dir.into(), "no key".into()))?;
    if found != wanted {
        return Err(Refused::Mismatch {
            dir: dir.into(),
            found,
            wanted,
        });
    }
    let phase = m
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if !phase.is_empty() && !PHASES.contains(&phase.as_str()) {
        return Err(Refused::BadManifest(
            dir.into(),
            format!("unknown phase {phase}"),
        ));
    }
    Ok(Some((
        phase,
        Totals::from_json(m.get("totals").unwrap_or(&Value::Null)),
    )))
}

/// Opens the dataset for `key` in `dir`, building it (or finishing an
/// interrupted build) first. `log` gets one line per step.
pub fn open_or_build(
    dir: &Path,
    key: Key,
    log: &mut dyn FnMut(&str),
) -> Result<Dataset, Box<dyn std::error::Error>> {
    let found = check(dir, key)?;
    let (mut phase, mut totals) = found.unwrap_or_default();
    let done = |phase: &str, step: &str| {
        PHASES.iter().position(|p| *p == phase) >= PHASES.iter().position(|p| *p == step)
            && !phase.is_empty()
    };
    if done(&phase, "complete") {
        return Ok(Dataset {
            dir: dir.into(),
            key,
            totals,
            built_now: false,
        });
    }
    std::fs::create_dir_all(dir)?;
    // A scratch copy left by an interrupted run is no part of a build.
    remove_scratch(dir)?;
    let n = usize::try_from(key.torrents)?;
    let db = dir.join(DB);
    let mut store = Store::open(&db, StoreConfig::default())?;
    let step = |phase: &mut String, name: &str, totals: &Totals| -> std::io::Result<()> {
        *phase = name.to_owned();
        write_manifest(dir, key, name, totals)
    };
    if phase.is_empty() {
        write_manifest(dir, key, "", &totals)?;
    }
    if !done(&phase, "stored") {
        totals = store_all(&mut store, key.seed, n, log)?;
        step(&mut phase, "stored", &totals)?;
    }
    if !done(&phase, "enriched") {
        let started = Instant::now();
        let mut after = 0;
        while let Some((last, _)) =
            lodestar_pipeline::enrich::enrich_stored(&mut store, after, 5000)?
        {
            after = last;
        }
        log(&format!(
            "enriched in {:.0} s",
            started.elapsed().as_secs_f64()
        ));
        step(&mut phase, "enriched", &totals)?;
    }
    if !done(&phase, "scraped") {
        scrape_all(&mut store, key.seed)?;
        step(&mut phase, "scraped", &totals)?;
    }
    if !done(&phase, "rollups") {
        year_of_rollups(&mut store)?;
        step(&mut phase, "rollups", &totals)?;
    }
    store.checkpoint()?;
    if !done(&phase, "indexed") {
        let index =
            lodestar_search::SearchIndex::open(&lodestar_search::index_dir(&dir.join("index")))?;
        index_all(&mut store, &index, log)?;
        step(&mut phase, "indexed", &totals)?;
    }
    // The store stamps two bookkeeping columns with the wall clock (when
    // each migration ran, when the dictionary was trained); nothing reads
    // them for search or sizes. Pinned, so a build is the same bytes.
    store.connection().execute_batch(&format!(
        "UPDATE schema_migrations SET applied_at = {e};
         UPDATE zstd_dicts SET created_at = {e};",
        e = synth::EPOCH
    ))?;
    store.checkpoint()?;
    drop(store);
    step(&mut phase, "complete", &totals)?;
    Ok(Dataset {
        dir: dir.into(),
        key,
        totals,
        built_now: true,
    })
}

/// Stores `n` torrents as the pipeline does: [`TRAIN`] without a
/// dictionary, then a trained one, the first ones recompressed with it,
/// and the rest with it. A resumed build skips what's stored (a crash
/// before training trains on resume; after it, the stored dictionary
/// stays active).
fn store_all(
    store: &mut Store,
    seed: u64,
    n: usize,
    log: &mut dyn FnMut(&str),
) -> Result<Totals, Box<dyn std::error::Error>> {
    let mut totals = Totals::default();
    let mut generator = Generator::new(seed);
    let have = (store.count()? as usize).min(n);
    // Totals cover the whole corpus, so the skipped part is counted too.
    for _ in 0..have {
        count(&generator.next_torrent(), &mut totals)?;
    }
    let first = TRAIN.min(n);
    if have < first {
        load(store, &mut generator, first - have, &mut totals)?;
    }
    if have <= first {
        store.train_file_dict(first, DICT_SIZE)?;
        store.recompress_files(1000)?;
        log(&format!("stored {first}, dictionary trained"));
    }
    let started = Instant::now();
    let from = have.max(first);
    let mut done = from;
    while done < n {
        let take = (n - done).min(100_000);
        load(store, &mut generator, take, &mut totals)?;
        done += take;
        log(&format!(
            "stored {done} ({:.0}/s)",
            (done - from) as f64 / started.elapsed().as_secs_f64()
        ));
    }
    Ok(totals)
}

fn count(t: &synth::SynthTorrent, totals: &mut Totals) -> Result<(), Box<dyn std::error::Error>> {
    let info = Info::parse(&t.info)?;
    totals.files += info.files.len() as u64;
    totals.info_bytes += t.info.len() as u64;
    totals.raw_tree_bytes += filetree::encode(&info.files, info.path_encoding)?.len() as u64;
    Ok(())
}

/// Generates and inserts `n` torrents in batches of 1,000, counting them
/// into `totals` (`size-report`'s load).
pub fn load(
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
                source: SourceId::new(0).expect("valid id"),
            })
            .collect();
        let t1 = Instant::now();
        store.insert(&batch)?;
        totals.store += t1.elapsed();
    }
    Ok(())
}

/// Writes one scrape result per stored torrent: about 40% of swarms are
/// live (a heavy tail of seeders, one to six of the 20 trackers reporting,
/// and history); the rest are dead.
pub fn scrape_all(store: &mut Store, seed: u64) -> Result<(), Box<dyn std::error::Error>> {
    let trackers = (0..TRACKERS)
        .map(|i| store.upsert_tracker(&format!("udp://tracker{i}.example:6969"), SCRAPED_AT))
        .collect::<Result<Vec<u16>, _>>()?;
    let mut after = 0;
    loop {
        let ids = store.ids_after(after, 5000)?;
        let Some(&last) = ids.last() else {
            return Ok(());
        };
        after = last;
        let updates: Vec<SwarmUpdate> = ids
            .iter()
            .map(|&id| synth_swarm(id, seed, &trackers))
            .collect();
        store.write(&WriteBatch {
            swarm: &updates,
            ..WriteBatch::default()
        })?;
    }
}

fn synth_swarm(id: i64, seed: u64, trackers: &[u16]) -> SwarmUpdate {
    // splitmix64: a cheap, well-mixed draw per torrent.
    let mut x = (id as u64 ^ seed).wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    let live = x % 100 < 40;
    let mut u = SwarmUpdate {
        torrent_id: id,
        at: SCRAPED_AT,
        seeders: 0,
        leechers: 0,
        completed: 0,
        trackers_reporting: 0,
        trackers_checked: trackers.len() as u16,
        dht: ((x >> 40) % 10 < 7).then_some((0, 0)),
        per_tracker: Vec::new(),
        next_scrape_at: SCRAPED_AT + 2 * 86_400,
        flags: 0,
        interest: false,
    };
    if live {
        // 70% of live swarms under 10 seeders, 25% under 100, 5% under 1,000.
        let scale = [10, 100, 1_000][((x >> 8) % 100 / 70 + (x >> 8) % 100 / 95) as usize];
        u.seeders = ((x >> 16) % scale) as u32;
        u.leechers = u.seeders / 3 + ((x >> 24) % 7) as u32;
        u.completed = u.seeders * 20 + ((x >> 32) % 100) as u32;
        let reporting = 1 + (x >> 44) % 6;
        let first = (x >> 50) as usize;
        u.per_tracker = (0..reporting as usize)
            .map(|k| TrackerCounts {
                tracker: trackers[(first + k * 7) % trackers.len()],
                seeders: u.seeders,
                leechers: u.leechers,
                completed: u.completed,
            })
            .collect();
        u.trackers_reporting = reporting as u16;
        u.dht = u.dht.map(|_| (u.seeders / 2, u.leechers / 2));
        u.next_scrape_at = SCRAPED_AT + 6 * 3600;
    }
    u
}

/// A busy crawler's hour of Insights rows: B11a's wide hour, every metric
/// code and all ten loss reasons (`examples/reporting-overhead.rs`
/// `wide_hour`): rollups as (metric, dim, key, value), cohorts as
/// (source, stage, reason, count).
type Hour = (Vec<(u32, u32, i64, i64)>, Vec<(i64, u8, u8, i64)>);

pub fn wide_hour() -> Hour {
    let mut r = Vec::new();
    for source in 0..2 {
        for strategy in 0..3 {
            r.push((1, source, strategy, 900));
            r.push((2, source, strategy, 300));
        }
        r.push((3, source, 0, 2));
        r.push((4, source, 0, 120));
    }
    for tracker in 0..8 {
        for outcome in 0..3 {
            r.push((5, tracker, outcome, 40));
        }
    }
    r.extend((0..17).map(|k| (9, 0, k, 3)));
    for t in 0..12 {
        r.push((10, t, 0, 10));
        r.push((10, t, 1, 2));
    }
    r.extend((80..110).map(|b| (11, 0, b, 4)));
    r.extend((0..2).map(|d| (12, d, 0, 3600)));
    for m in 6..=8 {
        r.extend((0..12).map(|k| (m, 0, k, 5)));
    }
    r.push((13, 0, 0, 1_000_000));
    r.push((14, 3, 0, 300));
    r.extend((1..=10).map(|reason| (14, 6, reason, 20)));
    r.extend((0..12).map(|t| (15, t, 0, 1)));
    let mut c = Vec::new();
    for source in 0..2 {
        for (stage, reason) in [(1, 0), (2, 0), (3, 0), (4, 0), (5, 0)] {
            c.push((source, stage, reason, 100));
        }
        c.extend((1..=10).map(|reason| (source, 6, reason, 10)));
    }
    (r, c)
}

/// A year of [`wide_hour`] ending at [`ANCHOR_HOUR`], with an index
/// watermark row an hour, compacted as the recorder does at the anchor
/// (30 days kept hourly).
fn year_of_rollups(store: &mut Store) -> Result<(), Box<dyn std::error::Error>> {
    let hour = wide_hour();
    {
        let c = store.connection();
        c.execute_batch("BEGIN")?;
        let mut r = c.prepare(
            "INSERT OR REPLACE INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut k = c.prepare(
            "INSERT OR REPLACE INTO cohort_hour (cohort, source, stage, reason, count) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut w = c.prepare(
            "INSERT OR REPLACE INTO index_watermark_hour (hour, last_id) VALUES (?1, ?2)",
        )?;
        for h in ANCHOR_HOUR - 365 * 24..ANCHOR_HOUR {
            for &(m, d, key, v) in &hour.0 {
                r.execute((h, m, d, key, v))?;
            }
            for &(s, st, re, n) in &hour.1 {
                k.execute((h, s, st, re, n))?;
            }
            w.execute((h, h * 100))?;
        }
        drop((r, k, w));
        c.execute_batch("COMMIT")?;
    }
    let r = store.compact_rollups(CompactOptions {
        now: ANCHOR_HOUR * 3600,
        keep_days: 30,
        hold_from_day: None,
        budget: Duration::from_secs(24 * 3600),
        kind: &metric::aggregation,
    })?;
    if !r.done {
        return Err("rollup compaction didn't finish".into());
    }
    Ok(())
}

/// Indexes the whole store into `index`, committing every
/// [`COMMIT_EVERY`] torrents.
fn index_all(
    store: &mut Store,
    index: &lodestar_search::SearchIndex,
    log: &mut dyn FnMut(&str),
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut indexer = index.writer(INDEX_HEAP)?;
    loop {
        let added = indexer.catch_up(store, 20_000)?;
        if added == 0 {
            break;
        }
        if indexer.pending() >= COMMIT_EVERY {
            indexer.commit()?;
            log(&format!(
                "indexed up to id {} ({:.0}/s)",
                indexer.last_id(),
                indexer.last_id() as f64 / started.elapsed().as_secs_f64()
            ));
        }
    }
    indexer.commit()?;
    indexer.finish()?;
    Ok(())
}

/// A digest of the dataset's content: every torrent row, file tree and
/// swarm row in id order, the dictionary, and the rollups, by FNV-1a.
/// Equal for two builds with the same key; it names no file's bytes, so
/// it also checks a copy.
pub fn digest(db: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    use lodestar_store::rusqlite::{Connection, OpenFlags, types::ValueRef};
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    for table in [
        "torrents",
        "torrent_files",
        "zstd_dicts",
        "swarm",
        "swarm_trackers",
        "swarm_history",
        "trackers",
        "rollup_hour",
        "cohort_hour",
        "rollup_day",
        "cohort_day",
    ] {
        eat(table.as_bytes());
        let mut stmt = c.prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))?;
        let cols = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            for i in 0..cols {
                match row.get_ref(i)? {
                    ValueRef::Null => eat(b"N"),
                    ValueRef::Integer(v) => eat(&v.to_le_bytes()),
                    ValueRef::Real(v) => eat(&v.to_le_bytes()),
                    ValueRef::Text(t) | ValueRef::Blob(t) => eat(t),
                }
            }
        }
    }
    Ok(h)
}

/// The folder writing rows run in: a copy of the store and indexes, made
/// fresh by [`scratch`].
pub fn scratch_dir(dir: &Path) -> PathBuf {
    dir.join("scratch")
}

fn remove_scratch(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(scratch_dir(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A fresh copy of `ds`'s store and indexes to write to (the copy
/// reflinks where the filesystem can). Returns its folder, laid out as
/// the dataset is; the caller removes it with [`drop_scratch`].
pub fn scratch(ds: &Dataset) -> std::io::Result<PathBuf> {
    remove_scratch(&ds.dir)?;
    let to = scratch_dir(&ds.dir);
    std::fs::create_dir_all(&to)?;
    std::fs::copy(ds.db(), to.join(DB))?;
    copy_tree(&ds.index_root(), &to.join("index"))?;
    Ok(to)
}

/// Removes the scratch copy.
pub fn drop_scratch(ds: &Dataset) -> std::io::Result<()> {
    remove_scratch(&ds.dir)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

// ── The report ──────────────────────────────────────────────────────────

/// The JSON report's format. Raise it when a field changes meaning.
pub const REPORT_FORMAT: u32 = 1;

/// A row's outcome. Only [`Status::Pass`] and [`Status::Reported`] (no
/// budget) and [`Status::Off`] (not offered in this mode, by design)
/// leave a report able to pass.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Pass,
    Fail,
    /// Measured; §3 sets no number for it.
    Reported,
    /// The build can't do it yet (the reason says what's missing).
    Unavailable(String),
    /// It ran but the value can't be trusted (the reason says why).
    Unmeasured(String),
    /// Not offered in this mode by design (low-resource regex and file
    /// queries).
    Off(String),
}

/// One measured line.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Stable, e.g. `search.keyword.1-common-word.p95`.
    pub id: String,
    /// The §3 row it answers to, e.g. "Keyword search, top 50".
    pub budget_row: &'static str,
    pub value: Option<f64>,
    pub unit: &'static str,
    /// Passes when `value <= budget`.
    pub budget: Option<f64>,
    pub status: Status,
}

impl Row {
    /// A value against an upper bound (`None`: reported only).
    pub fn measured(
        id: impl Into<String>,
        budget_row: &'static str,
        value: f64,
        unit: &'static str,
        budget: Option<f64>,
    ) -> Row {
        let status = match budget {
            _ if !value.is_finite() || value < 0.0 => {
                Status::Unmeasured(format!("value {value} is not a measurement"))
            }
            Some(b) if value <= b => Status::Pass,
            Some(_) => Status::Fail,
            None => Status::Reported,
        };
        Row {
            id: id.into(),
            budget_row,
            value: Some(value),
            unit,
            budget,
            status,
        }
    }

    /// A row with no value.
    pub fn without(
        id: impl Into<String>,
        budget_row: &'static str,
        unit: &'static str,
        budget: Option<f64>,
        status: Status,
    ) -> Row {
        Row {
            id: id.into(),
            budget_row,
            value: None,
            unit,
            budget,
            status,
        }
    }

    fn to_json(&self) -> Value {
        let (status, reason) = match &self.status {
            Status::Pass => ("pass", None),
            Status::Fail => ("fail", None),
            Status::Reported => ("reported", None),
            Status::Unavailable(r) => ("unavailable", Some(r)),
            Status::Unmeasured(r) => ("unmeasured", Some(r)),
            Status::Off(r) => ("off", Some(r)),
        };
        json!({
            "id": self.id,
            "budget_row": self.budget_row,
            "value": self.value,
            "unit": self.unit,
            "budget": self.budget,
            "status": status,
            "reason": reason,
        })
    }
}

/// The verdict over every row: `fail` if any row failed, else
/// `incomplete` if any is unavailable or unmeasured, else `pass`.
pub fn verdict(rows: &[Row]) -> &'static str {
    if rows.iter().any(|r| r.status == Status::Fail) {
        "fail"
    } else if rows
        .iter()
        .any(|r| matches!(r.status, Status::Unavailable(_) | Status::Unmeasured(_)))
    {
        "incomplete"
    } else {
        "pass"
    }
}

/// The whole report: the machine, the dataset, the revision, the rows.
pub fn report(tool: &str, mode: &str, key: Option<Key>, dir: Option<&Path>, rows: &[Row]) -> Value {
    json!({
        "report_format": REPORT_FORMAT,
        "tool": tool,
        "mode": mode,
        "verdict": verdict(rows),
        "machine": machine(dir),
        "dataset": key.map(Key::to_json),
        "git": git(),
        "rows": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
    })
}

/// Writes `report` to `path` (through a temporary file, so a reader
/// never sees half of one).
pub fn write_report(path: &Path, report: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(report).expect("json"))?;
    std::fs::rename(tmp, path)
}

/// Cores, memory, CPU, kernel and the dataset disk's kind. Linux reads
/// `/proc` and `/sys`; anything unreadable is `null`, never a guess.
pub fn machine(dir: Option<&Path>) -> Value {
    let read = |p: &str| std::fs::read_to_string(p).ok();
    let meminfo = read("/proc/meminfo");
    let mem = meminfo.as_deref().and_then(|m| {
        let kb: u64 = m
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(kb * 1024)
    });
    let cpu = read("/proc/cpuinfo").and_then(|c| {
        c.lines()
            .find_map(|l| l.strip_prefix("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
    });
    json!({
        "cores": std::thread::available_parallelism().ok().map(|n| n.get()),
        "memory_bytes": mem,
        "cpu": cpu,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "kernel": read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned()),
        "disk": dir.map(disk_kind),
    })
}

/// "nvme", "ssd", "hdd" or "unknown" for the block device holding `dir`
/// (an overlay or network filesystem has none: "unknown").
pub fn disk_kind(dir: &Path) -> &'static str {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(meta) = std::fs::metadata(dir) else {
            return "unknown";
        };
        let dev = meta.dev();
        let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
        let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
        let Ok(sys) = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")) else {
            return "unknown";
        };
        // A partition's queue is its disk's.
        let rotational = [
            sys.join("queue/rotational"),
            sys.join("../queue/rotational"),
        ]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok());
        if sys.to_string_lossy().contains("nvme") {
            return "nvme";
        }
        match rotational.as_deref().map(str::trim) {
            Some("0") => "ssd",
            Some("1") => "hdd",
            _ => "unknown",
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        "unknown"
    }
}

/// The revision measured: `$GITHUB_SHA` in CI, else `git rev-parse`, and
/// whether the tree had changes.
pub fn git() -> Value {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let rev = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| run(&["rev-parse", "HEAD"]));
    let dirty = run(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    json!({ "rev": rev, "dirty": dirty })
}

/// The process's resident set (Linux; `None` elsewhere).
pub fn rss() -> Option<u64> {
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
