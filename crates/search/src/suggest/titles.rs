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
//! [`MIN_INTERVAL`] (or ten build times, if longer). The set belongs to
//! one [`SearchIndex`](crate::SearchIndex), so an index swap starts a
//! new one for the new directory. It lives in memory only: a restart
//! rebuilds it, and until a build finishes there are no title
//! suggestions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use lodestar_classify::{ContentType, Record};
use lodestar_store::{Store, StoreConfig, flags};
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
/// Spellings tracked per title while building.
const VARIANTS: usize = 4;

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
    fst: Map<Vec<u8>>,
    /// Precomputed rankings: folded prefix → per visibility, indexes
    /// into `pinned`.
    big: HashMap<Box<str>, [Box<[u32]>; 4]>,
    pinned: Vec<(Box<str>, u64)>,
    torrents: u64,
    took: Duration,
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

#[derive(Default)]
struct Agg {
    counts: [u32; 4],
    variants: Vec<(String, u32)>,
}

impl TitleSet {
    /// Every title in `store`.
    pub fn build(store: &Store) -> Result<TitleSet> {
        let started = Instant::now();
        let adult = ContentType::Adult.code();
        let mut by_key: HashMap<String, Agg> = HashMap::new();
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
                    let agg = by_key.entry(key.clone()).or_default();
                    agg.counts[c] = agg.counts[c].saturating_add(1);
                    if let Some(v) = agg.variants.iter_mut().find(|(d, _)| *d == display) {
                        v.1 += 1;
                    } else if agg.variants.len() < VARIANTS {
                        agg.variants.push((display, 1));
                    }
                    seen.push(key);
                }
            }
            if read < READ_CHUNK {
                break;
            }
        }
        let mut entries: Vec<(Vec<u8>, usize, u64)> = by_key
            .into_iter()
            .map(|(key, agg)| {
                // The most used spelling; the first seen on a tie.
                let display = agg
                    .variants
                    .iter()
                    .enumerate()
                    .max_by_key(|(i, (_, n))| (*n, std::cmp::Reverse(*i)))
                    .map(|(_, (d, _))| d.clone())
                    .unwrap_or_default();
                let folded = key.len();
                let mut k = key.into_bytes();
                k.push(0);
                k.extend_from_slice(display.as_bytes());
                (k, folded, pack(agg.counts))
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut b = MapBuilder::memory();
        for (k, _, v) in &entries {
            b.insert(k, *v).map_err(fst_error)?;
        }
        let fst = Map::from_bytes(b.into_inner().map_err(fst_error)?).map_err(fst_error)?;
        let (big, pinned) = big_prefixes(&entries);
        Ok(TitleSet {
            fst,
            big,
            pinned,
            torrents,
            took: started.elapsed(),
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

    /// Memory held, in bytes (the FST, the precomputed rankings and the
    /// titles they name), for the budget (§3).
    pub fn memory_bytes(&self) -> usize {
        let fst = self.fst.as_fst().size();
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

fn fst_error(e: tantivy_fst::Error) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

/// The byte just past the first char of `s` at or after `from`, if any.
fn next_char(s: &[u8], from: usize) -> Option<usize> {
    let first = *s.get(from)?;
    let len = match first {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    };
    Some((from + len).min(s.len()))
}

type Big = (HashMap<Box<str>, [Box<[u32]>; 4]>, Vec<(Box<str>, u64)>);

/// The precomputed rankings: every folded prefix (whole chars) shared by
/// more than [`SCAN_CAP`] titles, with its best [`TOP`] per visibility.
/// `entries` are sorted, `(key, folded length, counts)`.
fn big_prefixes(entries: &[(Vec<u8>, usize, u64)]) -> Big {
    let mut big = HashMap::new();
    let mut pinned: Vec<(Box<str>, u64)> = Vec::new();
    let mut pin_of: HashMap<usize, u32> = HashMap::new();
    // Ranges of `entries` sharing a prefix of `len` bytes, over the cap.
    let mut todo: Vec<(usize, usize, usize)> = Vec::new();
    if entries.len() > SCAN_CAP {
        todo.push((0, entries.len(), 0));
    }
    while let Some((lo, hi, len)) = todo.pop() {
        let mut j = lo;
        while j < hi {
            let (key, folded, _) = &entries[j];
            let Some(next) = next_char(&key[..*folded], len) else {
                // Exactly the prefix: no longer prefix to group under.
                j += 1;
                continue;
            };
            let p = &key[..next];
            let mut end = j + 1;
            while end < hi && entries[end].0.starts_with(p) && entries[end].1 >= next {
                end += 1;
            }
            if end - j > SCAN_CAP {
                let prefix: Box<str> = String::from_utf8_lossy(p).into();
                let lists = std::array::from_fn(|combo| {
                    let mut best: Vec<usize> = (j..end)
                        .filter(|&e| visible(entries[e].2, combo) > 0)
                        .collect();
                    let rank = |a: &usize, b: &usize| {
                        visible(entries[*b].2, combo)
                            .cmp(&visible(entries[*a].2, combo))
                            .then_with(|| entries[*a].0.cmp(&entries[*b].0))
                    };
                    if best.len() > TOP {
                        best.select_nth_unstable_by(TOP, rank);
                        best.truncate(TOP);
                    }
                    best.sort_unstable_by(rank);
                    best.into_iter()
                        .map(|e| {
                            *pin_of.entry(e).or_insert_with(|| {
                                let (key, folded, packed) = &entries[e];
                                let display = String::from_utf8_lossy(&key[folded + 1..]);
                                pinned.push((display.into(), *packed));
                                (pinned.len() - 1) as u32
                            })
                        })
                        .collect()
                });
                big.insert(prefix, lists);
                todo.push((j, end, next));
            }
            j = end;
        }
    }
    (big, pinned)
}

/// The title set of one index, rebuilt in the background as it commits.
pub struct Titles {
    current: RwLock<Option<Arc<TitleSet>>>,
    state: Mutex<State>,
}

struct State {
    /// The store to build from (its path), once the live indexer says.
    source: Option<PathBuf>,
    /// Commits since the last build started.
    dirty: bool,
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
                running: false,
                interval: MIN_INTERVAL,
            }),
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
        let set = TitleSet::build(store)?;
        self.set(set);
        Ok(())
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
        }
        self.kick();
    }

    /// A commit happened: rebuild when the interval allows.
    pub(crate) fn committed(self: &Arc<Self>) {
        self.lock().dirty = true;
        self.kick();
    }

    fn kick(self: &Arc<Self>) {
        {
            let mut s = self.lock();
            if s.running || s.source.is_none() || !s.dirty {
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
            if !s.dirty {
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
        let built = Store::open_reader(&path, StoreConfig::default())
            .map_err(Error::from)
            .and_then(|store| TitleSet::build(&store));
        match built {
            Ok(set) => {
                tracing::debug!(titles = set.len(), "suggest: title set rebuilt");
                t.set(set);
            }
            Err(e) => {
                tracing::warn!("suggest: rebuilding titles: {e}");
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

    fn set_of(titles: &[(&str, usize)]) -> TitleSet {
        let mut entries: Vec<(Vec<u8>, usize, u64)> = titles
            .iter()
            .map(|(t, class_)| {
                let key = fold(t, false);
                let folded = key.len();
                let mut k = key.into_bytes();
                k.push(0);
                k.extend_from_slice(t.as_bytes());
                let mut counts = [0; 4];
                counts[*class_] = 1;
                (k, folded, pack(counts))
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut b = MapBuilder::memory();
        for (k, _, v) in &entries {
            b.insert(k, *v).unwrap();
        }
        let (big, pinned) = big_prefixes(&entries);
        TitleSet {
            fst: Map::from_bytes(b.into_inner().unwrap()).unwrap(),
            big,
            pinned,
            torrents: titles.len() as u64,
            took: Duration::ZERO,
        }
    }

    const PLAIN: Shown = Shown {
        adult: false,
        hidden: false,
    };

    /// A store of torrents named `names`, enriched.
    fn store_of(names: &[&str]) -> Store {
        use lodestar_bencode::{OwnedValue, to_vec};
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
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
        while lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
            .unwrap()
            .is_some()
        {}
        store
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
        // `Alpha` and `alpha` are one title, spelt as the first one seen.
        assert_eq!(set.len(), 6);
        let alpha = set.lookup("alpha", PLAIN, 5);
        assert_eq!((alpha[0].display.as_str(), alpha[0].count), ("Alpha", 2));
        assert_eq!(set.lookup("foxtrot", PLAIN, 5).len(), 1);
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
}
