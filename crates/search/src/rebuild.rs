//! Rebuilding the index for a new schema or tokenizer (architecture §8,
//! "Index evolution"): the newest complete older index keeps serving
//! while this build's index is built beside it, then readers and the
//! live indexer swap to the new one and the old directory is deleted.
//!
//! The index root holds one directory per version:
//!
//! - `v{schema}-t{tokenizer}`: complete. Only a finished build is ever
//!   given this name, by an atomic rename, so the name is the proof.
//! - `v{schema}-t{tokenizer}.building`: in progress. It's an ordinary
//!   index whose commit payloads carry its watermarks (§16), so an
//!   interrupted build resumes where its last commit left it. A
//!   `COMPLETE` marker is written after its final commit and before the
//!   rename, so a crash between the two only needs the rename.
//! - `*.trash`: an old directory renamed away before it's deleted, so a
//!   half-deleted index never keeps a complete-looking name.
//!
//! An exclusive lock on `.lock` in the root keeps a second instance out.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestar_store::{Store, StoreConfig};
use tantivy::directory::{Directory, DirectoryLock, Lock, MmapDirectory};

use crate::index::{Compat, Error, Indexer, Result, SearchIndex};
use crate::lease::{LeaseConfig, Leases};
use crate::live::{CommitHook, Committed, IndexStats, LiveConfig, LiveIndexer};
use crate::schema::{SCHEMA_VERSION, TOKENIZER_VERSION};

const BUILDING: &str = ".building";
const TRASH: &str = ".trash";
/// Written into a `.building` directory after its final commit.
const MARKER: &str = "COMPLETE";
const LOCK_FILE: &str = ".lock";

/// An index's schema and tokenizer versions, which name its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IndexVersion {
    pub schema: u32,
    pub tokenizer: u32,
}

impl IndexVersion {
    /// This build's.
    pub const CURRENT: IndexVersion = IndexVersion {
        schema: SCHEMA_VERSION,
        tokenizer: TOKENIZER_VERSION,
    };

    /// The complete index's directory name, `v{schema}-t{tokenizer}`.
    pub fn dir_name(&self) -> String {
        format!("v{}-t{}", self.schema, self.tokenizer)
    }

    fn parse(name: &str) -> Option<IndexVersion> {
        let (s, t) = name.strip_prefix('v')?.split_once("-t")?;
        let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
        if !digits(s) || !digits(t) {
            return None;
        }
        Some(IndexVersion {
            schema: s.parse().ok()?,
            tokenizer: t.parse().ok()?,
        })
    }
}

impl Default for IndexVersion {
    fn default() -> Self {
        IndexVersion::CURRENT
    }
}

impl std::fmt::Display for IndexVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.dir_name())
    }
}

/// What an entry in the index root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Complete(IndexVersion),
    Building(IndexVersion),
    Trash,
}

fn classify(name: &str) -> Option<Entry> {
    if name.ends_with(TRASH) {
        return Some(Entry::Trash);
    }
    match name.strip_suffix(BUILDING) {
        Some(v) => IndexVersion::parse(v).map(Entry::Building),
        None => IndexVersion::parse(name).map(Entry::Complete),
    }
}

/// The index root's directories, by kind.
fn scan(root: &Path) -> Result<Vec<(PathBuf, Entry)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root).map_err(Error::Io)? {
        let e = e.map_err(Error::Io)?;
        if !e.file_type().map_err(Error::Io)?.is_dir() {
            continue;
        }
        if let Some(kind) = e.file_name().to_str().and_then(classify) {
            out.push((e.path(), kind));
        }
    }
    Ok(out)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Makes a rename or a new file in `dir` durable. Windows can't open a
/// directory for this; NTFS journals the rename itself.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Tries a rename this many times before failing (about 10 s in all).
const RENAME_ATTEMPTS: u32 = 10;

/// Renames a directory, retrying with backoff. Windows refuses to rename
/// a directory while any file in it is open, and an antivirus or search
/// indexer opens freshly written segment files for a moment, so a
/// failure there is usually transient. Retrying keeps such a moment from
/// failing a whole rebuild (§11).
fn rename(from: &Path, to: &Path) -> Result<()> {
    rename_with(from, to, std::thread::sleep)
}

/// [`rename`], waiting between attempts with `sleep`, which tests pass
/// to act between attempts.
fn rename_with(from: &Path, to: &Path, mut sleep: impl FnMut(Duration)) -> Result<()> {
    let mut wait = Duration::from_millis(25);
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => break,
            Err(e) if attempt < RENAME_ATTEMPTS && e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    "index: renaming {} (attempt {attempt}): {e}",
                    from.display()
                );
                sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(4));
                attempt += 1;
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    if let Some(parent) = to.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Where a rebuild is, for the "Index rebuilding" banner (§10.4) and
/// Insights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildProgress {
    pub phase: RebuildPhase,
    /// Torrents in the new index so far (a resumed build starts with what
    /// it had).
    pub done: u64,
    /// Torrents in the store when the build started: the ones it builds
    /// before finishing. Later ones are added at the swap.
    pub total: u64,
    /// `reindex_log` entries replayed into the new index, apart from
    /// `done`.
    pub replayed: u64,
    /// Failed steps (each is retried), apart from the live indexer's.
    pub errors: u64,
    /// Unix seconds.
    pub started_at: u64,
    /// The index serving meanwhile, and the one being built.
    pub from: IndexVersion,
    pub to: IndexVersion,
    /// Fields of this build the serving index lacks: clauses on them
    /// match nothing until the swap ([`SearchIndex::missing_fields`]).
    pub unavailable: Vec<String>,
    /// Why the rebuild stopped, once [`RebuildPhase::Failed`].
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildPhase {
    /// Following the store into the new directory.
    Building,
    /// Caught up: waiting for merges, marking it complete, renaming it.
    Finishing,
    /// Moving readers and the live indexer to the new index.
    Swapping,
    /// It stopped on an error (logged); the old index keeps serving, and
    /// the next start resumes the build.
    Failed,
}

/// What one [`Rebuild::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    /// Torrents added.
    pub added: usize,
    /// `reindex_log` entries replayed.
    pub replayed: usize,
}

