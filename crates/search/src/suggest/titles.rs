//! The parsed-title FST (§8, B7): every effective and alternative title
//! in the store, deduplicated case-insensitively, with how many torrents
//! carry it in each visibility class, so typeahead can complete a title
//! by prefix and rank by frequency without touching the index.
//!
//! **Shape.** One [`Map`] from `fold(title) \0 display` to four 16-bit
//! counts (saturating), one per class: neither Adult nor hidden by a
//! filter rule, Adult, hidden, both. A request adds up the classes its
//! visibility shows, so a title only Adult or hidden torrents carry has
//! a count of 0 there and is never offered. `display` is the spelling
//! most torrents use.
//!
//! **Ranking.** A prefix with at most [`SCAN_CAP`] titles is ranked by
//! scanning its range. Above that, the best [`TOP`] per visibility are
//! precomputed at build time ("big" prefixes): every prefix whose range
//! is larger than the cap is one, so a lookup never scans more than the
//! cap.
//!
//! **Rebuilds** run on a thread of their own, never on the commit path:
//! the live indexer reports each commit ([`Titles::committed`]), and the
//! builder rebuilds from a read-only store connection at most once per
//! [`MIN_INTERVAL`] (or ten build times, if longer), and not while the
//! live indexer has a backlog (an import's drain): then once it has
//! caught up. The set belongs to one [`SearchIndex`](crate::SearchIndex),
//! so an index swap starts a new one for the new directory. A restart
//! rebuilds it, and until a build finishes there are no title
//! suggestions.
//!
//! **Memory** (X-import-drain-memory). A directory index's set is a file
//! in [`TITLES_DIR`] beside it, memory-mapped, so only the pages lookups
//! touch are resident. The build holds at most about [`RUN_BYTES`] of
//! spellings: it sorts them into runs spilled to that folder, merges the
//! runs straight into the file, and computes the precomputed rankings in
//! the same pass. The file is a cache, never a source of truth: each
//! build writes a new generation, checked as it's mapped (a bad file
//! fails the build, which keeps the old set and tries again), and the
//! old one is removed once nothing maps it. Each build first sweeps what
//! a crashed or failed build left.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestar_classify::{ContentType, Record};
use lodestar_store::{Store, StoreConfig, flags};
use tantivy::directory::{Directory, MmapDirectory, OwnedBytes};
use tantivy_fst::{IntoStreamer, Map, MapBuilder, Streamer};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::index::{Error, Result};

/// Titles a lookup scans at most; wider prefixes are precomputed.
pub const SCAN_CAP: usize = 4096;
/// Titles kept per precomputed prefix and visibility.
pub const TOP: usize = 24;
/// The least time between rebuilds.
pub const MIN_INTERVAL: Duration = Duration::from_secs(60);
/// Titles longer than this (folded, in bytes) aren't offered: they're
/// junk names more than titles anyone types.
const MAX_TITLE_BYTES: usize = 160;
/// Records read per query while building.
const READ_CHUNK: usize = if cfg!(test) { 3 } else { 20_000 };
/// The folder beside an index that holds its title sets and the runs of
/// a build in progress.
pub const TITLES_DIR: &str = "titles";
/// A build's runs folder, and a set file, are named for their generation.
const BUILD_PREFIX: &str = "build-";
const SET_PREFIX: &str = "set-";
const SET_SUFFIX: &str = ".fst";

/// What a request may see: which classes its counts add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub adult: bool,
    pub hidden: bool,
}

impl Shown {
    fn combo(self) -> usize {
        usize::from(self.adult) | (usize::from(self.hidden) << 1)
    }
}

/// Class of a torrent: bit 0 Adult, bit 1 hidden.
fn class(adult: bool, hidden: bool) -> usize {
    usize::from(adult) | (usize::from(hidden) << 1)
}

fn pack(counts: [u32; 4]) -> u64 {
    counts
        .iter()
        .enumerate()
        .fold(0, |p, (i, c)| p | (u64::from((*c).min(0xFFFF)) << (16 * i)))
}

/// Torrents a packed entry counts under the visibility `combo`: the
/// classes that are subsets of it.
fn visible(packed: u64, combo: usize) -> u32 {
    (0..4)
        .filter(|c| c & !combo == 0)
        .map(|c| ((packed >> (16 * c)) & 0xFFFF) as u32)
        .sum()
}

/// Folds text for matching: NFKD with marks removed, lowercase, and
/// every run of anything but letters and digits as one space, trimmed.
/// With `open_end`, a trailing separator is kept as one space, so a
/// prefix ending in a space only matches at a word boundary.
pub fn fold(s: &str, open_end: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.nfkd().filter(|c| !is_combining_mark(*c)) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    if open_end && gap && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// A title offered for a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    /// As most torrents spell it.
    pub display: String,
    /// Torrents with it that the request may see.
    pub count: u32,
}

/// The titles of one build.
pub struct TitleSet {
    /// In memory, or mapped from [`TitleSet::file`].
    fst: Map<OwnedBytes>,
    /// Precomputed rankings: folded prefix → per visibility, indexes
    /// into `pinned`.
    big: HashMap<Box<str>, [Box<[u32]>; 4]>,
    pinned: Vec<(Box<str>, u64)>,
    torrents: u64,
    took: Duration,
    /// The set's file, for a set built in a folder. Declared last, so the
    /// mapping above is dropped before the file is removed (Windows
    /// refuses to remove a mapped file).
    file: Option<SetFile>,
}

impl std::fmt::Debug for TitleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TitleSet")
            .field("titles", &self.len())
            .field("torrents", &self.torrents)
            .field("bytes", &self.memory_bytes())
            .finish()
    }
}

/// One spelling of a title while building: `fold \0 display`, the
/// final key's form, so the build holds each spelling once.
type Spelling = Box<[u8]>;

/// One title as the set holds it: its key (the spelling most torrents
/// use), the folded part's length and the packed counts.
type Entry = (Spelling, usize, u64);

/// Spellings a build holds in memory before it sorts them into a run and
/// spills it, so a build's memory is about this much whatever the
/// store's size (X-import-drain-memory).
const RUN_BYTES: usize = if cfg!(test) { 512 } else { 32 << 20 };
/// What one held spelling costs beside its bytes (its box, its counts
/// and its map slot), counted against [`RUN_BYTES`].
const ENTRY_OVERHEAD: usize = 56;

/// The set file: this header, then the FST. The header is the magic,
/// the format version, 4 reserved bytes, the title count and the FST's
/// length, little-endian.
const MAGIC: &[u8; 8] = b"LDSTITLE";
const FORMAT: u32 = 1;
const HEADER: usize = 32;

impl TitleSet {
    /// Every title in `store`, built in memory (tests and RAM indexes).
    pub fn build(store: &Store) -> Result<TitleSet> {
        TitleSet::build_in(store, None)
    }

    /// Every title in `store`. With `dir`, the build spills its sorted
    /// runs there and the set is a file there, memory-mapped: the build
    /// holds about [`RUN_BYTES`] and the set only its precomputed
    /// rankings, whatever the store's size. A failed build removes what
    /// it wrote.
    pub fn build_in(store: &Store, dir: Option<&Path>) -> Result<TitleSet> {
        TitleSet::build_with_runs(store, dir, RUN_BYTES)
    }

