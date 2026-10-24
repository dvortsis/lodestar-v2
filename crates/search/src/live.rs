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
use crate::suggest::Titles;

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
    /// Tests only: while above zero, a commit fails before it's written
    /// (counting down).
    #[cfg(test)]
    pub(crate) fail_commits: Option<Arc<AtomicU64>>,
    /// Tests only: the same, once the commit is on disk, as when the
    /// reader's reload fails.
    #[cfg(test)]
    pub(crate) fail_after_commits: Option<Arc<AtomicU64>>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        LiveConfig {
            heap_bytes: 256 << 20,
            poll: Duration::from_millis(500),
            commit_every: Duration::from_secs(2),
            commit_docs: 5000,
            batch: 1000,
            #[cfg(test)]
            fail_commits: None,
            #[cfg(test)]
            fail_after_commits: None,
        }
    }
}

/// An error waits at least the poll interval before the indexer retries,
/// doubling up to this.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// Documents added or replaced since start, counted as they're
    /// committed (so once, even when an error makes them read twice).
    pub indexed: AtomicU64,
    /// The highest id searchable.
    pub watermark: AtomicI64,
    pub commits: AtomicU64,
    /// Errors reading the store or committing; the indexer retries each.
    pub errors: AtomicU64,
    /// The latest of them, as text.
    pub last_error: std::sync::Mutex<Option<String>>,
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
        let indexer = index.writer(config.heap_bytes)?;
        // B7: the title FST follows this store, rebuilt off this thread.
        let titles = index.titles().clone();
        if let Some(path) = store.connection().path().filter(|p| !p.is_empty()) {
            titles.follow(path.into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        stats.watermark.store(indexer.last_id(), Ordering::Relaxed);
        let (s, st) = (stop.clone(), stats.clone());
        let thread = std::thread::Builder::new()
            .name("indexer".into())
            .spawn(move || {
                // The highest id reported to the hook: a commit that got
                // to disk but failed after (a reader reload, say) reported
                // nothing, so a reopened writer reports the gap.
                let mut reported = indexer.last_id();
                let mut first = Some(indexer);
                let mut backoff = config.poll;
                loop {
                    // After an error, a fresh writer resumes from the
                    // committed watermarks, as after a crash: work since
                    // the last commit is read from the store again, so
                    // nothing is skipped or reported twice.
                    let result = match first.take() {
                        Some(w) => Ok(w),
                        None => index.writer(config.heap_bytes).and_then(|w| {
                            index.refresh()?;
                            report_gap(&index, &store, (&st, &hook), &mut reported, w.last_id());
                            Ok(w)
                        }),
                    }
                    .and_then(|mut indexer| {
                        st.watermark.store(indexer.last_id(), Ordering::Relaxed);
                        follow(
                            &mut indexer,
                            &mut store,
                            &config,
                            (&st, &hook, &titles),
                            &s,
                            (&mut backoff, &mut reported),
                        )
                    });
                    let Err(e) = result else { return Ok(()) };
                    // Counted and logged every time; the indexer never
                    // stops on an error, only when told to.
                    st.errors.fetch_add(1, Ordering::Relaxed);
                    *st.last_error.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(e.to_string());
                    lodestar_log::log_event!(
                        crate::log_events::INDEXER_RETRY,
                        backoff_ms = backoff.as_millis() as u64,
                        error = crate::log_events::error(&e)
                    );
                    if !pause(&s, backoff) {
                        // Stopping: what failed (the last commit, say) is
                        // what `stop` reports.
                        return Err(e);
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
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

/// Follows the store into `indexer` until stopped, committing what's
/// pending at the end. Returns the first error, from the store or the
/// index, leaving the retry to the caller. A commit, or catching up with
/// nothing pending, resets `backoff`; a commit moves `reported` on.
fn follow(
    indexer: &mut crate::Indexer,
    store: &mut Store,
    config: &LiveConfig,
    (st, hook, titles): (&IndexStats, &CommitHook, &Arc<Titles>),
    stop: &AtomicBool,
    (backoff, reported): (&mut Duration, &mut i64),
) -> Result<()> {
    let mut since = Instant::now();
    let mut pending = Pending::default();
    while !stop.load(Ordering::Relaxed) {
        let CatchUp {
            added,
            adult_added: adult,
            replayed,
        } = indexer.catch_up_counted(store, config.batch)?;
        pending.added += added as u64;
        pending.adult += adult as u64;
        pending.indexed += (added + replayed) as u64;
        let due = indexer.pending() >= config.commit_docs
            || (indexer.pending() > 0 && since.elapsed() >= config.commit_every);
        if due {
            commit(indexer, &mut pending, config, st, hook)?;
            titles.committed();
            (*backoff, *reported) = (config.poll, indexer.last_id());
            since = Instant::now();
        }
        if added + replayed == 0 {
            // Caught up with nothing pending: an error hours from now
            // starts its backoff afresh.
            if indexer.pending() == 0 {
                *backoff = config.poll;
            }
            std::thread::sleep(config.poll);
        }
    }
    // Stopping (a swap or shutdown): this index's titles no longer
    // follow the store.
    titles.unfollow();
    if indexer.pending() > 0 {
        commit(indexer, &mut pending, config, st, hook)?;
        *reported = indexer.last_id();
    }
    Ok(())
}

/// What's been read since the last commit, for the counters and the hook.
#[derive(Default)]
struct Pending {
    /// Torrents new to the index, and how many of them are Adult.
    added: u64,
    adult: u64,
    /// Those plus the re-indexed ones.
    indexed: u64,
}

fn commit(
    indexer: &mut crate::Indexer,
    pending: &mut Pending,
    #[cfg_attr(not(test), allow(unused_variables))] config: &LiveConfig,
    st: &IndexStats,
    hook: &CommitHook,
) -> Result<()> {
    #[cfg(test)]
    let fail = |n: &Option<Arc<AtomicU64>>| {
        // Only the indexer thread counts down; the test only sets it.
        n.as_ref().is_some_and(|n| {
            let left = n.load(Ordering::Relaxed);
            left > 0 && {
                n.store(left - 1, Ordering::Relaxed);
                true
            }
        })
    };
    #[cfg(test)]
    if fail(&config.fail_commits) {
        return Err(crate::index::Error::Corrupt("test: the commit failed"));
    }
    indexer.commit()?;
    #[cfg(test)]
    if fail(&config.fail_after_commits) {
        return Err(crate::index::Error::Corrupt(
            "test: failed after the commit",
        ));
    }
    let Pending {
        added,
        adult,
        indexed,
    } = std::mem::take(pending);
    st.indexed.fetch_add(indexed, Ordering::Relaxed);
    st.watermark.store(indexer.last_id(), Ordering::Relaxed);
    st.commits.fetch_add(1, Ordering::Relaxed);
    let now = unix_now();
    st.last_commit.store(now, Ordering::Relaxed);
    if added > 0 {
        hook.publish(Committed {
            docs_added: added,
            adult_added: adult,
            max_torrent_id: indexer.last_id(),
            at: now,
        });
    }
    Ok(())
}

/// Reports torrents a commit wrote but never reported, because it failed
/// after reaching the disk: those in `(reported, last]`, counted as the
/// swap counts them, and counts them in [`IndexStats::indexed`]. A count
/// that fails only leaves them unannounced.
fn report_gap(
    index: &SearchIndex,
    store: &Store,
    (st, hook): (&IndexStats, &CommitHook),
    reported: &mut i64,
    last: i64,
) {
    if last <= *reported {
        return;
    }
    let counted = crate::rebuild::stored_between(store, *reported, last)
        .and_then(|n| Ok((n, index.adult_between(*reported, last)?)));
    match counted {
        Ok((docs_added, adult_added)) if docs_added > 0 => {
            st.indexed.fetch_add(docs_added, Ordering::Relaxed);
            hook.publish(Committed {
                docs_added,
                adult_added,
                max_torrent_id: last,
                at: unix_now(),
            });
        }
        Ok(_) => {}
        Err(e) => lodestar_log::log_event!(
            crate::log_events::INDEXER_COUNT_FAILED,
            error = crate::log_events::error(&e)
        ),
    }
    *reported = last;
}

/// Sleeps up to `d`, waking early when stopping. Returns whether to go on.
fn pause(stop: &AtomicBool, d: Duration) -> bool {
    let step = Duration::from_millis(20);
    let mut left = d;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let s = left.min(step);
        std::thread::sleep(s);
        left -= s;
    }
    !stop.load(Ordering::Relaxed)
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

    /// An indexer whose counters don't move this long is stuck.
    const STALL: Duration = Duration::from_secs(30);
    /// A backstop: one that keeps moving without getting there.
    const HANG: Duration = Duration::from_secs(300);

    /// What the indexer has done: torrents indexed, commits, errors.
    fn progress(stats: &IndexStats) -> (u64, u64, u64) {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        (get(&stats.indexed), get(&stats.commits), get(&stats.errors))
    }

    /// Waits until `done`. Fails at once, with the error, when the
    /// indexer reports more than `errors` errors in all or its thread has
    /// ended, and when its counters stop moving for [`STALL`].
    fn eventually(
        stats: &IndexStats,
        thread: Option<&JoinHandle<Result<()>>>,
        errors: u64,
        what: &str,
        mut done: impl FnMut() -> bool,
    ) {
        let (start, mut last) = (Instant::now(), progress(stats));
        let mut moved = start;
        while !done() {
            let now = progress(stats);
            let error = || stats.last_error.lock().unwrap().clone();
            assert!(now.2 <= errors, "{what}: the indexer failed: {:?}", error());
            assert!(
                !thread.is_some_and(JoinHandle::is_finished),
                "{what}: the indexer stopped: {:?}",
                error()
            );
            if now != last {
                (last, moved) = (now, Instant::now());
            }
            assert!(moved.elapsed() < STALL, "{what}: stalled at {now:?}");
            assert!(start.elapsed() < HANG, "{what}: never got there");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until the reports add up to `n` torrents, failing as
    /// [`eventually`] does past `errors` errors.
    fn wait_for(
        stats: &IndexStats,
        thread: Option<&JoinHandle<Result<()>>>,
        errors: u64,
        seen: &Mutex<Vec<Committed>>,
        n: u64,
    ) -> Vec<Committed> {
        let sum = || {
            seen.lock()
                .unwrap()
                .iter()
                .map(|c| c.docs_added)
                .sum::<u64>()
        };
        eventually(
            stats,
            thread,
            errors,
            &format!("reports adding up to {n}"),
            || sum() >= n,
        );
        seen.lock().unwrap().clone()
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
        let (stats, thread) = (live.stats(), live.thread.as_ref());
        let first = wait_for(&stats, thread, 0, &seen, 3);
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!((first[0].docs_added, first[0].max_torrent_id), (3, 3));
        assert!(first[0].at > 0);
        // What the report names is searchable when it arrives.
        assert_eq!(index.watermark().unwrap(), 3);
        for i in 0..3 {
            add(&mut writer, &format!("Batch.Two.{i}"));
        }
        let both = wait_for(&stats, thread, 0, &seen, 6);
        assert_eq!(both.len(), 2, "{both:?}");
        assert_eq!((both[1].docs_added, both[1].max_torrent_id), (3, 6));
        // Without a sink, commits go on and report nothing.
        hook.clear();
        for i in 0..3 {
            add(&mut writer, &format!("Unreported.{i}"));
        }
        eventually(&stats, thread, 0, "the watermark reaches 9", || {
            index.watermark().unwrap() >= 9
        });
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
    fn a_commit_that_fails_is_retried_and_nothing_is_skipped_or_counted_twice() {
        recovers_from_a_failed_commit(false);
    }

    #[test]
    fn a_commit_that_fails_once_on_disk_is_still_reported_once() {
        recovers_from_a_failed_commit(true);
    }

    /// One commit fails (a sharing violation on Windows, say), before it
    /// reaches the disk or, with `after`, once it has (a reader reload).
    /// Either way the indexer goes on, every torrent is indexed and
    /// reported once, and the counters say what happened.
    fn recovers_from_a_failed_commit(after: bool) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..3 {
            add(&mut writer, &format!("Before.{i}"));
        }
        let index = SearchIndex::open(&tmp.path().join("index")).unwrap();
        let (hook, seen) = collecting();
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let fail = Arc::new(AtomicU64::new(0));
        let config = LiveConfig {
            commit_every: Duration::from_secs(3600),
            commit_docs: 3,
            fail_commits: (!after).then(|| fail.clone()),
            fail_after_commits: after.then(|| fail.clone()),
            ..quick()
        };
        let live =
            LiveIndexer::spawn_with_stats(index.clone(), reader, config, Arc::default(), hook)
                .unwrap();
        let (stats, thread) = (live.stats(), live.thread.as_ref());
        wait_for(&stats, thread, 0, &seen, 3);
        fail.store(1, Ordering::Relaxed);
        for i in 0..3 {
            add(&mut writer, &format!("After.{i}"));
        }
        let got = wait_for(&stats, thread, 1, &seen, 6);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[1].docs_added, got[1].max_torrent_id), (3, 6));
        assert_eq!(index.watermark().unwrap(), 6);
        assert_eq!(index.num_docs(), 6, "added once");
        assert_eq!(stats.indexed.load(Ordering::Relaxed), 6, "counted once");
        assert_eq!(stats.errors.load(Ordering::Relaxed), 1);
        let error = stats.last_error.lock().unwrap().clone();
        let want = if after {
            "corrupt index: test: failed after the commit"
        } else {
            "corrupt index: test: the commit failed"
        };
        assert_eq!(error.as_deref(), Some(want));
        live.stop().unwrap();
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
        let stats = live.stats();
        wait_for(&stats, live.thread.as_ref(), 0, &seen, 1);
        let commits = stats.commits.load(Ordering::Relaxed);
        // New enrichment logs a re-index of the same torrent.
        lodestar_pipeline::enrich::enrich_stored(&mut writer, 0, 100)
            .unwrap()
            .unwrap();
        eventually(
            &stats,
            live.thread.as_ref(),
            0,
            "a commit for the re-index",
            || stats.commits.load(Ordering::Relaxed) != commits,
        );
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
        let got = wait_for(&live.stats(), live.thread.as_ref(), 0, &seen, 5);
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
        let stats = serving.stats();
        wait_for(&stats, None, 0, &seen, 1);
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
        // The rebuild's own progress says whether it's moving.
        let (start, mut last) = (Instant::now(), serving.progress());
        let mut moved = start;
        while let Some(p) = serving.progress() {
            assert_ne!(p.phase, crate::RebuildPhase::Failed, "{p:?}");
            if last.as_ref() != Some(&p) {
                (last, moved) = (Some(p), Instant::now());
            }
            assert!(moved.elapsed() < STALL, "the rebuild stalled: {last:?}");
            assert!(start.elapsed() < HANG, "the rebuild never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        adding.join().unwrap();
        // After it: the new index's indexer reports to the same hook.
        add(&mut writer, "After.Swap");
        let got = wait_for(&stats, None, 0, &seen, 32);
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