impl Step {
    /// Nothing to do: caught up with the store.
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.replayed == 0
    }
}

/// A build of one version's index in its `.building` directory. The
/// serving handle drives it; it's public for tools and tests. The caller
/// must hold the root's lock (or know nothing else uses the root).
pub struct Rebuild {
    index: SearchIndex,
    indexer: Indexer,
    dir: PathBuf,
    final_dir: PathBuf,
}

impl std::fmt::Debug for Rebuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rebuild")
            .field("dir", &self.dir)
            .field("indexer", &self.indexer)
            .finish()
    }
}

impl Rebuild {
    /// Opens `version`'s `.building` directory under `root`, creating it
    /// or resuming from its watermarks.
    pub fn open(root: &Path, version: IndexVersion, heap_bytes: usize) -> Result<Rebuild> {
        let dir = root.join(format!("{}{BUILDING}", version.dir_name()));
        let index = SearchIndex::open(&dir)?;
        let indexer = index.writer(heap_bytes)?;
        Ok(Rebuild {
            index,
            indexer,
            dir,
            final_dir: root.join(version.dir_name()),
        })
    }

    /// Adds up to `batch` torrents, then replays up to `batch`
    /// `reindex_log` entries, as the live indexer does.
    pub fn step(&mut self, store: &mut Store, batch: usize) -> Result<Step> {
        let (added, _, replayed) = self.indexer.catch_up_counted(store, batch)?;
        Ok(Step { added, replayed })
    }

    /// Whether it has followed the store up to torrent `id` and
    /// `reindex_log` entry `seq` (a snapshot taken when it started).
    pub fn reached(&self, id: i64, seq: i64) -> bool {
        self.indexer.reached(id, seq)
    }

    /// Torrents added or replaced since the last commit.
    pub fn pending(&self) -> usize {
        self.indexer.pending()
    }

    /// Commits with the watermarks: work up to here survives a crash.
    pub fn commit(&mut self) -> Result<()> {
        self.indexer.commit()
    }

    /// Documents committed.
    pub fn indexed(&self) -> u64 {
        self.index.num_docs()
    }

    /// Commits, waits for merges, writes the marker and renames the
    /// directory to its complete name, which it returns.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.indexer.commit()?;
        let Rebuild {
            index,
            indexer,
            dir,
            final_dir,
        } = self;
        indexer.finish()?;
        // Every handle on the directory is closed before the rename,
        // which Windows requires.
        drop(index);
        mark_complete(&dir)?;
        rename(&dir, &final_dir)?;
        Ok(final_dir)
    }
}

fn mark_complete(dir: &Path) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(dir.join(MARKER)).map_err(Error::Io)?;
    writeln!(f, "complete at {}", unix_now()).map_err(Error::Io)?;
    f.sync_all().map_err(Error::Io)?;
    sync_dir(dir);
    Ok(())
}

/// The highest torrent id in the store (0 when empty), found with a few
/// primary-key lookups: `ids_after(x, 1)` is empty exactly when `x` is at
/// least the highest id.
fn max_id(store: &Store) -> Result<i64> {
    let above = |x: i64| -> Result<bool> { Ok(!store.ids_after(x, 1)?.is_empty()) };
    if !above(0)? {
        return Ok(0);
    }
    // `lo` has an id above it, `hi` doesn't.
    let (mut lo, mut hi) = (0i64, 1i64);
    while above(hi)? {
        lo = hi;
        hi = hi.saturating_mul(2);
        if hi == i64::MAX {
            break;
        }
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if above(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(hi)
}

/// Settings for [`Serving::open_or_rebuild`].
#[derive(Debug, Clone)]
pub struct ServingConfig {
    /// The live indexer's. A rebuild reads `batch` torrents per step and
    /// uses a writer of `heap_bytes` of its own, beside the live one.
    pub live: LiveConfig,
    /// For the indexers' read-only store connections.
    pub store: StoreConfig,
    /// A rebuild commits every this many torrents: fewer, larger commits
    /// than live indexing, since nobody waits to search them.
    pub rebuild_commit_docs: usize,
    /// The version to serve: [`IndexVersion::CURRENT`]. Only tests set
    /// another, to rebuild without bumping the real constants.
    pub version: IndexVersion,
    /// The searcher leases cursors read through ([`Serving::leases`]).
    pub leases: LeaseConfig,
    /// Tests only: while set, a rebuild that has caught up waits before
    /// finishing, so a test can act at that point.
    #[doc(hidden)]
    pub hold_before_finish: Option<Arc<AtomicBool>>,
    /// Tests only: while set, a rebuild waits before its first step, so
    /// a test can read the progress it resumed from.
    #[doc(hidden)]
    pub hold_before_start: Option<Arc<AtomicBool>>,
    /// Tests only: the new index's live indexer fails to start at the
    /// swap.
    #[cfg(test)]
    pub(crate) fail_live_at_swap: bool,
}

impl Default for ServingConfig {
    fn default() -> Self {
        ServingConfig {
            live: LiveConfig::default(),
            store: StoreConfig::default(),
            rebuild_commit_docs: 50_000,
            version: IndexVersion::CURRENT,
            leases: LeaseConfig::default(),
            hold_before_finish: None,
            hold_before_start: None,
            #[cfg(test)]
            fail_live_at_swap: false,
        }
    }
}

/// State shared with the background thread.
struct Shared {
    /// The serving index. A mutex held only to clone or replace the
    /// `Arc`, so a swap waits behind at most one clone: no stream of
    /// readers can hold it off, whatever the platform's lock policy.
    current: Mutex<Arc<SearchIndex>>,
    progress: Mutex<Option<RebuildProgress>>,
    live: Mutex<Option<LiveIndexer>>,
    stats: Arc<IndexStats>,
    /// Every live indexer reports its commits here, across swaps (B4).
    hook: Arc<CommitHook>,
    leases: Arc<Leases>,
    stop: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut RebuildProgress)) {
        if let Some(p) = lock(&self.progress).as_mut() {
            f(p);
        }
    }

    fn set_phase(&self, phase: RebuildPhase) {
        self.update(|p| p.phase = phase);
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sleeps up to `d`, waking early when stopping. Returns whether to
    /// go on.
    fn pause(&self, d: Duration) -> bool {
        let step = Duration::from_millis(20);
        let mut left = d;
        while !left.is_zero() && !self.stopping() {
            let s = left.min(step);
            std::thread::sleep(s);
            left -= s;
        }
        !self.stopping()
    }
}

/// The index that serves searches, with the live indexer that follows
/// the store into it, and the background rebuild when this build's index
/// doesn't exist yet. Readers take [`Serving::index`] once per query, so
/// they move to the new index between queries; a searcher lease keeps
/// the index it was taken from, whose directory is deleted only once the
/// last lease on it is gone.
pub struct Serving {
    shared: Arc<Shared>,
    background: Mutex<Option<JoinHandle<()>>>,
    sweeper: Mutex<Option<JoinHandle<()>>>,
    root: PathBuf,
    /// Released last, after the threads have stopped.
    _lock: DirectoryLock,
}

impl std::fmt::Debug for Serving {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Serving")
            .field("root", &self.root)
            .field("progress", &self.progress())
            .finish_non_exhaustive()
    }
}

