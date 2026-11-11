//! PK-win-soak-ci: merges and segment garbage collection while readers
//! hold searchers, on a real `MmapDirectory` (architecture §11).
//!
//! Where a mapped file can't be deleted until its last map is gone (older
//! Windows), a merge or commit that retires a segment a lease still pins
//! makes tantivy's collection fail to delete its files; it keeps them in
//! its managed list and tries again at the next collection. `meta.json`
//! replacements that collide with a reader go through `retry_dir.rs`. On
//! windows-latest the delete of a mapped file succeeded on this test's
//! first CI run, so the summary line reports how many retired segments
//! outlived a collection rather than requiring any.
//! This drives that path through the public API: one writer commits small
//! batches with merges on (tantivy's default policy) while several reader
//! threads take [`Lease`]s, page them, keep some across commits and
//! merges, page them again, and drop them. It checks that
//!
//! - no commit, merge or collection fails or panics while maps are held;
//! - a held lease keeps returning what it returned when taken;
//! - every committed torrent is found by a search taken after the commit,
//!   and each is found alone by its own word once the run settles;
//! - merges land: the final segment count is a fraction of the commits;
//! - once every lease is gone and one more commit runs, each index folder
//!   holds only its live segments' files (per its own `meta.json`), its
//!   meta files and locks: every delete that failed was retried;
//! - the mmap cache gauge (X-mmap-cache-prune) then counts exactly the
//!   live segment files that are mapped.
//!
//! It runs on every OS; CI's `windows-gc-stress` job runs it on Windows. The work is fixed (commits and
//! batches come from a seeded generator); no assertion is on timing. The
//! only waits are condition waits with a deadline.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{CancelToken, Indexer, Lease, SearchIndex, Sort, Start, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

/// Commits the writer makes. Tantivy's log merge policy merges once a
/// level holds 8 segments, so this is a few dozen merges per index,
/// cascading into larger ones.
const COMMITS: u64 = 240;
/// Torrents per commit: 1 to this many.
const MAX_BATCH: u64 = 4;
const READERS: usize = 4;
/// Leases a reader holds at most at once.
const HELD: usize = 4;
/// Commits a reader keeps a lease across: 1 to this many.
const MAX_HOLD: u64 = 24;
/// How long any wait may take before the test fails with what it saw.
const DEADLINE: Duration = Duration::from_secs(60);
const NOW: i64 = 1_700_000_100;
/// A word in every torrent's name, for the count check.
const CORPUS: &str = "stresscorpus";

/// A small deterministic generator (xorshift64*), so the run is the same
/// every time apart from thread interleaving.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// In `1..=n`.
    fn upto(&mut self, n: u64) -> u64 {
        1 + self.next() % n
    }
}

/// A word naming torrent `id` alone: consonants only, so no stemmer rule
/// changes it and no other torrent's word stems to it.
fn word(id: i64) -> String {
    const LETTERS: &[u8] = b"bcdfghjklmnpqrtvwxz";
    let mut n = id as usize;
    let mut w = String::from("q");
    loop {
        w.push(LETTERS[n % LETTERS.len()] as char);
        n /= LETTERS.len();
        if n == 0 {
            return w;
        }
    }
}

fn d(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &str) -> OwnedValue {
    OwnedValue::Bytes(s.as_bytes().to_vec())
}

/// Torrent `id`'s info dictionary: two files, so the file index gets
/// segments (and merges) too.
fn torrent(id: i64) -> Vec<u8> {
    let w = word(id);
    let file = |name: String| {
        d(vec![
            ("length", OwnedValue::Int(1000 + id)),
            ("path", OwnedValue::List(vec![b(&name)])),
        ])
    };
    to_vec(&d(vec![
        (
            "files",
            OwnedValue::List(vec![file(format!("{w}.mkv")), file(format!("{w}.nfo"))]),
        ),
        ("name", b(&format!("{CORPUS} {w}"))),
        ("piece length", OwnedValue::Int(1 << 16)),
        // Distinct pieces, so every torrent has its own info hash.
        (
            "pieces",
            OwnedValue::Bytes(id.to_be_bytes().repeat(5)[..20].to_vec()),
        ),
    ]))
}