    /// [`TitleSet::build_in`] with runs of `run_bytes`, so the bench
    /// (`mem-titles`) spans many runs on a small store.
    #[doc(hidden)]
    pub fn build_with_runs(
        store: &Store,
        dir: Option<&Path>,
        run_bytes: usize,
    ) -> Result<TitleSet> {
        let started = Instant::now();
        let adult = ContentType::Adult.code();
        let mut work = Work::new(dir).map_err(Error::Io)?;
        let mut runs: Vec<Run> = Vec::new();
        let mut held: HashMap<Spelling, [u32; 4]> = HashMap::new();
        let mut held_bytes = 0usize;
        let mut torrents = 0u64;
        let mut seen: Vec<String> = Vec::new();
        // In chunks by id, each its own read: one statement over the whole
        // table would hold a read transaction for the whole build, and
        // with it every WAL checkpoint.
        let mut stmt = store
            .connection()
            .prepare_cached(
                "SELECT p.torrent_id, t.flags, t.content_type, p.data
                 FROM torrent_parsed p JOIN torrents t ON t.id = p.torrent_id
                 WHERE p.torrent_id > ?1 ORDER BY p.torrent_id LIMIT ?2",
            )
            .map_err(lodestar_store::Error::from)?;
        let mut after = 0i64;
        loop {
            let mut rows = stmt
                .query(lodestar_store::rusqlite::params![after, READ_CHUNK as i64])
                .map_err(lodestar_store::Error::from)?;
            let mut read = 0;
            while let Some(row) = rows.next().map_err(lodestar_store::Error::from)? {
                read += 1;
                after = row.get(0).map_err(lodestar_store::Error::from)?;
                let flags: i64 = row.get(1).map_err(lodestar_store::Error::from)?;
                let ct: Option<u8> = row.get(2).map_err(lodestar_store::Error::from)?;
                let data: Vec<u8> = row.get(3).map_err(lodestar_store::Error::from)?;
                let Ok(record) = Record::decode(&data) else {
                    continue;
                };
                torrents += 1;
                let c = class(ct == Some(adult), flags & flags::HIDDEN != 0);
                seen.clear();
                let titles = record
                    .release("title")
                    .into_iter()
                    .chain(record.release_all("alt"));
                for title in titles {
                    let key = fold(title, false);
                    if key.is_empty() || key.len() > MAX_TITLE_BYTES || seen.contains(&key) {
                        continue;
                    }
                    let display = title.split_whitespace().collect::<Vec<_>>().join(" ");
                    let mut k = Vec::with_capacity(key.len() + 1 + display.len());
                    k.extend_from_slice(key.as_bytes());
                    k.push(0);
                    k.extend_from_slice(display.as_bytes());
                    let counts = match held.entry(k.into_boxed_slice()) {
                        std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                        std::collections::hash_map::Entry::Vacant(v) => {
                            held_bytes += v.key().len() + ENTRY_OVERHEAD;
                            v.insert([0; 4])
                        }
                    };
                    counts[c] = counts[c].saturating_add(1);
                    seen.push(key);
                }
                if held_bytes >= run_bytes {
                    runs.push(work.spill(&mut held).map_err(Error::Io)?);
                    held_bytes = 0;
                }
            }
            if read < READ_CHUNK {
                break;
            }
        }
        drop(stmt);
        if !held.is_empty() || runs.is_empty() {
            runs.push(work.spill(&mut held).map_err(Error::Io)?);
        }
        drop(held);
        let (fst, file, (big, pinned)) = assemble(&mut work, runs)?;
        Ok(TitleSet {
            fst,
            big,
            pinned,
            torrents,
            took: started.elapsed(),
            file,
        })
    }

    /// Distinct titles.
    pub fn len(&self) -> usize {
        self.fst.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fst.is_empty()
    }

    /// Torrents with a readable record when it was built.
    pub fn torrents(&self) -> u64 {
        self.torrents
    }

    /// How long the build took.
    pub fn took(&self) -> Duration {
        self.took
    }

    /// Memory held, in bytes (the precomputed rankings and the titles
    /// they name, and the FST when it isn't a mapped file), for the
    /// budget (§3).
    pub fn memory_bytes(&self) -> usize {
        let fst = if self.file.is_some() {
            0
        } else {
            self.fst.as_fst().size()
        };
        let big: usize = self
            .big
            .iter()
            .map(|(k, lists)| {
                k.len() + 16 + lists.iter().map(|l| l.len() * 4 + 16).sum::<usize>() + 32
            })
            .sum();
        let pinned: usize = self.pinned.iter().map(|(d, _)| d.len() + 24).sum();
        fst + big + pinned
    }

    /// The titles starting with the folded `prefix` that `shown` may
    /// see, most torrents first (then by folded title), at most `limit`.
    pub fn lookup(&self, prefix: &str, shown: Shown, limit: usize) -> Vec<Title> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let combo = shown.combo();
        if let Some(lists) = self.big.get(prefix) {
            return lists[combo]
                .iter()
                .take(limit)
                .map(|&i| {
                    let (display, packed) = &self.pinned[i as usize];
                    Title {
                        display: display.to_string(),
                        count: visible(*packed, combo),
                    }
                })
                .collect();
        }
        let mut found: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut stream = self.fst.range().ge(prefix.as_bytes()).into_stream();
        let mut scanned = 0;
        while let Some((key, packed)) = stream.next() {
            if !key.starts_with(prefix.as_bytes()) || scanned == SCAN_CAP {
                break;
            }
            scanned += 1;
            let n = visible(packed, combo);
            if n > 0 {
                found.push((n, key.to_vec()));
            }
        }
        found.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        found
            .into_iter()
            .take(limit)
            .filter_map(|(count, key)| {
                let at = key.iter().position(|b| *b == 0)?;
                Some(Title {
                    display: String::from_utf8(key[at + 1..].to_vec()).ok()?,
                    count,
                })
            })
            .collect()
    }
}

/// A set's parts from its runs: they're merged in key order, grouped into
/// titles and streamed into the FST and the precomputed rankings.
fn assemble(
    work: &mut Work,
    runs: Vec<Run>,
) -> Result<(Map<OwnedBytes>, Option<SetFile>, Rankings)> {
    let mut merge = Merge::new(runs).map_err(Error::Io)?;
    let mut sink = Sink::new(work)?;
    let mut group = Grouper::default();
    let mut big = BigBuilder::default();
    let mut titles = 0u64;
    let mut emit = |e: Entry| -> Result<()> {
        sink.insert(&e.0, e.2)?;
        titles += 1;
        big.push(&e);
        Ok(())
    };
    while let Some((key, counts)) = merge.next().map_err(Error::Io)? {
        if let Some(e) = group.push(key, counts) {
            emit(e)?;
        }
    }
    if let Some(e) = group.finish() {
        emit(e)?;
    }
    drop(merge);
    let rankings = big.finish();
    let (fst, file) = sink.finish(work, titles)?;
    Ok((fst, file, rankings))
}

/// The precomputed rankings: prefix → per visibility, indexes into the
/// pinned titles.
type Rankings = (HashMap<Box<str>, [Box<[u32]>; 4]>, Vec<(Box<str>, u64)>);

