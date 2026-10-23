//! Near-real-time indexing (architecture §8): one thread follows the
//! store through its own read-only connection, adds new torrents and
//! commits every couple of seconds or few thousand documents, so a new
//! torrent is searchable within the 5 s budget (§3).

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestar_store::Store;

use crate::index::{CatchUp, Result, SearchIndex};

#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// Writer heap (§8: 256 MB).
    pub heap_bytes: usize,
    /// How often to look for new torrents when idle.
    pub poll: Duration,
    /// Commit at least this often while documents are pending...
    pub commit_every: Duration,
    /// ...or once this many are pending.
    pub commit_docs: usize,
    /// Torrents read from the store per step.
    pub batch: usize,
}

impl Default for LiveConfig {
    fn default() -> Self {
        LiveConfig {
            heap_bytes: 256 << 20,
            poll: Duration::from_millis(500),
            commit_every: Duration::from_secs(2),
            commit_docs: 5000,
            batch: 1000,
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// Documents added since start.
    pub indexed: AtomicU64,
    /// The highest id searchable.
    pub watermark: AtomicI64,
    pub commits: AtomicU64,
    pub errors: AtomicU64,
    /// Unix seconds of the last commit.
    pub last_commit: AtomicU64,
}

/// What a live indexer reports after each commit that made new torrents
/// searchable (B4): the SSE hub turns it into the `torrents` event and
/// the "N new" banner (§10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Committed {
    /// Torrents new to the index in this commit. Re-indexed ones
    /// (`reindex_log`) aren't counted.
    pub docs_added: u64,
    /// How many of `docs_added` are Adult: indexed with `type:adult`,
    /// the term that hides them while "Show adult content" is off
    /// (§20.9), so whoever announces them can leave them out.
    pub adult_added: u64,
    /// The highest torrent id searchable after it.
    pub max_torrent_id: i64,
    /// Unix seconds.
    pub at: u64,
}

type Sink = Arc<dyn Fn(Committed) + Send + Sync>;

/// Where live indexers report their commits. [`Serving`](crate::Serving)
/// keeps one in its shared state and hands it to every live indexer it
/// starts (at open, at a swap, after a failed swap), so a sink set once
/// outlives an index swap. A rebuild's own commits never report.
///
/// The sink runs on the indexer's thread, after the commit, and must
/// not block: hand the value off and return. With no sink set, a commit
/// reports nothing.
///
/// It has two slots: the one [`CommitHook::set`] replaces (the SSE hub's)
/// and any number [`CommitHook::add`] appends (Insights' searchable-time
/// recorder, B11a), which `set` and `clear` leave alone.
#[derive(Default)]
pub struct CommitHook {
    sink: RwLock<Option<Sink>>,
    extra: RwLock<Vec<Sink>>,
}

impl std::fmt::Debug for CommitHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self
            .sink
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        let extra = self
            .extra
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("CommitHook")
            .field("set", &set)
            .field("extra", &extra)
            .finish()
    }
}