/// What the writer has made searchable, for the readers.
#[derive(Default)]
struct Progress {
    commits: u64,
    /// The highest torrent id committed; ids run from 1 without gaps.
    committed: i64,
    done: bool,
}

#[derive(Default)]
struct Shared {
    progress: Mutex<Progress>,
    changed: Condvar,
}

impl Shared {
    fn publish(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().unwrap());
        self.changed.notify_all();
    }

    /// Waits until a commit after `seen` has been published, or the
    /// writer is done; `(commits, committed, done)`.
    fn wait_past(&self, seen: u64) -> (u64, i64, bool) {
        let until = Instant::now() + DEADLINE;
        let mut p = self.progress.lock().unwrap();
        while p.commits <= seen && !p.done {
            let left = until.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no commit after #{seen} within {DEADLINE:?} (committed {})",
                p.committed
            );
            p = self.changed.wait_timeout(p, left).unwrap().0;
        }
        (p.commits, p.committed, p.done)
    }
}

/// One search: the lease, and what its first page said when taken.
struct Held {
    lease: Lease,
    /// The torrent searched for and the hit ids it returned.
    probe: i64,
    hits: Vec<u64>,
    corpus_total: usize,
    corpus: Lease,
    /// Release after this commit.
    until: u64,
}

fn lease(index: &Arc<SearchIndex>, q: &str) -> Lease {
    let node = parse(q).unwrap();
    Lease::new(
        index.clone(),
        &node,
        NOW,
        Sort::Relevance,
        &CancelToken::new(),
    )
    .unwrap_or_else(|e| panic!("lease for {q:?}: {e}"))
}

fn page_ids(lease: &Lease) -> Vec<u64> {
    let page = lease
        .page(Start::Offset(0), 10, &CancelToken::new())
        .unwrap_or_else(|e| panic!("paging a lease: {e}"));
    page.hits.iter().map(|h| h.id).collect()
}

/// Checks a lease taken now against what the writer had committed
/// before it was taken, and returns it to hold.
fn take(index: &Arc<SearchIndex>, rng: &mut Rng, committed: i64, until: u64) -> Held {
    let probe = rng.upto(committed as u64) as i64;
    let lease_one = lease(index, &word(probe));
    let hits = page_ids(&lease_one);
    assert_eq!(
        hits,
        [probe as u64],
        "torrent {probe} ({}) committed but not found alone",
        word(probe)
    );
    let corpus = lease(index, CORPUS);
    let total = corpus.total();
    assert!(!total.approx, "a corpus this small is counted exactly");
    // Commits may land between the read of `committed` and the lease:
    // never fewer than committed, never more than the writer makes.
    let most = (COMMITS * MAX_BATCH) as usize;
    assert!(
        total.value >= committed as usize && total.value <= most,
        "{CORPUS} counts {} with {committed} committed",
        total.value
    );
    Held {
        lease: lease_one,
        probe,
        hits,
        corpus_total: total.value,
        corpus,
        until,
    }
}

/// A held lease, after commits and merges retired its segments: the same
/// answers as when it was taken.
fn recheck(h: &Held) {
    assert_eq!(
        page_ids(&h.lease),
        h.hits,
        "a held lease for torrent {} changed its answer",
        h.probe
    );
    assert_eq!(h.corpus.total().value, h.corpus_total);
    let ids = page_ids(&h.corpus);
    assert_eq!(ids.len(), h.corpus_total.min(10));
    assert_eq!(
        page_ids(&h.corpus),
        ids,
        "a held lease paged twice disagrees"
    );
}