fn fst_error(e: tantivy_fst::Error) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

fn folded_len(k: &[u8]) -> usize {
    k.iter().position(|b| *b == 0).unwrap_or(k.len())
}

/// Groups the spellings of each title, in key order, into one entry: the
/// counts added up and the most used spelling kept (the first in byte
/// order on a tie). Spellings of a title are adjacent, sharing the
/// folded key.
#[derive(Default)]
struct Grouper {
    group: Option<(Spelling, u32, [u32; 4])>,
}

impl Grouper {
    /// Takes the next spelling; returns the previous title once it's done.
    fn push(&mut self, key: Spelling, counts: [u32; 4]) -> Option<Entry> {
        let n: u32 = counts.iter().fold(0u32, |a, c| a.saturating_add(*c));
        if let Some((best, best_n, total)) = &mut self.group
            && best[..folded_len(best)] == key[..folded_len(&key)]
        {
            for (t, c) in total.iter_mut().zip(counts) {
                *t = t.saturating_add(c);
            }
            if n > *best_n {
                *best = key;
                *best_n = n;
            }
            return None;
        }
        self.group.replace((key, n, counts)).map(Self::entry)
    }

    /// The last title.
    fn finish(&mut self) -> Option<Entry> {
        self.group.take().map(Self::entry)
    }

    fn entry((key, _, counts): (Spelling, u32, [u32; 4])) -> Entry {
        let folded = folded_len(&key);
        (key, folded, pack(counts))
    }
}

/// One sorted run of spellings: `[u32 key length][key][4 × u32 counts]`
/// per spelling, little-endian, in key order, each key once.
enum Run {
    Mem(Vec<u8>),
    File(PathBuf),
}

/// What one build writes, in its folder: a runs folder, removed when the
/// build ends however it ends, and the set file, removed too unless the
/// build hands it to its set. Without a folder, everything is in memory.
struct Work {
    runs: Option<PathBuf>,
    set: Option<PathBuf>,
    spilled: usize,
}

impl Work {
    fn new(dir: Option<&Path>) -> std::io::Result<Work> {
        let Some(dir) = dir else {
            return Ok(Work {
                runs: None,
                set: None,
                spilled: 0,
            });
        };
        std::fs::create_dir_all(dir)?;
        let generation = generation();
        let runs = dir.join(format!("{BUILD_PREFIX}{generation}"));
        std::fs::create_dir(&runs)?;
        Ok(Work {
            runs: Some(runs),
            set: Some(dir.join(format!("{SET_PREFIX}{generation}{SET_SUFFIX}"))),
            spilled: 0,
        })
    }

    /// Sorts the held spellings into a run, emptying `held`.
    fn spill(&mut self, held: &mut HashMap<Spelling, [u32; 4]>) -> std::io::Result<Run> {
        let mut sorted: Vec<(Spelling, [u32; 4])> = held.drain().collect();
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let write = |w: &mut dyn Write| -> std::io::Result<()> {
            for (key, counts) in &sorted {
                let len = u32::try_from(key.len()).map_err(std::io::Error::other)?;
                w.write_all(&len.to_le_bytes())?;
                w.write_all(key)?;
                for c in counts {
                    w.write_all(&c.to_le_bytes())?;
                }
            }
            Ok(())
        };
        let Some(runs) = &self.runs else {
            let mut buf = Vec::new();
            write(&mut buf)?;
            return Ok(Run::Mem(buf));
        };
        let path = runs.join(format!("run-{}", self.spilled));
        self.spilled += 1;
        let mut w = BufWriter::new(File::create(&path)?);
        write(&mut w)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(Run::File(path))
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        if let Some(runs) = &self.runs {
            let _ = std::fs::remove_dir_all(runs);
        }
        if let Some(set) = &self.set {
            let _ = std::fs::remove_file(set);
        }
    }
}