impl CommitHook {
    /// Reports every later commit to `sink`, replacing any earlier one.
    pub fn set(&self, sink: impl Fn(Committed) + Send + Sync + 'static) {
        *self.sink.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(sink));
    }

    /// Stops reporting to the sink [`CommitHook::set`] gave. Sinks
    /// [`CommitHook::add`] gave keep theirs.
    pub fn clear(&self) {
        *self.sink.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Also reports every later commit to `sink`, beside the one `set`
    /// gave, for as long as the hook lives. The same rule applies: it
    /// runs on the indexer's thread and must not block.
    pub fn add(&self, sink: impl Fn(Committed) + Send + Sync + 'static) {
        self.extra
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::new(sink));
    }

    /// Hands `c` to every sink. The locks are held only to clone the
    /// sinks, never while they run.
    pub(crate) fn publish(&self, c: Committed) {
        let sink = self
            .sink
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let extra = self
            .extra
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for sink in sink.iter().chain(&extra) {
            sink(c);
        }
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug)]
pub struct LiveIndexer {
    stop: Arc<AtomicBool>,
    stats: Arc<IndexStats>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl LiveIndexer {
    /// Starts following `store` (a reader connection) into `index`.
    pub fn spawn(index: SearchIndex, store: Store, config: LiveConfig) -> Result<LiveIndexer> {
        LiveIndexer::spawn_with_stats(index, store, config, Arc::default(), Arc::default())
    }

    /// The same, counting into `stats` and reporting commits to `hook`,
    /// which outlive this indexer: the serving handle keeps one of each
    /// across an index swap.
    pub fn spawn_with_stats(
        index: SearchIndex,
        mut store: Store,
        config: LiveConfig,
        stats: Arc<IndexStats>,
        hook: Arc<CommitHook>,
    ) -> Result<LiveIndexer> {
        let mut indexer = index.writer(config.heap_bytes)?;
        let stop = Arc::new(AtomicBool::new(false));
        stats.watermark.store(indexer.last_id(), Ordering::Relaxed);
        let (s, st) = (stop.clone(), stats.clone());
        let thread = std::thread::Builder::new()
            .name("indexer".into())
            .spawn(move || {
                let mut since = Instant::now();
                // New torrents since the last commit, for the hook
                // (`replayed` unused).
                let mut new_docs = CatchUp::default();
                let commit = |indexer: &mut crate::Indexer, new_docs: &mut CatchUp| -> Result<()> {
                    indexer.commit()?;
                    st.watermark.store(indexer.last_id(), Ordering::Relaxed);
                    st.commits.fetch_add(1, Ordering::Relaxed);
                    let now = unix_now();
                    st.last_commit.store(now, Ordering::Relaxed);
                    if new_docs.added > 0 {
                        let c = std::mem::take(new_docs);
                        hook.publish(Committed {
                            docs_added: c.added as u64,
                            adult_added: c.adult_added as u64,
                            max_torrent_id: indexer.last_id(),
                            at: now,
                        });
                    }
                    Ok(())
                };
                while !s.load(Ordering::Relaxed) {
                    let added = match indexer.catch_up_counted(&mut store, config.batch) {
                        Ok(c) => {
                            new_docs.added += c.added;
                            new_docs.adult_added += c.adult_added;
                            c.added + c.replayed
                        }
                        Err(e) => {
                            // Transient (a busy database); try again shortly.
                            st.errors.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!("indexer: {e}");
                            std::thread::sleep(config.poll);
                            continue;
                        }
                    };
                    st.indexed.fetch_add(added as u64, Ordering::Relaxed);
                    let due = indexer.pending() >= config.commit_docs
                        || (indexer.pending() > 0 && since.elapsed() >= config.commit_every);
                    if due {
                        commit(&mut indexer, &mut new_docs)?;
                        since = Instant::now();
                    }
                    if added == 0 {
                        std::thread::sleep(config.poll);
                    }
                }
                if indexer.pending() > 0 {
                    commit(&mut indexer, &mut new_docs)?;
                }
                Ok(())
            })
            .map_err(crate::index::Error::Io)?;
        Ok(LiveIndexer {
            stop,
            stats,
            thread: Some(thread),
        })
    }

    pub fn stats(&self) -> Arc<IndexStats> {
        self.stats.clone()
    }

    /// Commits what's pending and stops.
    pub fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(crate::index::Error::Corrupt("indexer thread panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for LiveIndexer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use lodestar_store::StoreConfig;

    use super::*;
    use crate::{IndexVersion, Serving, ServingConfig};

    const HEAP: usize = 15 << 20;

    fn add(store: &mut Store, name: &str) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        let raw = to_vec(&OwnedValue::Dict(
            [
                ("length", OwnedValue::Int(1 << 20)),
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

    fn quick() -> LiveConfig {
        LiveConfig {
            heap_bytes: HEAP,
            poll: Duration::from_millis(10),
            commit_every: Duration::from_millis(30),
            batch: 50,
            ..LiveConfig::default()
        }
    }

    /// A hook collecting what it's handed.
    fn collecting() -> (Arc<CommitHook>, Arc<Mutex<Vec<Committed>>>) {
        let hook = Arc::new(CommitHook::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        hook.set(move |c| s.lock().unwrap().push(c));
        (hook, seen)
    }

    /// Waits until the reports add up to `n` torrents.
    fn wait_for(seen: &Mutex<Vec<Committed>>, n: u64) -> Vec<Committed> {
        let start = Instant::now();
        loop {
            let got = seen.lock().unwrap().clone();
            if got.iter().map(|c| c.docs_added).sum::<u64>() >= n {
                return got;
            }
            assert!(start.elapsed() < Duration::from_secs(30), "{got:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn each_commit_reports_once_with_its_count_and_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..3 {
            add(&mut writer, &format!("Batch.One.{i}"));
        }
        let index = SearchIndex::open(&tmp.path().join("index")).unwrap();
        let (hook, seen) = collecting();
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let config = LiveConfig {
            // One commit per batch here, whatever the timing.
            commit_every: Duration::from_secs(3600),
            commit_docs: 3,
            ..quick()
        };
        let live = LiveIndexer::spawn_with_stats(
            index.clone(),
            reader,
            config,
            Arc::default(),
            hook.clone(),
        )
        .unwrap();
        let first = wait_for(&seen, 3);
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!((first[0].docs_added, first[0].max_torrent_id), (3, 3));
        assert!(first[0].at > 0);
        // What the report names is searchable when it arrives.
        assert_eq!(index.watermark().unwrap(), 3);
        for i in 0..3 {
            add(&mut writer, &format!("Batch.Two.{i}"));
        }
        let both = wait_for(&seen, 6);
        assert_eq!(both.len(), 2, "{both:?}");
        assert_eq!((both[1].docs_added, both[1].max_torrent_id), (3, 6));
        // Without a sink, commits go on and report nothing.
        hook.clear();
        for i in 0..3 {
            add(&mut writer, &format!("Unreported.{i}"));
        }
        let start = Instant::now();
        while index.watermark().unwrap() < 9 {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        live.stop().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// A sink `add` gave runs beside the one `set` gave, and outlives
    /// `set` replacing it and `clear`.
    #[test]
    fn added_sinks_report_beside_the_set_one() {
        let (hook, seen) = collecting();
        let extra = Arc::new(Mutex::new(Vec::new()));
        let e = extra.clone();
        hook.add(move |c| e.lock().unwrap().push(c));
        let c = |n| Committed {
            docs_added: n,
            adult_added: 0,
            max_torrent_id: n as i64,
            at: 1,
        };
        hook.publish(c(1));
        hook.set(|_| {});
        hook.publish(c(2));
        hook.clear();
        hook.publish(c(3));
        assert_eq!(*seen.lock().unwrap(), [c(1)]);
        assert_eq!(*extra.lock().unwrap(), [c(1), c(2), c(3)]);
        assert!(format!("{hook:?}").contains("extra: 1"));
    }

    #[test]
    fn re_indexing_alone_reports_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        add(&mut writer, "Only.One");
        let index = SearchIndex::open(&tmp.path().join("index")).unwrap();
        let (hook, seen) = collecting();
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let live =
            LiveIndexer::spawn_with_stats(index.clone(), reader, quick(), Arc::default(), hook)
                .unwrap();
        wait_for(&seen, 1);
        let stats = live.stats();
        let commits = stats.commits.load(Ordering::Relaxed);
        // New enrichment logs a re-index of the same torrent.
        lodestar_pipeline::enrich::enrich_stored(&mut writer, 0, 100)
            .unwrap()
            .unwrap();
        let start = Instant::now();
        while stats.commits.load(Ordering::Relaxed) == commits {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        live.stop().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// B4b: each report says how many of its torrents are Adult, as the
    /// index stores them (`type:adult`).
    #[test]
    fn a_report_counts_its_adult_torrents() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for name in [
            "Big.Movie.2019.1080p.mkv",
            "Studio.Scene.XXX.1080p.mp4",
            "Other.Movie.2018.720p.mkv",
            "Another.Clip.XXX.720p.mp4",
            "Third.Movie.2017.2160p.mkv",
        ] {
            add(&mut writer, name);
        }
        lodestar_pipeline::enrich::enrich_stored(&mut writer, 0, 100)
            .unwrap()
            .unwrap();
        let index = SearchIndex::open(&tmp.path().join("index")).unwrap();
        let (hook, seen) = collecting();
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let config = LiveConfig {
            commit_every: Duration::from_secs(3600),
            commit_docs: 5,
            ..quick()
        };
        let live =
            LiveIndexer::spawn_with_stats(index.clone(), reader, config, Arc::default(), hook)
                .unwrap();
        let got = wait_for(&seen, 5);
        live.stop().unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((got[0].docs_added, got[0].adult_added), (5, 2));
        // The swap counts the same way, from the index itself.
        assert_eq!(index.adult_between(0, 5).unwrap(), 2);
        assert_eq!(index.adult_between(2, 5).unwrap(), 1);
        assert_eq!(index.adult_between(0, 1).unwrap(), 0);
        assert_eq!(index.adult_between(1, 2).unwrap(), 1);
        assert_eq!(index.adult_between(4, 5).unwrap(), 0);
    }

    /// An index of this build's version under `root`, filled from
    /// `store`, which a build with a newer tokenizer serves and keeps
    /// following while it rebuilds.
    fn exact_old(root: &Path, store: &mut Store) -> IndexVersion {
        let old = IndexVersion::CURRENT;
        let index = SearchIndex::open(&root.join(old.dir_name())).unwrap();
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(store, 100_000).unwrap();
        w.commit().unwrap();
        w.finish().unwrap();
        IndexVersion {
            tokenizer: old.tokenizer + 1,
            ..old
        }
    }

    #[test]
    fn the_hook_survives_a_swap_and_counts_every_torrent_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..20 {
            add(&mut writer, &format!("Old.{i}"));
        }
        let new = exact_old(&root, &mut writer);
        let hold = Arc::new(AtomicBool::new(true));
        let config = ServingConfig {
            live: quick(),
            rebuild_commit_docs: 5,
            version: new,
            hold_before_finish: Some(hold.clone()),
            ..ServingConfig::default()
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        assert!(serving.rebuilding());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        serving
            .commit_hook()
            .set(move |c| s.lock().unwrap().push(c));
        // Before the swap: the old index's indexer reports.
        add(&mut writer, "Before.Swap");
        wait_for(&seen, 1);
        // The rebuild's own commits (20 torrents, in fours) reported
        // nothing.
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Torrents stored while the swap runs are reported once, by the
        // swap or by the new indexer.
        let adding = {
            let db = db.clone();
            std::thread::spawn(move || {
                let mut w = Store::open(&db, StoreConfig::default()).unwrap();
                for i in 0..30 {
                    add(&mut w, &format!("During.{i}"));
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        hold.store(false, Ordering::Relaxed);
        let start = Instant::now();
        while serving.rebuilding() {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        adding.join().unwrap();
        // After it: the new index's indexer reports to the same hook.
        add(&mut writer, "After.Swap");
        let got = wait_for(&seen, 32);
        assert_eq!(got.iter().map(|c| c.docs_added).sum::<u64>(), 32, "{got:?}");
        assert_eq!(got.iter().map(|c| c.max_torrent_id).max(), Some(52));
        serving.shutdown().unwrap();
    }

    /// Stores `name` with its classification in one transaction, as the
    /// pipeline's writer does, so no indexer can see it unclassified.
    fn add_classified(store: &mut Store, name: &str) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        let raw = to_vec(&OwnedValue::Dict(
            [
                ("length", OwnedValue::Int(1 << 20)),
                ("name", b(name)),
                ("piece length", OwnedValue::Int(1 << 16)),
                ("pieces", OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
        ));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        let mut prepared = store.preparer().prepare(&info, Some(&raw)).unwrap();
        prepared.enrichment = Some(lodestar_pipeline::enrich::enrich(
            &prepared.name,
            &info.files,
            info.path_encoding,
        ));
        store
            .write(&lodestar_store::WriteBatch {
                prepared: &[lodestar_store::PreparedTorrent {
                    torrent: &prepared,
                    discovered_at: 1_700_000_000,
                    metadata_at: 1_700_000_001,
                    extra_flags: 0,
                    source: lodestar_store::SourceId::new(0).unwrap(),
                    verified: true,
                }],
                ..lodestar_store::WriteBatch::default()
            })
            .unwrap();
    }

    /// B4b-r2: torrents only the swap makes visible (stored after the old
    /// indexer stopped) are reported by the swap, with their Adult count.
    #[test]
    fn the_swap_reports_its_adult_torrents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..20 {
            add(&mut writer, &format!("Old.{i}"));
        }
        let new = exact_old(&root, &mut writer);
        let hold = Arc::new(AtomicBool::new(true));
        let (reached, at_swap) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
        );
        let config = ServingConfig {
            live: quick(),
            rebuild_commit_docs: 5,
            version: new,
            hold_before_finish: Some(hold.clone()),
            hold_at_swap: Some((reached.clone(), at_swap.clone())),
            ..ServingConfig::default()
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        serving
            .commit_hook()
            .set(move |c| s.lock().unwrap().push(c));
        hold.store(false, Ordering::Relaxed);
        let start = Instant::now();
        while !reached.load(Ordering::Relaxed) {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        // The old indexer has stopped; the new one hasn't started.
        add_classified(&mut writer, "Swap.Scene.XXX.1080p.mp4");
        add_classified(&mut writer, "Swap.Movie.2019.1080p.mkv");
        add_classified(&mut writer, "Swap.Clip.XXX.720p.mp4");
        at_swap.store(false, Ordering::Relaxed);
        while serving.rebuilding() {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        let c = got[0];
        assert_eq!((c.docs_added, c.adult_added, c.max_torrent_id), (3, 2, 23));
        serving.shutdown().unwrap();
    }
}