/// One reader: on each commit it takes a lease, holds up to [`HELD`]
/// across later commits, and re-checks each one before dropping it.
fn reader(index: Arc<SearchIndex>, shared: Arc<Shared>, seed: u64, taken: Arc<AtomicUsize>) {
    let mut rng = Rng(seed);
    let mut held: VecDeque<Held> = VecDeque::new();
    let mut seen = 0;
    loop {
        let (commits, committed, done) = shared.wait_past(seen);
        seen = commits;
        // Release the expired ones, and the oldest past the cap.
        held.retain(|h| {
            let keep = h.until > commits && !done;
            if !keep {
                recheck(h);
            }
            keep
        });
        if done {
            break;
        }
        while held.len() >= HELD {
            recheck(&held.pop_front().unwrap());
        }
        if committed > 0 {
            let until = commits + rng.upto(MAX_HOLD);
            held.push_back(take(&index, &mut rng, committed, until));
            taken.fetch_add(1, Ordering::Relaxed);
        }
        // A search on the handle's current searcher too, as `/search`
        // does; dropped at once.
        let current = index
            .search(&index.query(&parse(CORPUS).unwrap(), NOW), 5)
            .unwrap_or_else(|e| panic!("search: {e}"));
        assert!(current.total >= committed as usize);
    }
}

/// `dir`'s `meta.json`. A merge thread may be replacing it: on Windows the
/// open can then fail for a moment, so it's retried until [`DEADLINE`].
fn read_meta(dir: &Path) -> Vec<u8> {
    let path = dir.join("meta.json");
    let until = Instant::now() + DEADLINE;
    loop {
        match std::fs::read(&path) {
            Ok(bytes) => return bytes,
            // Polls a condition (the replace finishing), not a pause.
            Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => panic!("reading {}: {e}", path.display()),
        }
    }
}

/// Segment ids in a `meta.json`, without hyphens, as file names spell
/// them.
fn live_segments(dir: &Path) -> HashSet<String> {
    let meta: serde_json::Value = serde_json::from_slice(&read_meta(dir)).unwrap();
    meta["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["segment_id"].as_str().unwrap().replace('-', ""))
        .collect()
}

/// Files in `dir` that aren't a live segment's, the meta files or the
/// locks; and how many live segment files aren't empty (tantivy maps
/// those, not empty ones).
fn leaks(dir: &Path) -> (BTreeSet<String>, u64) {
    const KEPT: &[&str] = &[
        "meta.json",
        ".managed.json",
        ".tantivy-meta.lock",
        ".tantivy-writer.lock",
    ];
    let live = live_segments(dir);
    let (mut leaked, mut mapped) = (BTreeSet::new(), 0);
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        let meta = e.metadata().unwrap();
        if meta.is_dir() || KEPT.contains(&name.as_str()) {
            continue;
        }
        let segment = name.split('.').next().unwrap();
        if live.contains(segment) {
            mapped += u64::from(meta.len() > 0);
        } else {
            leaked.insert(name);
        }
    }
    (leaked, mapped)
}

/// Commits once more with nothing pending (the collection runs on every
/// commit) and waits for merges, until both folders hold no leaked files
/// and the gauge matches; fails after [`DEADLINE`] with what it saw.
fn settle(index: &SearchIndex, root: &Path) -> u32 {
    let dirs = [root.to_path_buf(), root.join("files")];
    let until = Instant::now() + DEADLINE;
    let mut rounds = 0;
    loop {
        rounds += 1;
        let mut w = index.writer(15 << 20).unwrap();
        w.commit().unwrap();
        w.finish().unwrap();
        // `finish` reloads after the merges: commit again so the prune
        // and the collection see the reloaded readers.
        let mut w = index.writer(15 << 20).unwrap();
        w.commit().unwrap();
        drop(w);
        let seen: Vec<_> = dirs.iter().map(|d| leaks(d)).collect();
        let gauge = index.mmap_cache_entries().unwrap();
        let clean = seen.iter().all(|(l, _)| l.is_empty())
            && seen.iter().zip(gauge).all(|((_, m), g)| *m == g);
        if clean {
            return rounds;
        }
        assert!(
            Instant::now() < until,
            "after {rounds} settling commits: leaked {:?}, mapped live files {:?}, gauge {gauge:?}",
            seen.iter().map(|(l, _)| l).collect::<Vec<_>>(),
            seen.iter().map(|(_, m)| m).collect::<Vec<_>>(),
        );
    }
}

/// Segments a previous `meta.json` listed that the current one doesn't
/// but whose files are still on disk: retired, not yet deleted.
fn retired_on_disk(dir: &Path, ever: &mut HashSet<String>) -> HashSet<String> {
    let live = live_segments(dir);
    let mut on_disk = HashSet::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        on_disk.insert(name.split('.').next().unwrap().to_owned());
    }
    let retired = ever
        .iter()
        .filter(|s| !live.contains(*s) && on_disk.contains(*s))
        .cloned()
        .collect();
    ever.extend(live);
    retired
}