/// A process-unique name for a build's files.
fn generation() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{:x}-{nanos:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Removes what earlier builds left in `dir` (runs and sets of a crashed
/// or failed build, sets a mapping kept from being deleted), except
/// `keep`. What can't be removed yet stays for the next sweep.
fn sweep(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if Some(path.as_path()) == keep {
            continue;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(BUILD_PREFIX) {
            let _ = std::fs::remove_dir_all(&path);
        } else if name.starts_with(SET_PREFIX) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Reads one run back.
struct RunReader(Box<dyn Read + Send>);

impl RunReader {
    fn open(run: Run) -> std::io::Result<RunReader> {
        Ok(RunReader(match run {
            Run::Mem(buf) => Box::new(std::io::Cursor::new(buf)),
            Run::File(path) => Box::new(BufReader::new(File::open(path)?)),
        }))
    }

    fn next(&mut self) -> std::io::Result<Option<(Spelling, [u32; 4])>> {
        let mut len = [0u8; 4];
        match self.0.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let mut key = vec![0u8; u32::from_le_bytes(len) as usize];
        self.0.read_exact(&mut key)?;
        let mut counts = [0u32; 4];
        for c in &mut counts {
            let mut b = [0u8; 4];
            self.0.read_exact(&mut b)?;
            *c = u32::from_le_bytes(b);
        }
        Ok(Some((key.into_boxed_slice(), counts)))
    }
}

/// The runs merged into one stream in key order, each spelling once
/// with its counts added up across the runs.
struct Merge {
    readers: Vec<RunReader>,
    counts: Vec<[u32; 4]>,
    heads: BinaryHeap<Reverse<(Spelling, usize)>>,
}

impl Merge {
    fn new(runs: Vec<Run>) -> std::io::Result<Merge> {
        let mut m = Merge {
            readers: Vec::with_capacity(runs.len()),
            counts: vec![[0; 4]; runs.len()],
            heads: BinaryHeap::with_capacity(runs.len()),
        };
        for run in runs {
            m.readers.push(RunReader::open(run)?);
            m.advance(m.readers.len() - 1)?;
        }
        Ok(m)
    }

    fn advance(&mut self, i: usize) -> std::io::Result<()> {
        if let Some((key, counts)) = self.readers[i].next()? {
            self.counts[i] = counts;
            self.heads.push(Reverse((key, i)));
        }
        Ok(())
    }

    fn next(&mut self) -> std::io::Result<Option<(Spelling, [u32; 4])>> {
        let Some(Reverse((key, i))) = self.heads.pop() else {
            return Ok(None);
        };
        let mut total = self.counts[i];
        self.advance(i)?;
        while let Some(Reverse((k, j))) = self.heads.peek()
            && *k == key
        {
            let j = *j;
            self.heads.pop();
            for (t, c) in total.iter_mut().zip(self.counts[j]) {
                *t = t.saturating_add(c);
            }
            self.advance(j)?;
        }
        Ok(Some((key, total)))
    }
}

/// Where the FST goes: memory, or the set file after its header.
enum Sink {
    Mem(MapBuilder<Vec<u8>>),
    File(MapBuilder<BufWriter<File>>),
}

impl Sink {
    fn new(work: &Work) -> Result<Sink> {
        Ok(match &work.set {
            None => Sink::Mem(MapBuilder::memory()),
            Some(path) => {
                let mut w = BufWriter::new(File::create(path).map_err(Error::Io)?);
                w.write_all(&[0; HEADER]).map_err(Error::Io)?;
                Sink::File(MapBuilder::new(w).map_err(fst_error)?)
            }
        })
    }

    fn insert(&mut self, key: &[u8], value: u64) -> Result<()> {
        match self {
            Sink::Mem(b) => b.insert(key, value),
            Sink::File(b) => b.insert(key, value),
        }
        .map_err(fst_error)
    }

    /// Finishes the FST: in memory, or written out (header last, then
    /// synced to disk) and mapped back, its file handed to the set.
    fn finish(self, work: &mut Work, titles: u64) -> Result<(Map<OwnedBytes>, Option<SetFile>)> {
        let b = match self {
            Sink::Mem(b) => {
                let bytes = OwnedBytes::new(b.into_inner().map_err(fst_error)?);
                return Ok((open_fst(bytes)?, None));
            }
            Sink::File(b) => b,
        };
        let mut file = b
            .into_inner()
            .map_err(fst_error)?
            .into_inner()
            .map_err(|e| Error::Io(e.into_error()))?;
        let fst_len = file.stream_position().map_err(Error::Io)? - HEADER as u64;
        file.seek(SeekFrom::Start(0)).map_err(Error::Io)?;
        file.write_all(&header(titles, fst_len))
            .map_err(Error::Io)?;
        file.sync_all().map_err(Error::Io)?;
        drop(file);
        let path = work.set.take().expect("a file sink has its path");
        // Handed over first: if the open fails, the guard removes it.
        let file = SetFile(path);
        Ok((open_set(&file.0)?, Some(file)))
    }
}

fn header(titles: u64, fst_len: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[16..24].copy_from_slice(&titles.to_le_bytes());
    h[24..32].copy_from_slice(&fst_len.to_le_bytes());
    h
}

fn open_fst(bytes: OwnedBytes) -> Result<Map<OwnedBytes>> {
    Ok(Map::from(
        tantivy_fst::raw::Fst::new(bytes).map_err(fst_error)?,
    ))
}

/// Maps a set file, checking its header, its length and its title count.
/// A missing, short, foreign or mismatched file is an error, so the
/// builder keeps the set it has and tries again later.
fn open_set(path: &Path) -> Result<Map<OwnedBytes>> {
    let bad = || Error::Corrupt("title set file");
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(bad());
    };
    let all = MmapDirectory::open(dir)?
        .open_read(Path::new(name))
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?
        .read_bytes()
        .map_err(Error::Io)?;
    if all.len() < HEADER {
        return Err(bad());
    }
    let h = &all[..HEADER];
    let word = |at: usize| u64::from_le_bytes(h[at..at + 8].try_into().expect("8 bytes"));
    if &h[..8] != MAGIC
        || h[8..12] != FORMAT.to_le_bytes()
        || word(24) != (all.len() - HEADER) as u64
    {
        return Err(bad());
    }
    let fst = open_fst(all.slice(HEADER..all.len()))?;
    if fst.len() as u64 != word(16) {
        return Err(bad());
    }
    Ok(fst)
}

/// A set's file, removed once the set (and with it the mapping) is gone.
struct SetFile(PathBuf);

impl Drop for SetFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One precomputed-ranking candidate, ordered worst last, so a heap's
/// top is its worst: fewest torrents, then latest in key order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Top {
    fewer: Reverse<u32>,
    ord: u64,
    title: Rc<(Box<str>, u64)>,
}

/// A prefix of the current title: its length in bytes, the titles under
/// it so far and its best per visibility.
struct Open {
    len: usize,
    count: usize,
    tops: [BinaryHeap<Top>; 4],
}

/// The precomputed rankings (every folded prefix, in whole chars, that
/// more than [`SCAN_CAP`] titles share, with its best [`TOP`] per
/// visibility), from the entries in key order. It holds only the
/// prefixes of the current title, so its memory is bounded by the
/// longest title and the rankings it keeps.
#[derive(Default)]
struct BigBuilder {
    current: Vec<u8>,
    open: Vec<Open>,
    ord: u64,
    big: HashMap<Box<str>, [Box<[u32]>; 4]>,
    pinned: Vec<(Box<str>, u64)>,
    pin_of: HashMap<u64, u32>,
}

impl BigBuilder {
    fn push(&mut self, (key, folded, packed): &Entry) {
        let f = &key[..*folded];
        let common = self
            .current
            .iter()
            .zip(f)
            .take_while(|(a, b)| a == b)
            .count();
        // Prefixes of the last title that this one doesn't share are done.
        while self.open.last().is_some_and(|o| o.len > common) {
            let o = self.open.pop().expect("checked");
            self.close(o);
        }
        let from = self.open.last().map_or(0, |o| o.len);
        self.current.clear();
        self.current.extend_from_slice(f);
        let text = String::from_utf8_lossy(f);
        for (i, c) in text.char_indices() {
            let len = i + c.len_utf8();
            if len > from {
                self.open.push(Open {
                    len,
                    count: 0,
                    tops: Default::default(),
                });
            }
        }
        let visible: [u32; 4] = std::array::from_fn(|combo| visible(*packed, combo));
        let mut title: Option<Rc<(Box<str>, u64)>> = None;
        for o in &mut self.open {
            o.count += 1;
            for (heap, &v) in o.tops.iter_mut().zip(&visible) {
                if v == 0 {
                    continue;
                }
                let full = heap.len() >= TOP;
                // A later title ties worse, so it must beat the worst.
                if full && heap.peek().is_some_and(|w| w.fewer.0 >= v) {
                    continue;
                }
                let title = title
                    .get_or_insert_with(|| {
                        let display = String::from_utf8_lossy(&key[folded + 1..]);
                        Rc::new((display.into(), *packed))
                    })
                    .clone();
                if full {
                    heap.pop();
                }
                heap.push(Top {
                    fewer: Reverse(v),
                    ord: self.ord,
                    title,
                });
            }
        }
        self.ord += 1;
    }

    fn close(&mut self, o: Open) {
        if o.count <= SCAN_CAP {
            return;
        }
        let prefix: Box<str> = String::from_utf8_lossy(&self.current[..o.len]).into();
        let (pinned, pin_of) = (&mut self.pinned, &mut self.pin_of);
        let lists = o.tops.map(|heap| {
            heap.into_sorted_vec()
                .into_iter()
                .map(|t| {
                    *pin_of.entry(t.ord).or_insert_with(|| {
                        pinned.push((t.title.0.clone(), t.title.1));
                        (pinned.len() - 1) as u32
                    })
                })
                .collect()
        });
        self.big.insert(prefix, lists);
    }

    fn finish(mut self) -> Rankings {
        while let Some(o) = self.open.pop() {
            self.close(o);
        }
        (self.big, self.pinned)
    }
}

/// The title set of one index, rebuilt in the background as it commits.
pub struct Titles {
    current: RwLock<Option<Arc<TitleSet>>>,
    state: Mutex<State>,
    /// Where its sets are built ([`TITLES_DIR`] beside a directory
    /// index); `None` builds them in memory (a RAM index).
    dir: Option<PathBuf>,
}

struct State {
    /// The store to build from (its path), once the live indexer says.
    source: Option<PathBuf>,
    /// Commits since the last build started.
    dirty: bool,
    /// The live indexer has a backlog (a drain): builds wait until it's
    /// caught up, and the commits meanwhile keep the set dirty.
    behind: bool,
    /// A builder thread is running (building or waiting out the
    /// interval).
    running: bool,
    interval: Duration,
}

impl Default for Titles {
    fn default() -> Self {
        Titles {
            current: RwLock::new(None),
            state: Mutex::new(State {
                source: None,
                dirty: false,
                behind: false,
                running: false,
                interval: MIN_INTERVAL,
            }),
            dir: None,
        }
    }
}

impl std::fmt::Debug for Titles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Titles")
            .field("current", &self.current())
            .finish_non_exhaustive()
    }
}