/// Takes the exclusive lock on the index root.
fn lock_root(root: &Path) -> Result<DirectoryLock> {
    use tantivy::directory::error::LockError;
    MmapDirectory::open(root)?
        .acquire_lock(&Lock {
            filepath: PathBuf::from(LOCK_FILE),
            is_blocking: false,
        })
        .map_err(|e| match e {
            LockError::LockBusy => Error::Locked(root.to_path_buf()),
            LockError::IoError(io) => Error::LockFailed {
                root: root.to_path_buf(),
                error: std::io::Error::new(io.kind(), io.to_string()),
            },
        })
}

impl Serving {
    /// Opens the index under `root` (e.g. `data/index`) and starts the
    /// live indexer, reading the store at `store_path`.
    ///
    /// When `config.version`'s index isn't complete, the newest complete
    /// older index serves while it's built in the background, and the
    /// handle swaps to it once it has caught up with the store. With
    /// nothing older to serve (a new install, or only indexes too
    /// different to query), this version's index is opened directly and
    /// fills as the live indexer catches up, as it always did.
    ///
    /// Fails with [`Error::Locked`] while another handle serves `root`,
    /// and with the error itself when an older index can't be opened
    /// (rather than discarding it).
    pub fn open_or_rebuild(
        root: &Path,
        store_path: &Path,
        config: ServingConfig,
    ) -> Result<Serving> {
        std::fs::create_dir_all(root).map_err(Error::Io)?;
        let dir_lock = lock_root(root)?;
        let want = config.version;
        let final_dir = root.join(want.dir_name());
        let building = root.join(format!("{}{BUILDING}", want.dir_name()));
        // A crash between the marker and the rename: only the rename is
        // left to do.
        if !final_dir.exists() && building.join(MARKER).exists() {
            rename(&building, &final_dir)?;
        }
        let open_reader = || Store::open_reader(store_path, config.store.clone());

        let mut from = None;
        let (serving, writable) = if final_dir.exists() {
            (SearchIndex::open(&final_dir)?, true)
        } else if let Some((v, index, compat)) = newest_servable(root, want)? {
            from = Some(v);
            (index, compat == Compat::Exact)
        } else {
            // Nothing older to serve, so a partial build is the best
            // index there is: it becomes this version's and fills live.
            if building.exists() {
                rename(&building, &final_dir)?;
            }
            (SearchIndex::open(&final_dir)?, true)
        };

        // Opened before any thread starts, so a broken store or directory
        // fails the start cleanly.
        let build = match from {
            Some(from) => {
                let store = open_reader()?;
                let builder = Rebuild::open(root, want, config.live.heap_bytes)?;
                // What the build covers before it finishes; later
                // torrents and log entries are added at the swap.
                let snapshot = (max_id(&store)?, store.reindex_head()?);
                let progress = RebuildProgress {
                    phase: RebuildPhase::Building,
                    done: builder.indexed(),
                    total: store.count()?,
                    replayed: 0,
                    errors: 0,
                    started_at: unix_now(),
                    from,
                    to: want,
                    unavailable: serving.missing_fields().to_vec(),
                    error: None,
                };
                Some((store, builder, snapshot, progress))
            }
            None => None,
        };
        let stats = Arc::new(IndexStats::default());
        let hook = Arc::new(CommitHook::default());
        let live = if writable {
            Some(LiveIndexer::spawn_with_stats(
                serving.clone(),
                open_reader()?,
                config.live.clone(),
                stats.clone(),
                hook.clone(),
            )?)
        } else {
            None
        };
        let shared = Arc::new(Shared {
            current: Mutex::new(Arc::new(serving)),
            progress: Mutex::new(None),
            live: Mutex::new(live),
            stats,
            hook,
            leases: Arc::new(Leases::new(config.leases.clone())),
            stop: AtomicBool::new(false),
        });

        let every = sweep_every(&config.leases);
        let thread = std::thread::Builder::new().name("index-rebuild".into());
        let (sh, root_buf) = (shared.clone(), root.to_path_buf());
        let store_path = store_path.to_path_buf();
        let background = match build {
            Some((store, builder, snapshot, progress)) => {
                tracing::info!("index: rebuilding {want} while {} serves", progress.from);
                *lock(&shared.progress) = Some(progress);
                thread.spawn(move || {
                    let done =
                        rebuild_and_swap(&sh, builder, store, snapshot, &store_path, &config);
                    match done {
                        Ok(Some(retired)) => {
                            if wait_released(&sh, &retired) {
                                clean(&sh, &root_buf, want);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::error!("index: rebuilding {want} failed: {e}");
                            sh.update(|p| {
                                p.phase = RebuildPhase::Failed;
                                p.error = Some(e.to_string());
                            });
                        }
                    }
                })
            }
            None => thread.spawn(move || clean(&sh, &root_buf, want)),
        }
        .map_err(Error::Io)?;
        let sweeper = spawn_sweeper(&shared, every).inspect_err(|_| {
            // No `Serving` to stop the rebuild thread: it stops itself.
            shared.stop.store(true, Ordering::Relaxed);
        })?;
        Ok(Serving {
            shared,
            background: Mutex::new(Some(background)),
            sweeper: Mutex::new(Some(sweeper)),
            root: root.to_path_buf(),
            _lock: dir_lock,
        })
    }

    /// The index to run the next query on.
    pub fn index(&self) -> Arc<SearchIndex> {
        lock(&self.shared.current).clone()
    }

    /// The rebuild under way, or `None` when this build's index serves.
    pub fn progress(&self) -> Option<RebuildProgress> {
        lock(&self.shared.progress).clone()
    }

    /// Whether an older index is serving while the new one is built.
    pub fn rebuilding(&self) -> bool {
        self.progress().is_some()
    }

    /// The live indexer's counters, kept across the swap.
    pub fn stats(&self) -> Arc<IndexStats> {
        self.shared.stats.clone()
    }

    /// Where the live indexer reports its commits; it keeps reporting
    /// there after a swap. Set its sink once the listener exists: commits
    /// before that report nothing.
    pub fn commit_hook(&self) -> &Arc<CommitHook> {
        &self.shared.hook
    }

    /// The searcher leases taken on the indexes this handle serves.
    pub fn leases(&self) -> &Arc<Leases> {
        &self.shared.leases
    }

    /// Stops the rebuild (which commits what it has, to resume next
    /// time) and the live indexer (which commits what's pending).
    pub fn shutdown(self) -> Result<()> {
        self.halt()
    }

    /// [`Serving::shutdown`] through a shared handle; later calls do
    /// nothing.
    pub fn stop(&self) -> Result<()> {
        self.halt()
    }

    fn halt(&self) -> Result<()> {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = lock(&self.sweeper).take() {
            t.thread().unpark();
            let _ = t.join();
        }
        if let Some(t) = lock(&self.background).take() {
            // The thread logs its own errors.
            let _ = t.join();
        }
        match lock(&self.shared.live).take() {
            Some(live) => live.stop(),
            None => Ok(()),
        }
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        if let Err(e) = self.halt() {
            tracing::warn!("index: stopping: {e}");
        }
    }
}

/// Expired leases are dropped at least this often, so they don't pin
/// their searchers and merged-away segment files until the next lease
/// is taken.
const SWEEP_AT_MOST_EVERY: Duration = Duration::from_secs(30);

/// How often the sweeper runs: a quarter of the lease ttl, at most
/// [`SWEEP_AT_MOST_EVERY`], so an idle lease goes within 1.25 ttl.
fn sweep_every(config: &LeaseConfig) -> Duration {
    (config.ttl / 4).clamp(Duration::from_millis(1), SWEEP_AT_MOST_EVERY)
}

/// Sweeps the leases every `every` until stopping. It parks between
/// sweeps; [`Serving::stop`] unparks it.
fn spawn_sweeper(shared: &Arc<Shared>, every: Duration) -> Result<JoinHandle<()>> {
    let sh = shared.clone();
    std::thread::Builder::new()
        .name("lease-sweep".into())
        .spawn(move || {
            while !sh.stopping() {
                std::thread::park_timeout(every);
                if !sh.stopping() {
                    sh.leases.sweep();
                }
            }
        })
        .map_err(Error::Io)
}

/// Tries to open an older index this many times before failing the
/// start: an antivirus scan or a share hiccup is no reason to discard it.
const OPEN_ATTEMPTS: u32 = 5;

/// The newest complete index older than `want` that can serve this
/// build's queries. One without the fields every search needs is a
/// schema mismatch and is passed over; one that won't open is retried,
/// then fails the start, so a healthy index is never discarded over a
/// transient error.
fn newest_servable(
    root: &Path,
    want: IndexVersion,
) -> Result<Option<(IndexVersion, SearchIndex, Compat)>> {
    let mut older: Vec<(IndexVersion, PathBuf)> = scan(root)?
        .into_iter()
        .filter_map(|(path, e)| match e {
            Entry::Complete(v) if v < want => Some((v, path)),
            _ => None,
        })
        .collect();
    older.sort_by_key(|o| std::cmp::Reverse(o.0));
    for (v, path) in older {
        // No index there at all (a crash while creating it): nothing to
        // keep.
        if !path.join("meta.json").exists() || !path.join("files").join("meta.json").exists() {
            tracing::warn!("index: {v} holds no index; not serving it");
            continue;
        }
        let mut wait = Duration::from_millis(100);
        let mut attempt = 1;
        let opened = loop {
            match SearchIndex::open_existing(&path) {
                Err(e) if attempt < OPEN_ATTEMPTS => {
                    tracing::warn!("index: opening {v} (attempt {attempt}): {e}");
                    std::thread::sleep(wait);
                    wait *= 2;
                    attempt += 1;
                }
                other => break other?,
            }
        };
        match opened {
            Some((index, compat)) => return Ok(Some((v, index, compat))),
            None => tracing::warn!("index: {v} lacks fields every search needs; not serving it"),
        }
    }
    Ok(None)
}

/// Builds to the snapshot, finishes, and swaps the new index in. On a
/// swap, it returns the retired index's [`SearchIndex::holders`]; `None`
/// when stopped first, and what's committed is resumed next time.
fn rebuild_and_swap(
    shared: &Shared,
    mut builder: Rebuild,
    mut store: Store,
    (max_id, reindex_head): (i64, i64),
    store_path: &Path,
    config: &ServingConfig,
) -> Result<Option<Weak<()>>> {
    if let Some(hold) = &config.hold_before_start {
        while hold.load(Ordering::Relaxed) && shared.pause(Duration::from_millis(5)) {}
    }
    let mut wait = config.live.poll;
    while !builder.reached(max_id, reindex_head) {
        if shared.stopping() {
            builder.commit()?;
            return Ok(None);
        }
        match builder.step(&mut store, config.live.batch) {
            // Torrents at the end deleted since the snapshot.
            Ok(step) if step.is_empty() => break,
            Ok(step) => {
                wait = config.live.poll;
                shared.update(|p| {
                    p.done += step.added as u64;
                    p.replayed += step.replayed as u64;
                    // The last step can run past the snapshot.
                    p.total = p.total.max(p.done);
                });
                if builder.pending() >= config.rebuild_commit_docs {
                    builder.commit()?;
                }
            }
            Err(e) => {
                // A busy database; anything else repeats and is logged
                // each time.
                shared.update(|p| p.errors += 1);
                tracing::warn!("index rebuild: {e}");
                shared.pause(wait);
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
    }
    if let Some(hold) = &config.hold_before_finish {
        while hold.load(Ordering::Relaxed) && shared.pause(Duration::from_millis(5)) {}
    }
    if shared.stopping() {
        builder.commit()?;
        return Ok(None);
    }
    shared.set_phase(RebuildPhase::Finishing);
    let final_dir = builder.finish()?;
    if shared.stopping() {
        // Complete on disk: the next start serves it.
        return Ok(None);
    }
    shared.set_phase(RebuildPhase::Swapping);
    let new = SearchIndex::open(&final_dir)?;
    // The old live indexer stops (committing) before the new index's
    // last catch-up, so everything readers could see in the old index is
    // in the new one when they move.
    let old_live = lock(&shared.live).take();
    let old_followed = old_live.is_some();
    if let Some(old) = old_live
        && let Err(e) = old.stop()
    {
        tracing::warn!("index: stopping the old indexer: {e}");
    }
    // What readers can find now; torrents past it are new to them at the
    // swap, and no live indexer reported them.
    let visible = lock(&shared.current).watermark();
    let mut fresh = None;
    let started = (|| {
        let mut w = new.writer(config.live.heap_bytes)?;
        while w.catch_up(&mut store, config.live.batch)? > 0 {}
        w.commit()?;
        let last = w.last_id();
        w.finish()?;
        // A count that fails only leaves those torrents unannounced.
        if let Ok(visible) = visible {
            fresh = stored_between(&store, visible, last)
                .and_then(|n| Ok((n, new.adult_between(visible, last)?)))
                .ok()
                .map(|n| (n, last));
        }
        #[cfg(test)]
        if config.fail_live_at_swap {
            return Err(Error::Corrupt("test: the new live indexer didn't start"));
        }
        LiveIndexer::spawn_with_stats(
            new.clone(),
            store,
            config.live.clone(),
            shared.stats.clone(),
            shared.hook.clone(),
        )
    })();
    let live = match started {
        Ok(live) => live,
        Err(e) => {
            // The old index keeps serving, so it must keep following the
            // store. Its indexer was stopped above, and the store
            // connection went with the failed start: open another.
            if old_followed {
                restart_old(shared, store_path, config);
            }
            return Err(e);
        }
    };
    *lock(&shared.live) = Some(live);
    let old = std::mem::replace(&mut *lock(&shared.current), Arc::new(new));
    *lock(&shared.progress) = None;
    if let Some(((docs_added, adult_added), max_torrent_id)) = fresh.filter(|f| f.0.0 > 0) {
        shared.hook.publish(Committed {
            docs_added,
            adult_added,
            max_torrent_id,
            at: unix_now(),
        });
    }
    tracing::info!("index: swapped to the rebuilt index");
    Ok(Some(old.holders()))
}

/// Torrents stored with ids in `(after, upto]`.
pub(crate) fn stored_between(store: &Store, mut after: i64, upto: i64) -> Result<u64> {
    let mut n = 0;
    while after < upto {
        let ids = store.ids_after(after, 10_000)?;
        let Some(&last) = ids.last() else { break };
        n += ids.iter().filter(|&&id| id <= upto).count() as u64;
        after = last;
    }
    Ok(n)
}

/// Starts a live indexer on the index serving now, through a new store
/// connection: after a failed swap, so the old index doesn't stop taking
/// new torrents until a restart.
fn restart_old(shared: &Shared, store_path: &Path, config: &ServingConfig) {
    let old = lock(&shared.current).as_ref().clone();
    let started = Store::open_reader(store_path, config.store.clone())
        .map_err(Error::from)
        .and_then(|store| {
            LiveIndexer::spawn_with_stats(
                old,
                store,
                config.live.clone(),
                shared.stats.clone(),
                shared.hook.clone(),
            )
        });
    match started {
        Ok(live) => *lock(&shared.live) = Some(live),
        Err(e) => tracing::error!("index: restarting the old indexer: {e}"),
    }
}

/// Queries in flight when a lease's limit runs out.
const RELEASE_SLACK: Duration = Duration::from_secs(60);

/// Waits until nothing holds the retired index (no lease, no query in
/// flight), so its files aren't deleted from under a reader (Windows
/// can't delete a mapped file at all). Expired leases are dropped while
/// waiting. A lease lives at most [`LeaseConfig::max_age`]; past that
/// (plus [`RELEASE_SLACK`]) whatever still holds it isn't a lease, and the
/// deletion goes ahead, retried as usual. `false` when stopping: the
/// next start deletes it.
fn wait_released(shared: &Shared, retired: &Weak<()>) -> bool {
    let limit = shared.leases.config().max_age + RELEASE_SLACK;
    let start = Instant::now();
    let mut wait = Duration::from_millis(10);
    loop {
        shared.leases.sweep();
        if retired.strong_count() == 0 {
            return true;
        }
        if start.elapsed() >= limit {
            tracing::warn!("index: the retired index is still open; deleting it anyway");
            return true;
        }
        if !shared.pause(wait) {
            return false;
        }
        wait = (wait * 2).min(Duration::from_secs(1));
    }
}

/// Deletes what `want` makes obsolete: older complete indexes, other
/// versions' partial builds, this version's partial build once it's
/// complete, and trash. Newer versions are left for a downgrade's sake.
/// Windows can't delete a file while it's mapped, and readers may still
/// hold the old index for a moment, so failures are retried with backoff
/// and, after that, at the next start.
fn clean(shared: &Shared, root: &Path, want: IndexVersion) {
    let mut wait = Duration::from_millis(50);
    for attempt in 0..CLEAN_ATTEMPTS {
        match clean_once(root, want) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => tracing::debug!("index cleanup: {e}"),
        }
        if attempt + 1 == CLEAN_ATTEMPTS || !shared.pause(wait) {
            break;
        }
        wait = (wait * 2).min(Duration::from_secs(10));
    }
    tracing::warn!("index: some old index directories remain; deleting them at the next start");
}

const CLEAN_ATTEMPTS: u32 = 20;

/// One pass; `true` when nothing obsolete is left.
fn clean_once(root: &Path, want: IndexVersion) -> Result<bool> {
    let complete = root.join(want.dir_name()).exists();
    let mut clean = true;
    for (path, entry) in scan(root)? {
        let obsolete = match entry {
            Entry::Trash => true,
            Entry::Complete(v) => v < want && complete,
            Entry::Building(v) => v != want || complete,
        };
        if !obsolete {
            continue;
        }
        if let Err(e) = discard(&path) {
            tracing::debug!("index cleanup: {}: {e}", path.display());
            clean = false;
        }
    }
    Ok(clean)
}

/// Renames a directory to trash, then deletes it.
fn discard(path: &Path) -> std::io::Result<()> {
    let trash = if path.to_string_lossy().ends_with(TRASH) {
        path.to_path_buf()
    } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(TRASH);
        let trash = path.with_file_name(name);
        if trash.exists() {
            std::fs::remove_dir_all(&trash)?;
        }
        std::fs::rename(path, &trash)?;
        trash
    };
    std::fs::remove_dir_all(&trash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileFields;
    use crate::schema::Fields;
    use crate::{LiveConfig, TorrentDoc};
    use tantivy::TantivyDocument;

    #[test]
    fn a_rename_blocked_for_a_moment_is_retried() {
        // Stands in for a file held open by a scanner on Windows: the
        // target is in the way (a non-empty directory) until the first
        // retry's wait, which fails the rename on every platform. It's
        // cleared there, on this thread: cleared from another thread, a
        // retry could land between `remove_dir_all` emptying the target
        // and removing it, rename onto the empty directory (POSIX allows
        // that), and leave the removal facing a full one.
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("v2-t1.building"), tmp.path().join("v2-t1"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("meta.json"), b"{}").unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("busy"), b"").unwrap();
        let mut waits = Vec::new();
        rename_with(&from, &to, |d| {
            if waits.is_empty() {
                std::fs::remove_dir_all(&to).unwrap();
            }
            waits.push(d);
        })
        .unwrap();
        assert_eq!(waits, [Duration::from_millis(25)], "one retry");
        assert!(to.join("meta.json").exists() && !from.exists());
        // A missing source isn't transient: it fails at once.
        let mut waited = false;
        assert!(rename_with(&from, &tmp.path().join("x"), |_| waited = true).is_err());
        assert!(!waited);
        // Something that stays in the way fails after every attempt,
        // backing off up to 4 s.
        std::fs::create_dir_all(from.join("d")).unwrap();
        let mut waits = Vec::new();
        assert!(rename_with(&from, &to, |d| waits.push(d)).is_err());
        assert_eq!(waits.len() as u32, RENAME_ATTEMPTS - 1);
        assert_eq!(waits[..3], [25, 50, 100].map(Duration::from_millis));
        assert_eq!(waits.last(), Some(&Duration::from_secs(4)));
    }

    #[test]
    fn names_parse_by_kind() {
        let v = |schema, tokenizer| IndexVersion { schema, tokenizer };
        assert_eq!(classify("v4-t1"), Some(Entry::Complete(v(4, 1))));
        assert_eq!(classify("v12-t3.building"), Some(Entry::Building(v(12, 3))));
        assert_eq!(classify("v4-t1.trash"), Some(Entry::Trash));
        assert_eq!(classify("v4-t1.building.trash"), Some(Entry::Trash));
        for bad in [
            "v4",
            "v-t1",
            "v4-t",
            "v+4-t1",
            "x4-t1",
            "v4-t1.old",
            "files",
        ] {
            assert_eq!(classify(bad), None, "{bad}");
        }
        assert_eq!(
            IndexVersion::CURRENT.dir_name(),
            format!("v{SCHEMA_VERSION}-t{TOKENIZER_VERSION}")
        );
        assert!(v(3, 9) < v(4, 1) && v(4, 1) < v(4, 2));
    }

    /// An older index's schema: this build's, except that `junk` has an
    /// inverted index too, a field this build dropped is left over, and
    /// the fields in `drop` didn't exist yet.
    fn older_schema(current: &tantivy::schema::Schema, drop: &[&str]) -> tantivy::schema::Schema {
        use tantivy::schema::{FAST, FieldEntry, INDEXED, Schema};
        let mut b = Schema::builder();
        b.add_text_field("legacy", tantivy::schema::STRING);
        for (_, entry) in current.fields() {
            match entry.name() {
                name if drop.contains(&name) => continue,
                "junk" => b.add_field(FieldEntry::new_u64("junk".into(), (FAST | INDEXED).into())),
                _ => b.add_field(entry.clone()),
            };
        }
        b.build()
    }

    fn create_old(dir: &Path, drop: &[&str], drop_files: &[&str]) {
        std::fs::create_dir_all(dir.join("files")).unwrap();
        tantivy::Index::create_in_dir(dir, older_schema(&crate::schema::build().0, drop)).unwrap();
        tantivy::Index::builder()
            .schema(older_schema(&crate::files::schema().0, drop_files))
            .settings(crate::files::settings())
            .create_in_dir(dir.join("files"))
            .unwrap();
    }

    /// `doc` without the values of fields the index lacks.
    fn strip(doc: TantivyDocument) -> TantivyDocument {
        let mut out = TantivyDocument::new();
        for (f, v) in doc.field_values() {
            if f != crate::compile::ABSENT {
                out.add_field_value(f, v);
            }
        }
        out
    }

    /// Fills an older index the way the build that made it would have:
    /// documents carry only the fields it has.
    fn fill_old(dir: &Path, store: &mut Store, fields: &Fields, file_fields: &FileFields) {
        let open = |d: &Path| {
            let index = tantivy::Index::open_in_dir(d).unwrap();
            index.tokenizers().register(
                crate::tokenizer::NAME_TOKENIZER,
                crate::tokenizer::NameTokenizer::default(),
            );
            index.writer::<TantivyDocument>(15 << 20).unwrap()
        };
        let (mut w, mut fw) = (open(dir), open(&dir.join("files")));
        for id in store.ids_after(0, 10_000).unwrap() {
            let row = store.torrent(id).unwrap().unwrap();
            let tree = store.files(id).unwrap().unwrap();
            w.add_document(strip(TorrentDoc::new(&row, &tree).to_document(fields)))
                .unwrap();
            for d in crate::files::file_docs(id as u64, &tree) {
                fw.add_document(strip(d.to_document(file_fields))).unwrap();
            }
        }
        w.commit().unwrap();
        fw.commit().unwrap();
    }

    fn add(store: &mut Store, name: &str) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        let d = |e: Vec<(&str, OwnedValue)>| {
            OwnedValue::Dict(
                e.into_iter()
                    .map(|(k, v)| (k.as_bytes().to_vec(), v))
                    .collect(),
            )
        };
        let raw = to_vec(&d(vec![
            ("length", OwnedValue::Int(2_000_000_000)),
            ("name", b(name)),
            ("piece length", OwnedValue::Int(1 << 16)),
            ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ]));
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

    const NOW: i64 = 1_700_000_100;

    fn found(index: &SearchIndex, q: &str) -> usize {
        let node = crate::parse(q).unwrap();
        index.search(&index.query(&node, NOW), 10).unwrap().total
    }

    fn found_sorted(index: &SearchIndex, q: &str, sort: crate::Sort) -> usize {
        let node = crate::parse(q).unwrap();
        index
            .search_with_fallback(&node, NOW, sort, 10, 0)
            .unwrap()
            .0
            .total
    }

    #[test]
    fn an_older_index_lacking_new_fields_serves_until_the_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..50 {
            add(&mut writer, &format!("Old.Show.{i}.1080p.mkv"));
        }
        let old = IndexVersion {
            schema: 1,
            tokenizer: 1,
        };
        let new = IndexVersion {
            schema: 2,
            tokenizer: 1,
        };

        // Lacking a field every search needs is a schema mismatch.
        let broken = root.join("v0-t1");
        create_old(&broken, &["text"], &[]);
        assert!(SearchIndex::open_existing(&broken).unwrap().is_none());

        // `size` (a sort), `tag` (facets) and the files' `ext` are newer
        // than this index.
        let dir = root.join(old.dir_name());
        create_old(&dir, &["size", "tag"], &["ext"]);
        let (probe, compat) = SearchIndex::open_existing(&dir).unwrap().unwrap();
        assert_eq!(compat, Compat::ReadOnly);
        assert_eq!(probe.missing_fields(), ["size", "tag", "file.ext"]);
        assert!(matches!(probe.writer(HEAP), Err(Error::ReadOnly)));
        fill_old(&dir, &mut writer, probe.fields(), probe.file_fields());
        drop(probe);
        // Enriched after the old index was built: only the new one has
        // the tags.
        lodestar_pipeline::enrich::enrich_stored(&mut writer, 0, 100)
            .unwrap()
            .unwrap();

        let hold = Arc::new(AtomicBool::new(true));
        let config = ServingConfig {
            live: LiveConfig {
                heap_bytes: HEAP,
                poll: Duration::from_millis(20),
                commit_every: Duration::from_millis(100),
                batch: 10,
                ..LiveConfig::default()
            },
            rebuild_commit_docs: 20,
            version: new,
            hold_before_finish: Some(hold.clone()),
            ..ServingConfig::default()
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        let p = serving.progress().unwrap();
        assert_eq!(p.from, old, "the newest servable");
        assert_eq!(p.unavailable, ["size", "tag", "file.ext"]);
        let index = serving.index();
        // Queries compiled for this build run on it without errors; a
        // clause on a field it lacks matches nothing.
        assert_eq!(found(&index, "old show"), 50);
        assert_eq!(found(&index, "old show junk:0"), 50);
        assert_eq!(found(&index, "res:1080p"), 0);
        assert_eq!(found(&index, "old show size:>1"), 0);
        assert_eq!(found(&index, "size:>1 | show"), 50);
        assert_eq!(found(&index, "file:(ext:mkv)"), 0);
        assert_eq!(found(&index, "file:(show)"), 50);
        // A sort on a missing field falls back to relevance.
        let by_size = crate::Sort::Size { desc: true };
        assert!(!index.sort_available(by_size));
        assert_eq!(found_sorted(&index, "show", by_size), 50);
        drop(index);

        hold.store(false, Ordering::Relaxed);
        let start = std::time::Instant::now();
        while serving.rebuilding() {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        let index = serving.index();
        assert!(index.missing_fields().is_empty());
        assert_eq!(found(&index, "res:1080p"), 50);
        assert_eq!(found(&index, "old show size:>1"), 50);
        assert_eq!(found(&index, "file:(ext:mkv)"), 50);
        assert!(index.sort_available(by_size));
        assert_eq!(found_sorted(&index, "show", by_size), 50);
        drop(index);
        add(&mut writer, "New.Show.mkv");
        while found(&serving.index(), "new show") == 0 {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        serving.shutdown().unwrap();
    }

    const HEAP: usize = 15 << 20;

    /// An index of this build's schema under `root`, filled from `store`,
    /// that a build with a newer tokenizer serves (and keeps following)
    /// while it rebuilds.
    fn exact_old(root: &Path, store: &mut Store) -> (IndexVersion, IndexVersion) {
        let old = IndexVersion::CURRENT;
        let index = SearchIndex::open(&root.join(old.dir_name())).unwrap();
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(store, 100_000).unwrap();
        w.commit().unwrap();
        w.finish().unwrap();
        let new = IndexVersion {
            tokenizer: old.tokenizer + 1,
            ..old
        };
        (old, new)
    }

    fn quick(version: IndexVersion) -> ServingConfig {
        ServingConfig {
            live: LiveConfig {
                heap_bytes: HEAP,
                poll: Duration::from_millis(20),
                commit_every: Duration::from_millis(50),
                batch: 50,
                ..LiveConfig::default()
            },
            rebuild_commit_docs: 100,
            version,
            ..ServingConfig::default()
        }
    }

    #[test]
    fn expired_leases_are_swept_without_another_search() {
        use crate::{CancelToken, Lease, LeaseConfig, SearchHandle};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        add(&mut writer, "Old.Show.mkv");
        // This build's index exists: nothing to rebuild.
        let (current, _) = exact_old(&root, &mut writer);
        let config = ServingConfig {
            leases: LeaseConfig {
                ttl: Duration::from_millis(100),
                ..LeaseConfig::default()
            },
            ..quick(current)
        };
        let serving = Arc::new(Serving::open_or_rebuild(&root, &db, config).unwrap());
        assert!(!serving.rebuilding());
        let handle = SearchHandle::serving(serving.clone());
        let cancel = CancelToken::new();
        let node = crate::parse("show").unwrap();
        let lease = Arc::new(
            Lease::new(handle.index(), &node, NOW, crate::Sort::Relevance, &cancel).unwrap(),
        );
        let held = Arc::downgrade(&lease);
        handle.leases().insert(lease, 1);
        // Nothing takes or reads a lease again; the sweeper drops it, and
        // with it the searcher it pinned.
        wait_for("the sweep", || handle.leases().is_empty());
        assert!(held.upgrade().is_none());
        drop(handle);
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }

    #[test]
    fn the_sweeper_runs_at_a_quarter_ttl_and_stops_at_once() {
        use crate::LeaseConfig;
        let every = |ttl| {
            sweep_every(&LeaseConfig {
                ttl,
                ..LeaseConfig::default()
            })
        };
        assert_eq!(every(Duration::from_secs(600)), SWEEP_AT_MOST_EVERY);
        assert_eq!(every(Duration::from_secs(60)), Duration::from_secs(15));
        assert_eq!(every(Duration::ZERO), Duration::from_millis(1));
        // Stopping unparks it rather than waiting out its 30 s.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        let (current, _) = exact_old(&root, &mut writer);
        let serving = Serving::open_or_rebuild(&root, &db, quick(current)).unwrap();
        let start = std::time::Instant::now();
        serving.shutdown().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{:?}",
            start.elapsed()
        );
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_lease_reads_its_snapshot_across_a_swap_and_the_old_index_waits_for_it() {
        use crate::{CancelToken, Lease, LeaseConfig, SearchHandle, Start};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..300 {
            add(&mut writer, &format!("Old.Show.{i}.1080p.mkv"));
        }
        let (old, new) = exact_old(&root, &mut writer);
        let hold = Arc::new(AtomicBool::new(true));
        let config = ServingConfig {
            hold_before_finish: Some(hold.clone()),
            leases: LeaseConfig {
                ttl: Duration::from_millis(1500),
                ..LeaseConfig::default()
            },
            ..quick(new)
        };
        let serving = Arc::new(Serving::open_or_rebuild(&root, &db, config).unwrap());
        let handle = SearchHandle::serving(serving.clone());
        assert_eq!(serving.progress().unwrap().from, old);

        // The first page, from the old index; the rest through the lease.
        let cancel = CancelToken::new();
        let node = crate::parse("show").unwrap();
        let lease =
            Lease::new(handle.index(), &node, NOW, crate::Sort::Relevance, &cancel).unwrap();
        let want: Vec<u64> = lease
            .page(Start::Offset(0), 1_000, &cancel)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(want.len(), 300);
        let first = lease.page(Start::Offset(0), 37, &cancel).unwrap();
        let mut got: Vec<u64> = first.hits.iter().map(|h| h.id).collect();
        let mut next = first.next;
        let id = handle.leases().insert(Arc::new(lease), 1);
        let page = |next: &mut Option<crate::Position>, got: &mut Vec<u64>| {
            let lease = handle.leases().get(&id, 1).unwrap();
            let p = lease
                .page(Start::After(next.unwrap()), 37, &cancel)
                .unwrap();
            got.extend(p.hits.iter().map(|h| h.id));
            *next = p.next;
        };
        page(&mut next, &mut got);

        // Commits land on the old index, then the swap happens, mid-scroll.
        for i in 0..20 {
            add(&mut writer, &format!("New.Show.{i}.mkv"));
        }
        // A reader scrolling slowly keeps its lease alive meanwhile.
        let touch = || drop(handle.leases().get(&id, 1).unwrap());
        wait_for("the old index to take the new torrents", || {
            touch();
            found(&serving.index(), "new show") == 20
        });
        page(&mut next, &mut got);
        hold.store(false, Ordering::Relaxed);
        wait_for("the swap", || {
            touch();
            !serving.rebuilding()
        });
        let old_dir = root.join(old.dir_name());
        assert!(old_dir.exists(), "a lease still reads the old index");
        while next.is_some() {
            page(&mut next, &mut got);
            assert!(old_dir.exists());
        }
        let unique: std::collections::HashSet<u64> = got.iter().copied().collect();
        assert_eq!(unique.len(), got.len(), "a duplicate across the swap");
        assert_eq!(got, want, "the snapshot's hits, in its order");
        // New searches read the new index, with everything.
        assert_eq!(found(&serving.index(), "show"), 320);

        // Once the lease expires, the old directory goes.
        wait_for("the old index to be deleted", || !old_dir.exists());
        assert!(handle.leases().is_empty());
        drop(handle);
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }

    #[test]
    fn a_failed_swap_leaves_the_old_index_serving_and_following_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..40 {
            add(&mut writer, &format!("Old.Show.{i}.mkv"));
        }
        let (old, new) = exact_old(&root, &mut writer);
        let config = ServingConfig {
            fail_live_at_swap: true,
            ..quick(new)
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        wait_for("the swap to fail", || {
            serving.progress().unwrap().phase == RebuildPhase::Failed
        });
        let index = serving.index();
        assert!(index.missing_fields().is_empty());
        assert_eq!(found(&index, "old show"), 40);
        drop(index);
        assert!(root.join(old.dir_name()).exists());
        // The old index's indexer was stopped for the swap; it's running
        // again, so new torrents still become searchable.
        add(&mut writer, "Fresh.Thing.mkv");
        wait_for("the restarted indexer", || {
            found(&serving.index(), "fresh thing") == 1
        });
        serving.shutdown().unwrap();
    }
}
