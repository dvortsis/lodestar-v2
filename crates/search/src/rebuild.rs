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
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lodestar_store::{Store, StoreConfig};
use tantivy::directory::{Directory, DirectoryLock, Lock, MmapDirectory};

use crate::index::{Compat, Error, Indexer, Result, SearchIndex};
use crate::live::{IndexStats, LiveConfig, LiveIndexer};
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

fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(Error::Io)?;
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
    /// Torrents in the store when the build started.
    pub total: u64,
    /// Unix seconds.
    pub started_at: u64,
    /// The index serving meanwhile, and the one being built.
    pub from: IndexVersion,
    pub to: IndexVersion,
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

    /// Adds up to `batch` torrents (and replays `reindex_log`) like the
    /// live indexer does. Returns how many it added or replaced; 0 means
    /// it has caught up with the store.
    pub fn step(&mut self, store: &mut Store, batch: usize) -> Result<usize> {
        self.indexer.catch_up(store, batch)
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
}

impl Default for ServingConfig {
    fn default() -> Self {
        ServingConfig {
            live: LiveConfig::default(),
            store: StoreConfig::default(),
            rebuild_commit_docs: 50_000,
            version: IndexVersion::CURRENT,
        }
    }
}

/// State shared with the background thread.
struct Shared {
    current: RwLock<Arc<SearchIndex>>,
    progress: Mutex<Option<RebuildProgress>>,
    live: Mutex<Option<LiveIndexer>>,
    stats: Arc<IndexStats>,
    stop: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn set_phase(&self, phase: RebuildPhase) {
        if let Some(p) = lock(&self.progress).as_mut() {
            p.phase = phase;
        }
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
/// they move to the new index between queries.
pub struct Serving {
    shared: Arc<Shared>,
    background: Option<JoinHandle<()>>,
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

impl Serving {
    /// Opens the index under `root` (e.g. `data/index`) and starts the
    /// live indexer, reading the store at `store_path`.
    ///
    /// When `config.version`'s index isn't complete, the newest complete
    /// older index serves while it's built in the background, and the
    /// handle swaps to it once it has caught up with the store. With
    /// nothing older to serve (a new install, or a schema too different
    /// to query), this version's index is opened directly and fills as
    /// the live indexer catches up, as it always did.
    ///
    /// Fails with [`Error::Locked`] while another handle serves `root`.
    pub fn open_or_rebuild(
        root: &Path,
        store_path: &Path,
        config: ServingConfig,
    ) -> Result<Serving> {
        std::fs::create_dir_all(root).map_err(Error::Io)?;
        let dir_lock = MmapDirectory::open(root)?
            .acquire_lock(&Lock {
                filepath: PathBuf::from(LOCK_FILE),
                is_blocking: false,
            })
            .map_err(|e| match e {
                tantivy::directory::error::LockError::LockBusy => Error::Locked(root.to_path_buf()),
                tantivy::directory::error::LockError::IoError(io) => {
                    Error::Io(std::io::Error::new(io.kind(), io.to_string()))
                }
            })?;
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
                let progress = RebuildProgress {
                    phase: RebuildPhase::Building,
                    done: builder.indexed(),
                    total: store.count()?,
                    started_at: unix_now(),
                    from,
                    to: want,
                };
                Some((store, builder, progress))
            }
            None => None,
        };
        let stats = Arc::new(IndexStats::default());
        let live = if writable {
            Some(LiveIndexer::spawn_with_stats(
                serving.clone(),
                open_reader()?,
                config.live.clone(),
                stats.clone(),
            )?)
        } else {
            None
        };
        let shared = Arc::new(Shared {
            current: RwLock::new(Arc::new(serving)),
            progress: Mutex::new(None),
            live: Mutex::new(live),
            stats,
            stop: AtomicBool::new(false),
        });

        let thread = std::thread::Builder::new().name("index-rebuild".into());
        let background = match build {
            Some((mut store, builder, progress)) => {
                tracing::info!("index: rebuilding {want} while {} serves", progress.from);
                *lock(&shared.progress) = Some(progress);
                let (shared, root) = (shared.clone(), root.to_path_buf());
                thread.spawn(move || {
                    match rebuild(&shared, builder, &mut store, &config) {
                        Ok(Some(new)) => {
                            let live = LiveIndexer::spawn_with_stats(
                                new.clone(),
                                store,
                                config.live.clone(),
                                shared.stats.clone(),
                            );
                            match live {
                                Ok(live) => swap(&shared, new, live),
                                Err(e) => {
                                    // The next start finds it complete.
                                    tracing::error!("index: can't follow the rebuilt index: {e}");
                                    shared.set_phase(RebuildPhase::Failed);
                                    return;
                                }
                            }
                            clean(&shared, &root, want);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::error!("index: rebuilding {want} failed: {e}");
                            shared.set_phase(RebuildPhase::Failed);
                        }
                    }
                })
            }
            None => {
                let (shared, root) = (shared.clone(), root.to_path_buf());
                thread.spawn(move || clean(&shared, &root, want))
            }
        }
        .map_err(Error::Io)?;
        Ok(Serving {
            shared,
            background: Some(background),
            root: root.to_path_buf(),
            _lock: dir_lock,
        })
    }

    /// The index to run the next query on.
    pub fn index(&self) -> Arc<SearchIndex> {
        self.shared
            .current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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

    /// Stops the rebuild (which commits what it has, to resume next
    /// time) and the live indexer (which commits what's pending).
    pub fn shutdown(mut self) -> Result<()> {
        self.halt()
    }

    fn halt(&mut self) -> Result<()> {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.background.take() {
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

/// The newest complete index older than `want` that can serve this
/// build's queries.
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
        match SearchIndex::open_existing(&path) {
            Ok(Some((index, compat))) => return Ok(Some((v, index, compat))),
            Ok(None) => {
                tracing::warn!("index: {v} lacks fields this build queries; not serving it")
            }
            Err(e) => tracing::warn!("index: can't open {v} to serve during the rebuild: {e}"),
        }
    }
    Ok(None)
}

/// Builds until caught up, then finishes and opens the result. `None`
/// when stopped first; what's committed is resumed next time.
fn rebuild(
    shared: &Shared,
    mut builder: Rebuild,
    store: &mut Store,
    config: &ServingConfig,
) -> Result<Option<SearchIndex>> {
    let mut done = builder.indexed();
    loop {
        if shared.stopping() {
            builder.commit()?;
            return Ok(None);
        }
        match builder.step(store, config.live.batch) {
            Ok(0) => break,
            Ok(n) => {
                done += n as u64;
                if let Some(p) = lock(&shared.progress).as_mut() {
                    p.done = done;
                }
                if builder.pending() >= config.rebuild_commit_docs {
                    builder.commit()?;
                }
            }
            Err(e) => {
                // A busy database; Tantivy errors repeat and are logged
                // each time until the next start.
                shared
                    .stats
                    .errors
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!("index rebuild: {e}");
                shared.pause(config.live.poll);
            }
        }
    }
    shared.set_phase(RebuildPhase::Finishing);
    let final_dir = builder.finish()?;
    if shared.stopping() {
        // Complete on disk: the next start serves it.
        return Ok(None);
    }
    shared.set_phase(RebuildPhase::Swapping);
    let new = SearchIndex::open(&final_dir)?;
    // Torrents stored since the last step, so readers lose nothing at
    // the swap; the live indexer would add them within its commit
    // interval anyway.
    let mut w = new.writer(config.live.heap_bytes)?;
    while w.catch_up(store, config.live.batch)? > 0 {}
    w.commit()?;
    w.finish()?;
    Ok(Some(new))
}

/// Points readers and live indexing at `new`, then stops the old live
/// indexer, which commits what it has to the old index only.
fn swap(shared: &Shared, new: SearchIndex, live: LiveIndexer) {
    let old_live = lock(&shared.live).replace(live);
    *shared
        .current
        .write()
        .unwrap_or_else(PoisonError::into_inner) = Arc::new(new);
    *lock(&shared.progress) = None;
    if let Some(old) = old_live
        && let Err(e) = old.stop()
    {
        tracing::warn!("index: stopping the old indexer: {e}");
    }
    tracing::info!("index: swapped to the rebuilt index");
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

    /// An index whose schema has every field this build uses, but not
    /// identically: here `junk` gained an inverted index and an old field
    /// is left over. `drop_field` builds one that lacks a field instead.
    fn older_schema(drop_field: Option<&str>) -> tantivy::schema::Schema {
        use tantivy::schema::{FAST, FieldEntry, INDEXED, Schema};
        let (current, _) = crate::schema::build();
        let mut b = Schema::builder();
        b.add_text_field("legacy", tantivy::schema::STRING);
        for (_, entry) in current.fields() {
            match entry.name() {
                "junk" => b.add_field(FieldEntry::new_u64("junk".into(), (FAST | INDEXED).into())),
                name if Some(name) == drop_field => continue,
                _ => b.add_field(entry.clone()),
            };
        }
        b.build()
    }

    fn create_old(dir: &Path, drop_field: Option<&str>) {
        std::fs::create_dir_all(dir.join("files")).unwrap();
        tantivy::Index::create_in_dir(dir, older_schema(drop_field)).unwrap();
        tantivy::Index::builder()
            .schema(crate::files::schema().0)
            .settings(crate::files::settings())
            .create_in_dir(dir.join("files"))
            .unwrap();
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
            ("length", OwnedValue::Int(1000)),
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

    fn found(index: &SearchIndex, q: &str) -> usize {
        let node = crate::parse(q).unwrap();
        index
            .search(&index.query(&node, 1_700_000_100), 10)
            .unwrap()
            .total
    }

    #[test]
    fn a_different_schema_serves_read_only_by_field_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..50 {
            add(&mut writer, &format!("Old.Show.{i}"));
        }
        let old = IndexVersion {
            schema: 1,
            tokenizer: 1,
        };
        let new = IndexVersion {
            schema: 2,
            tokenizer: 1,
        };

        // Lacking a field this build queries, it can't serve.
        create_old(&root.join("v0-t1"), Some("title_stem"));
        assert!(
            SearchIndex::open_existing(&root.join("v0-t1"))
                .unwrap()
                .is_none()
        );

        let dir = root.join(old.dir_name());
        create_old(&dir, None);
        let (index, compat) = SearchIndex::open_existing(&dir).unwrap().unwrap();
        assert_eq!(compat, Compat::ReadOnly);
        assert!(matches!(index.writer(HEAP), Err(Error::ReadOnly)));
        let mut w = index.force_writable().writer(HEAP).unwrap();
        while w.catch_up(&mut writer, 100).unwrap() > 0 {}
        w.commit().unwrap();
        w.finish().unwrap();

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
            ..ServingConfig::default()
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        assert_eq!(serving.progress().unwrap().from, old, "the newest servable");
        // Queries compiled for this build run on it, fast fields included.
        assert_eq!(found(&serving.index(), "old show"), 50);
        assert_eq!(found(&serving.index(), "old show junk:0"), 50);
        let start = std::time::Instant::now();
        while serving.rebuilding() {
            assert_eq!(found(&serving.index(), "old show"), 50);
            assert!(start.elapsed() < Duration::from_secs(30));
        }
        add(&mut writer, "New.Show");
        while found(&serving.index(), "new show") == 0 {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(serving.index().num_docs(), 51);
        serving.shutdown().unwrap();
    }

    const HEAP: usize = 15 << 20;
}
