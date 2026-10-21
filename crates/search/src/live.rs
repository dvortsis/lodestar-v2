//! Near-real-time indexing (architecture §8): one thread follows the
//! store through its own read-only connection, adds new torrents and
//! commits every couple of seconds or few thousand documents, so a new
//! torrent is searchable within the 5 s budget (§3).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestar_store::Store;

use crate::index::{Result, SearchIndex};

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

#[derive(Debug)]
pub struct LiveIndexer {
    stop: Arc<AtomicBool>,
    stats: Arc<IndexStats>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl LiveIndexer {
    /// Starts following `store` (a reader connection) into `index`.
    pub fn spawn(index: SearchIndex, mut store: Store, config: LiveConfig) -> Result<LiveIndexer> {
        let mut indexer = index.writer(config.heap_bytes)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(IndexStats::default());
        stats.watermark.store(indexer.last_id(), Ordering::Relaxed);
        let (s, st) = (stop.clone(), stats.clone());
        let thread = std::thread::Builder::new()
            .name("indexer".into())
            .spawn(move || {
                let mut since = Instant::now();
                let commit = |indexer: &mut crate::Indexer| -> Result<()> {
                    indexer.commit()?;
                    st.watermark.store(indexer.last_id(), Ordering::Relaxed);
                    st.commits.fetch_add(1, Ordering::Relaxed);
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    st.last_commit.store(now, Ordering::Relaxed);
                    Ok(())
                };
                while !s.load(Ordering::Relaxed) {
                    let added = match indexer.catch_up(&mut store, config.batch) {
                        Ok(n) => n,
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
                        commit(&mut indexer)?;
                        since = Instant::now();
                    }
                    if added == 0 {
                        std::thread::sleep(config.poll);
                    }
                }
                if indexer.pending() > 0 {
                    commit(&mut indexer)?;
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