impl Titles {
    /// Title sets built in `dir` (the index's [`TITLES_DIR`]).
    pub fn at(dir: PathBuf) -> Titles {
        Titles {
            dir: Some(dir),
            ..Titles::default()
        }
    }

    /// The latest build, if one has finished.
    pub fn current(&self) -> Option<Arc<TitleSet>> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Builds from `store` now, on this thread (tests and tools; the
    /// server rebuilds in the background).
    pub fn rebuild_from(&self, store: &Store) -> Result<()> {
        let set = self.build(store)?;
        self.set(set);
        Ok(())
    }

    /// A new set from `store`, in this index's folder, after sweeping
    /// what earlier builds left there (all but the current set's file).
    fn build(&self, store: &Store) -> Result<TitleSet> {
        if let Some(dir) = &self.dir {
            let current = self.current();
            let keep = current.as_ref().and_then(|c| c.file.as_ref());
            sweep(dir, keep.map(|f| f.0.as_path()));
        }
        TitleSet::build_in(store, self.dir.as_deref())
    }

    fn set(&self, set: TitleSet) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(set));
    }

    /// The least time between background rebuilds ([`MIN_INTERVAL`] by
    /// default; tests shorten it).
    pub fn set_interval(&self, interval: Duration) {
        self.lock().interval = interval;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follows the store at `path` (the live indexer's): builds now, and
    /// again after commits.
    pub(crate) fn follow(self: &Arc<Self>, path: PathBuf) {
        {
            let mut s = self.lock();
            s.source = Some(path);
            s.dirty = true;
            s.behind = false;
        }
        self.kick();
    }

    /// Stops following: no more rebuilds (the live indexer is stopping).
    /// The set built last keeps serving whoever still holds the index.
    pub(crate) fn unfollow(&self) {
        self.lock().source = None;
    }

    /// A commit happened: rebuild when the interval allows.
    pub(crate) fn committed(self: &Arc<Self>) {
        self.lock().dirty = true;
        self.kick();
    }

    /// Whether the live indexer has a backlog. While it has, no build
    /// starts (each would read the whole store again, mid-drain); once it
    /// has caught up, the commits it made meanwhile get their build.
    pub(crate) fn set_behind(self: &Arc<Self>, behind: bool) {
        let caught_up = {
            let mut s = self.lock();
            let was = std::mem::replace(&mut s.behind, behind);
            was && !behind
        };
        if caught_up {
            self.kick();
        }
    }

    fn kick(self: &Arc<Self>) {
        {
            let mut s = self.lock();
            if s.running || s.source.is_none() || !s.dirty || s.behind {
                return;
            }
            s.running = true;
        }
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("titles".into())
            .spawn(move || builder(weak));
        if spawned.is_err() {
            self.lock().running = false;
        }
    }
}

/// The builder thread: builds while there are commits it hasn't seen,
/// waiting out the interval after each build, and exits when there are
/// none (or the index is gone).
fn builder(titles: Weak<Titles>) {
    loop {
        let Some(t) = titles.upgrade() else {
            return;
        };
        let (path, interval) = {
            let mut s = t.lock();
            // Behind: the dirty flag stays, for `set_behind` to act on.
            if !s.dirty || s.behind {
                s.running = false;
                return;
            }
            s.dirty = false;
            (s.source.clone(), s.interval)
        };
        let Some(path) = path else {
            t.lock().running = false;
            return;
        };
        let started = Instant::now();
        // No memory map: the build reads the store once, front to back,
        // and pages this connection mapped would count in the process's
        // resident memory again on top of the other connections' maps of
        // the same file (X-import-drain-memory).
        let unmapped = StoreConfig {
            mmap_size: 0,
            ..StoreConfig::default()
        };
        let built = Store::open_reader(&path, unmapped)
            .map_err(Error::from)
            .and_then(|store| t.build(&store));
        match built {
            Ok(set) => {
                lodestar_log::log_event!(
                    crate::log_events::SUGGEST_TITLES_REBUILT,
                    titles = lodestar_log::Count(set.len() as u64)
                );
                t.set(set);
            }
            Err(e) => {
                lodestar_log::log_event!(
                    crate::log_events::SUGGEST_TITLES_FAILED,
                    error = lodestar_log::Error::new(&e)
                );
                // Try again after the interval.
                t.lock().dirty = true;
            }
        }
        drop(t);
        // Rebuilds take at most a tenth of the time.
        std::thread::sleep(interval.max(started.elapsed() * 10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The precomputed rankings by brute force, as the spec reads: every
    /// whole-char prefix of more than [`SCAN_CAP`] titles, with its best
    /// [`TOP`] per visibility (most torrents, then first in key order).
    fn brute_rankings(entries: &[Entry]) -> Rankings {
        let mut under: std::collections::BTreeMap<&[u8], Vec<usize>> = Default::default();
        for (i, (k, folded, _)) in entries.iter().enumerate() {
            let f = std::str::from_utf8(&k[..*folded]).unwrap();
            for (j, c) in f.char_indices() {
                under.entry(&k[..j + c.len_utf8()]).or_default().push(i);
            }
        }
        let (mut big, mut pinned) = (HashMap::new(), Vec::new());
        for (p, ids) in under.into_iter().filter(|(_, ids)| ids.len() > SCAN_CAP) {
            let lists = std::array::from_fn(|combo| {
                let v = |i: usize| visible(entries[i].2, combo);
                let mut best: Vec<usize> = ids.iter().copied().filter(|&i| v(i) > 0).collect();
                best.sort_by(|&a, &b| v(b).cmp(&v(a)).then(a.cmp(&b)));
                best.truncate(TOP);
                best.into_iter()
                    .map(|i| {
                        let (k, folded, packed) = &entries[i];
                        pinned.push((String::from_utf8_lossy(&k[folded + 1..]).into(), *packed));
                        (pinned.len() - 1) as u32
                    })
                    .collect()
            });
            big.insert(String::from_utf8_lossy(p).into(), lists);
        }
        (big, pinned)
    }

    fn set_of(titles: &[(&str, usize)]) -> TitleSet {
        let mut entries: Vec<(Spelling, usize, u64)> = titles
            .iter()
            .map(|(t, class_)| {
                let key = fold(t, false);
                let folded = key.len();
                let mut k = key.into_bytes();
                k.push(0);
                k.extend_from_slice(t.as_bytes());
                let mut counts = [0; 4];
                counts[*class_] = 1;
                (k.into_boxed_slice(), folded, pack(counts))
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut b = MapBuilder::memory();
        for (k, _, v) in &entries {
            b.insert(k, *v).unwrap();
        }
        let (big, pinned) = brute_rankings(&entries);
        TitleSet {
            fst: open_fst(OwnedBytes::new(b.into_inner().unwrap())).unwrap(),
            big,
            pinned,
            torrents: titles.len() as u64,
            took: Duration::ZERO,
            file: None,
        }
    }

    const PLAIN: Shown = Shown {
        adult: false,
        hidden: false,
    };

    /// A store of torrents named `names`, enriched.
    fn store_of(names: &[&str]) -> Store {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        fill(&mut store, names);
        store
    }

    /// Adds torrents named `names` to `store`, enriched.
    fn fill(store: &mut Store, names: &[&str]) {
        use lodestar_bencode::{OwnedValue, to_vec};
        for name in names {
            let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
            let raw = to_vec(&OwnedValue::Dict(
                [
                    ("length", OwnedValue::Int(1 << 30)),
                    ("name", b(name)),
                    ("piece length", OwnedValue::Int(1 << 16)),
                    ("pieces", OwnedValue::Bytes(vec![0; 20])),
                ]
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
            ));
            let info = lodestar_metainfo::Info::parse(&raw).unwrap();
            store
                .insert(&[lodestar_store::NewTorrent {
                    info: &info,
                    raw_info: Some(&raw),
                    discovered_at: 1_700_000_000,
                    metadata_at: 1_700_000_001,
                    extra_flags: 0,
                    source: lodestar_store::SourceId::new(0).unwrap(),
                }])
                .unwrap();
        }
        while lodestar_pipeline::enrich::enrich_stored(store, 0, 100)
            .unwrap()
            .is_some()
        {}
    }

    #[test]
    fn a_build_reads_every_chunk() {
        // Seven torrents, read three at a time here.
        let names = [
            "Alpha.2001.1080p.mkv",
            "Bravo.2002.1080p.mkv",
            "Charlie.2003.1080p.mkv",
            "Delta.2004.1080p.mkv",
            "Echo.2005.1080p.mkv",
            "Foxtrot.2006.1080p.mkv",
            "alpha.2007.720p.mkv",
        ];
        let set = TitleSet::build(&store_of(&names)).unwrap();
        assert_eq!(set.torrents(), 7);
        // `Alpha` and `alpha` are one title, spelt as the first in byte
        // order on a tie.
        assert_eq!(set.len(), 6);
        let alpha = set.lookup("alpha", PLAIN, 5);
        assert_eq!((alpha[0].display.as_str(), alpha[0].count), ("Alpha", 2));
        assert_eq!(set.lookup("foxtrot", PLAIN, 5).len(), 1);
    }

    #[test]
    fn spellings_merge_into_one_title() {
        let sp = |t: &str, c: [u32; 4]| -> (Spelling, [u32; 4]) {
            let mut k = fold(t, false).into_bytes();
            k.push(0);
            k.extend_from_slice(t.as_bytes());
            (k.into_boxed_slice(), c)
        };
        let mut all = vec![
            sp("The Matrix", [1, 0, 0, 0]),
            sp("the matrix", [2, 1, 0, 0]),
            sp("THE MATRIX", [0, 0, 1, 0]),
            sp("Matrix", [1, 0, 0, 0]),
            sp("Ab", [1, 0, 0, 0]),
            sp("ab", [1, 0, 0, 0]),
        ];
        all.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut g = Grouper::default();
        let mut entries: Vec<Entry> = all.into_iter().filter_map(|(k, c)| g.push(k, c)).collect();
        entries.extend(g.finish());
        let got: Vec<(String, u64)> = entries
            .into_iter()
            .map(|(k, folded, packed)| {
                (String::from_utf8(k[folded + 1..].to_vec()).unwrap(), packed)
            })
            .collect();
        assert_eq!(
            got,
            [
                // A tie: the first in byte order.
                ("Ab".to_string(), pack([2, 0, 0, 0])),
                ("Matrix".to_string(), pack([1, 0, 0, 0])),
                // The most used spelling, every class added up.
                ("the matrix".to_string(), pack([3, 1, 1, 0])),
            ]
        );
    }

    #[test]
    fn an_unfollowed_set_never_rebuilds() {
        let t = Arc::new(Titles::default());
        t.follow(PathBuf::from("/nonexistent/lodestar-test.db"));
        t.unfollow();
        // A commit after the live indexer stopped starts nothing.
        t.lock().running = false;
        t.committed();
        assert!(!t.lock().running);
        assert!(t.current().is_none());
    }

    #[test]
    fn folding() {
        assert_eq!(fold("  The Dark-Knight  ", false), "the dark knight");
        assert_eq!(fold("Amélie", false), "amelie");
        assert_eq!(fold("ＡＢＣ", false), "abc");
        assert_eq!(fold("dark ", true), "dark ");
        assert_eq!(fold("dark-", true), "dark ");
        assert_eq!(fold(" -", true), "");
        assert_eq!(
            fold("Spider-Man: No Way Home", false),
            "spider man no way home"
        );
        assert_eq!(fold("千と千尋", false), "千と千尋");
    }

    #[test]
    fn packing_and_visibility() {
        let p = pack([1, 2, 3, 70_000]);
        assert_eq!(visible(p, 0), 1);
        assert_eq!(visible(p, 1), 3);
        assert_eq!(visible(p, 2), 4);
        assert_eq!(visible(p, 3), 1 + 2 + 3 + 0xFFFF);
    }

    #[test]
    fn prefix_lookup_ranks_and_hides() {
        let s = set_of(&[
            ("The Dark Knight", 0),
            ("The Dark Tower", 0),
            ("The Darkest Hour", 1),
            ("Darkman", 2),
        ]);
        let got = |p: &str, shown| -> Vec<String> {
            s.lookup(&fold(p, true), shown, 10)
                .into_iter()
                .map(|t| t.display)
                .collect()
        };
        assert_eq!(got("the dar", PLAIN), ["The Dark Knight", "The Dark Tower"]);
        assert_eq!(
            got(
                "the dar",
                Shown {
                    adult: true,
                    hidden: false
                }
            ),
            ["The Dark Knight", "The Dark Tower", "The Darkest Hour"]
        );
        assert!(got("dark", PLAIN).is_empty());
        assert_eq!(
            got(
                "dark",
                Shown {
                    adult: false,
                    hidden: true
                }
            ),
            ["Darkman"]
        );
        // A trailing space wants a word boundary.
        assert_eq!(
            got("the dark ", PLAIN),
            ["The Dark Knight", "The Dark Tower"]
        );
        assert!(got("the darkest ", PLAIN).is_empty());
        assert!(s.lookup("", PLAIN, 10).is_empty());
    }

    #[test]
    fn wide_prefixes_are_precomputed_and_agree_with_a_scan() {
        let n = SCAN_CAP * 3;
        let titles: Vec<(String, usize)> = (0..n)
            .map(|i| (format!("Alpha {i:05}"), usize::from(i % 7 == 0)))
            .chain((0..50).map(|i| (format!("Beta {i}"), 0)))
            .collect();
        let refs: Vec<(&str, usize)> = titles.iter().map(|(t, c)| (t.as_str(), *c)).collect();
        let mut s = set_of(&refs);
        assert!(s.big.contains_key("a"));
        assert!(s.big.contains_key("alpha 0"));
        assert!(!s.big.contains_key("b"));
        // Every count is 1 here, so the best are the first by key.
        let got = s.lookup("alpha", PLAIN, 5);
        let want: Vec<String> = (1..=6)
            .filter(|i| i % 7 != 0)
            .map(|i| format!("Alpha {i:05}"))
            .take(5)
            .collect();
        assert_eq!(
            got.iter().map(|t| t.display.clone()).collect::<Vec<_>>(),
            want
        );
        // Adult entries are listed only when shown.
        let adult = s.lookup(
            "alpha 0000",
            Shown {
                adult: true,
                hidden: false,
            },
            3,
        );
        assert_eq!(adult[0].display, "Alpha 00000");
        assert_ne!(s.lookup("alpha 0000", PLAIN, 3)[0].display, "Alpha 00000");
        assert!(s.memory_bytes() > s.fst.as_fst().size());
        // A narrow prefix scans.
        assert_eq!(s.lookup("beta 4", PLAIN, 20).len(), 11);
        s.big.clear();
        assert_eq!(s.lookup("beta 4", PLAIN, 20).len(), 11);
    }

    // ---- X-import-drain-memory: the bounded build ----

    /// The spelling key of one title occurrence, as the build forms it.
    fn spelling(title: &str) -> Option<Spelling> {
        let key = fold(title, false);
        if key.is_empty() || key.len() > MAX_TITLE_BYTES {
            return None;
        }
        let display = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut k = key.into_bytes();
        k.push(0);
        k.extend_from_slice(display.as_bytes());
        Some(k.into_boxed_slice())
    }

    /// The set these occurrences give, by brute force: each title's
    /// counts added up, under the spelling most occurrences use (the first
    /// in byte order on a tie).
    fn reference(occurrences: &[(String, usize)]) -> TitleSet {
        use std::collections::BTreeMap;
        let mut by_title: BTreeMap<Vec<u8>, BTreeMap<Spelling, [u32; 4]>> = BTreeMap::new();
        for (t, c) in occurrences {
            if let Some(k) = spelling(t) {
                let folded = k[..folded_len(&k)].to_vec();
                by_title.entry(folded).or_default().entry(k).or_default()[*c] += 1;
            }
        }
        let entries: Vec<Entry> = by_title
            .into_values()
            .map(|spellings| {
                let n = |c: &[u32; 4]| c.iter().sum::<u32>();
                let mut total = [0u32; 4];
                let mut best: Option<(&Spelling, u32)> = None;
                for (k, c) in &spellings {
                    total.iter_mut().zip(c).for_each(|(t, c)| *t += c);
                    if best.is_none_or(|(_, b)| n(c) > b) {
                        best = Some((k, n(c)));
                    }
                }
                let k = best.unwrap().0.clone();
                let folded = folded_len(&k);
                (k, folded, pack(total))
            })
            .collect();
        let mut b = MapBuilder::memory();
        for (k, _, v) in &entries {
            b.insert(k, *v).unwrap();
        }
        let (big, pinned) = brute_rankings(&entries);
        TitleSet {
            fst: open_fst(OwnedBytes::new(b.into_inner().unwrap())).unwrap(),
            big,
            pinned,
            torrents: 0,
            took: Duration::ZERO,
            file: None,
        }
    }

    /// The same through the bounded build: the occurrences in this order,
    /// spilled into a run every `run` of them.
    fn streamed(occurrences: &[(String, usize)], run: usize, dir: Option<&Path>) -> TitleSet {
        let mut work = Work::new(dir).unwrap();
        let mut runs = Vec::new();
        let mut held: HashMap<Spelling, [u32; 4]> = HashMap::new();
        for (i, (t, c)) in occurrences.iter().enumerate() {
            if let Some(k) = spelling(t) {
                let counts = held.entry(k).or_default();
                counts[*c] = counts[*c].saturating_add(1);
            }
            if (i + 1) % run == 0 {
                runs.push(work.spill(&mut held).unwrap());
            }
        }
        runs.push(work.spill(&mut held).unwrap());
        let (fst, file, (big, pinned)) = assemble(&mut work, runs).unwrap();
        TitleSet {
            fst,
            big,
            pinned,
            torrents: 0,
            took: Duration::ZERO,
            file,
        }
    }

    /// The precomputed rankings as titles, so pinned indexes don't matter.
    fn rankings(s: &TitleSet) -> std::collections::BTreeMap<String, Vec<Vec<(String, u64)>>> {
        s.big
            .iter()
            .map(|(p, lists)| {
                let lists = lists
                    .iter()
                    .map(|l| {
                        l.iter()
                            .map(|&i| {
                                let (d, packed) = &s.pinned[i as usize];
                                (d.to_string(), *packed)
                            })
                            .collect()
                    })
                    .collect();
                (p.to_string(), lists)
            })
            .collect()
    }

    fn entries(s: &TitleSet) -> Vec<(Vec<u8>, u64)> {
        let mut out = Vec::new();
        let mut st = s.fst.stream();
        while let Some((k, v)) = st.next() {
            out.push((k.to_vec(), v));
        }
        out
    }

    fn assert_same(got: &TitleSet, want: &TitleSet, probes: &[String]) {
        assert_eq!(entries(got), entries(want));
        assert_eq!(rankings(got), rankings(want));
        for p in probes {
            for combo in 0..4 {
                let shown = Shown {
                    adult: combo & 1 != 0,
                    hidden: combo & 2 != 0,
                };
                assert_eq!(got.lookup(p, shown, 30), want.lookup(p, shown, 30), "{p:?}");
            }
        }
    }

    fn probes(occurrences: &[(String, usize)]) -> Vec<String> {
        let mut p: Vec<String> = occurrences
            .iter()
            .take(40)
            .flat_map(|(t, _)| {
                let f = fold(t, false);
                let cut: Vec<String> = f
                    .char_indices()
                    .map(|(i, c)| f[..i + c.len_utf8()].to_string())
                    .collect();
                cut
            })
            .collect();
        p.extend(["a", "b", "a ", "ab", "ba", "e"].map(String::from));
        p
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(6))]
        /// The bounded build gives the set the in-memory build gave,
        /// precomputed rankings and ties included, however the
        /// occurrences fall into runs. Few letters and their spellings
        /// (case, an accent) make a wide prefix (every title starts with
        /// `a`), spellings of one title split across runs, and mostly
        /// unique titles, so the precomputed lists are cut among ties.
        #[test]
        fn the_bounded_build_matches_the_in_memory_one(
            occurrences in proptest::collection::vec(("[aA][aAbBé ]{6,14}", 0usize..4), 9000..12000),
            run in 1usize..5000,
            on_disk in proptest::bool::ANY,
        ) {
            let want = reference(&occurrences);
            proptest::prop_assert!(!want.big.is_empty(), "the case should have wide prefixes");
            let dir = tempfile::tempdir().unwrap();
            let got = streamed(&occurrences, run, on_disk.then_some(dir.path()));
            assert_same(&got, &want, &probes(&occurrences));
            proptest::prop_assert_eq!(got.file.is_some(), on_disk);
        }
    }

    #[test]
    fn the_bounded_build_matches_on_edge_cases() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<Vec<(String, usize)>> = vec![
            // Nothing at all.
            vec![],
            // One title, one spelling.
            vec![("Alpha".into(), 0)],
            // Two spellings tied, in different runs: the first in byte order.
            vec![("alpha".into(), 0), ("Alpha".into(), 1)],
            // A title that is a prefix of the next, and non-ASCII.
            vec![
                ("ab".into(), 0),
                ("abc".into(), 2),
                ("千と".into(), 0),
                ("千と千尋".into(), 3),
            ],
            // A wide prefix of equal counts, out of order: every list is
            // cut among ties, which go to the first in key order.
            (0..SCAN_CAP + 300)
                .rev()
                .map(|i| (format!("Alpha {:05}", (i * 7919) % (SCAN_CAP + 300)), i % 4))
                .collect(),
        ];
        for occ in &cases {
            let want = reference(occ);
            for run in [1, 2, 100] {
                for d in [None, Some(dir.path())] {
                    assert_same(&streamed(occ, run, d), &want, &probes(occ));
                }
            }
        }
    }

    #[test]
    fn a_build_in_a_folder_maps_its_file_and_removes_its_runs() {
        let names = [
            "Alpha.2001.1080p.mkv",
            "Bravo.2002.1080p.mkv",
            "alpha.2007.720p.mkv",
        ];
        let store = store_of(&names);
        let dir = tempfile::tempdir().unwrap();
        let set = TitleSet::build_in(&store, Some(dir.path())).unwrap();
        let mem = TitleSet::build(&store).unwrap();
        assert_same(&set, &mem, &["alpha".into(), "b".into()]);
        let file = set.file.as_ref().unwrap().0.clone();
        let listed: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            listed,
            std::slice::from_ref(&file),
            "only the set's file stays"
        );
        // The FST is mapped, not held.
        assert_eq!(
            set.memory_bytes(),
            mem.memory_bytes() - mem.fst.as_fst().size()
        );
        drop(set);
        assert!(!file.exists(), "a dropped set removes its file");
    }

    #[test]
    fn a_bad_set_file_is_refused() {
        let store = store_of(&["Alpha.2001.1080p.mkv", "Bravo.2002.1080p.mkv"]);
        let dir = tempfile::tempdir().unwrap();
        let set = TitleSet::build_in(&store, Some(dir.path())).unwrap();
        let good = std::fs::read(&set.file.as_ref().unwrap().0).unwrap();
        let path = dir.path().join("bad.fst");
        assert!(open_set(&path).is_err(), "missing");
        let mut cases: Vec<(&str, Vec<u8>)> = vec![
            ("short", good[..10].to_vec()),
            ("header only", good[..HEADER].to_vec()),
            ("truncated", good[..good.len() - 1].to_vec()),
        ];
        let mut patched = |what, at: usize, bytes: &[u8]| {
            let mut b = good.clone();
            b[at..at + bytes.len()].copy_from_slice(bytes);
            cases.push((what, b));
        };
        patched("magic", 0, b"X");
        patched("version", 8, &2u32.to_le_bytes());
        patched("count", 16, &99u64.to_le_bytes());
        patched("fst", HEADER, &[0xFF; 8]);
        for (what, bytes) in cases {
            std::fs::write(&path, bytes).unwrap();
            assert!(open_set(&path).is_err(), "{what}");
        }
        std::fs::write(&path, &good).unwrap();
        assert_eq!(open_set(&path).unwrap().len(), 2);
    }

    #[test]
    fn a_failed_build_keeps_the_last_set_and_leaves_nothing() {
        let mut store = store_of(&["Alpha.2001.1080p.mkv"]);
        let dir = tempfile::tempdir().unwrap();
        let titles = Titles::at(dir.path().to_path_buf());
        titles.rebuild_from(&store).unwrap();
        let first = titles.current().unwrap();
        fill(&mut store, &["Bravo.2002.1080p.mkv"]);
        // A store the build can't read: it fails after starting its runs.
        store
            .connection()
            .execute_batch("ALTER TABLE torrent_parsed RENAME TO gone")
            .unwrap();
        assert!(titles.rebuild_from(&store).is_err());
        let still = titles.current().unwrap();
        assert!(
            Arc::ptr_eq(&first, &still),
            "the last good set still serves"
        );
        assert_eq!(still.lookup("alpha", PLAIN, 5).len(), 1);
        let listed: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(listed, [first.file.as_ref().unwrap().0.clone()]);
    }

    #[test]
    fn a_build_sweeps_what_crashed_builds_left() {
        let store = store_of(&["Alpha.2001.1080p.mkv"]);
        let dir = tempfile::tempdir().unwrap();
        let titles = Titles::at(dir.path().to_path_buf());
        titles.rebuild_from(&store).unwrap();
        let first = titles.current().unwrap();
        // A crashed build's runs and set, and a file that isn't ours.
        let runs = dir.path().join(format!("{BUILD_PREFIX}dead"));
        std::fs::create_dir(&runs).unwrap();
        std::fs::write(runs.join("run-0"), b"x").unwrap();
        let stale = dir.path().join(format!("{SET_PREFIX}dead{SET_SUFFIX}"));
        std::fs::write(&stale, b"x").unwrap();
        let other = dir.path().join("notes.txt");
        std::fs::write(&other, b"x").unwrap();
        titles.rebuild_from(&store).unwrap();
        let second = titles.current().unwrap();
        assert!(!runs.exists() && !stale.exists() && other.exists());
        // The set still held keeps its file until it's dropped.
        let first_file = first.file.as_ref().unwrap().0.clone();
        assert!(first_file.exists());
        drop(first);
        assert!(!first_file.exists());
        assert!(second.file.as_ref().unwrap().0.exists());
        assert_eq!(second.lookup("alpha", PLAIN, 5).len(), 1);
    }

    /// Waits for `ok`, failing after 10 s with `what`.
    fn wait_for(what: &str, ok: impl Fn() -> bool) {
        let start = Instant::now();
        while !ok() {
            assert!(start.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn builds_wait_while_the_indexer_is_behind_and_keep_the_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("s.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        fill(&mut store, &["Alpha.2001.1080p.mkv"]);
        let t = Arc::new(Titles::at(dir.path().join(TITLES_DIR)));
        t.set_interval(Duration::from_millis(1));
        let has = |p: &str| {
            t.current()
                .is_some_and(|s| !s.lookup(p, PLAIN, 1).is_empty())
        };
        t.follow(db.clone());
        wait_for("the first build", || has("alpha"));
        // A drain: commits arrive while the indexer is behind.
        t.set_behind(true);
        fill(&mut store, &["Bravo.2002.1080p.mkv"]);
        t.committed();
        wait_for("the builder to stop", || !t.lock().running);
        assert!(t.lock().dirty, "the commit isn't lost");
        assert!(!has("bravo"), "no build while behind");
        // Caught up: the build the commits wanted runs.
        t.set_behind(false);
        wait_for("the build after catching up", || has("bravo"));
        // A restart mid-drain: following again builds at once.
        fill(&mut store, &["Charlie.2003.1080p.mkv"]);
        t.set_behind(true);
        t.committed();
        wait_for("the builder to stop", || !t.lock().running);
        t.follow(db);
        wait_for("the build at the next start", || has("charlie"));
        t.unfollow();
    }
}