fn commit_batch(indexer: &mut Indexer, store: &mut Store, first: i64, n: u64) {
    let raws: Vec<Vec<u8>> = (0..n as i64).map(|i| torrent(first + i)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .map(|info| NewTorrent {
            info,
            raw_info: None,
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    let mut added = 0;
    while added < n as usize {
        let step = indexer
            .catch_up(store, 64)
            .unwrap_or_else(|e| panic!("catch-up at torrent {first}: {e}"));
        assert!(step > 0, "the indexer stopped short of torrent {first}");
        added += step;
    }
    indexer
        .commit()
        .unwrap_or_else(|e| panic!("commit at torrent {first}: {e}"));
}

#[test]
fn merges_and_collection_while_readers_hold_maps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("index");
    let index = Arc::new(SearchIndex::open(&root).unwrap());
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let mut indexer = index.writer(15 << 20).unwrap();
    let shared = Arc::new(Shared::default());
    let taken = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..READERS)
        .map(|i| {
            let (index, shared, taken) = (index.clone(), shared.clone(), taken.clone());
            std::thread::spawn(move || reader(index, shared, 0x9E37_79B9 + i as u64, taken))
        })
        .collect();

    let mut rng = Rng(0x5EED);
    let mut next = 1;
    let mut ever = [HashSet::new(), HashSet::new()];
    let mut retired = [HashSet::new(), HashSet::new()];
    // Retired segments still on disk after a later commit's collection:
    // a delete that failed (a merge in flight retires segments too, but
    // the next collection deletes them where a mapped file can be
    // unlinked).
    let mut survived = 0;
    for commit in 1..=COMMITS {
        let n = rng.upto(MAX_BATCH);
        commit_batch(&mut indexer, &mut store, next, n);
        next += n as i64;
        shared.publish(|p| {
            p.commits = commit;
            p.committed = next - 1;
        });
        let dirs = [root.clone(), root.join("files")];
        for ((dir, ever), before) in dirs.iter().zip(&mut ever).zip(&mut retired) {
            let now = retired_on_disk(dir, ever);
            survived += now.intersection(before).count();
            *before = now;
        }
    }
    shared.publish(|p| p.done = true);
    for r in readers {
        if let Err(panic) = r.join() {
            std::panic::resume_unwind(panic);
        }
    }
    let committed = (next - 1) as u64;
    assert_eq!(index.num_docs(), committed);
    // Each torrent has two files.
    assert_eq!(index.num_files(), committed * 2);
    let segments_ever = ever.iter().map(HashSet::len).sum::<usize>();
    // Not asserted: whether a delete of a mapped file fails is the OS's
    // call. On windows-latest it succeeded on this PR's first run (0
    // here), so the retry path it would take is reported, not required.

    // The production shutdown: waits for the merges in flight, so none
    // finishes after `settle` has looked, and fails if one failed.
    indexer.finish().unwrap();
    let rounds = settle(&index, &root);
    // Tantivy only logs a failed merge. Every commit made a segment in
    // each index, so without merges each would hold one per commit.
    let segments = index.segment_counts();
    assert!(
        segments.iter().all(|&n| n < COMMITS / 10),
        "{segments:?} live segments after {COMMITS} commits: merges didn't land"
    );
    // Everything committed is still there after the merges, each torrent
    // found alone by its own word.
    let all = lease(&index, CORPUS);
    assert_eq!(all.total().value as u64, committed);
    for id in 1..=committed as i64 {
        assert_eq!(
            page_ids(&lease(&index, &word(id))),
            [id as u64],
            "torrent {id} ({}) not found alone after settling",
            word(id)
        );
    }
    eprintln!(
        "{COMMITS} commits, {committed} torrents, {} leases, {segments_ever} segments ever, \
         {segments:?} at the end, {survived} retired segments outlived a collection, settled in {rounds} round(s)",
        taken.load(Ordering::Relaxed)
    );
}
