//! Insights, the binary's side (B11a): the recorder that turns index
//! commits into searchable time, the Searchable funnel stage and the
//! coverage counters (architecture §7 "Insights rollups"), and the
//! [`InsightsSource`] the API reads beside `OpsApi`.
//!
//! - **The commit hook only hands off.** A sink on the serving index's
//!   [`CommitHook`] `try_send`s each [`Committed`] into a small channel
//!   and returns; the indexer never waits. A commit dropped because the
//!   channel is full costs nothing: the next one (or the 30 s tick, which
//!   reads the index's watermark) covers its ids, since
//!   [`Store::record_searchable`] takes the range's lower end from the
//!   table.
//! - **One transaction per commit,** on the recorder's own connection,
//!   so the ingest writer never waits on it for long (`BEGIN IMMEDIATE`,
//!   a primary-key range of `torrents`).
//! - **Coverage:** every 30 s the recorder adds the seconds measured to
//!   `metric::COVERAGE` (indexing always while search is on, crawling
//!   while not paused), so a quiet hour reads 0 and a paused one a gap.
//! - **A crash** can lose the last commit's rows (and up to 30 s of
//!   coverage); B11b's `lodestar rollup --repair` recomputes searchable
//!   counts from `torrents` and `index_watermark_hour`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use lodestar_api::insights::{self as api, InsightsSource, OverviewCache};
use lodestar_pipeline::Monitor;
use lodestar_pipeline::metric::{self, coverage};
use lodestar_search::{CommitHook, Committed, IndexStats};
use lodestar_sources::Stage;
use lodestar_store::{Counter, SearchableCodes, Store, StoreConfig, flags};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// The codes `record_searchable` writes under.
pub const CODES: SearchableCodes = SearchableCodes {
    stage: Stage::Searchable as u8,
    newly: metric::NEWLY_SEARCHABLE,
    delay: metric::TIME_TO_SEARCHABLE,
};

/// Coverage is added, and the index's watermark checked, this often.
const TICK: Duration = Duration::from_secs(30);
/// Commits waiting for the recorder; past this they're dropped (and
/// covered by the next one).
const QUEUE: usize = 64;

/// What the API reads beside `OpsApi`.
pub struct Insights {
    /// The writer's last commit, unix seconds (0: none yet).
    last_commit: Box<dyn Fn() -> u64 + Send + Sync>,
    started_at: i64,
    /// Bumped whenever the recorder writes, for the overview cache.
    recorded: AtomicI64,
    /// Imported torrents; -1 until the count at start is in.
    imported: AtomicI64,
    cache: OverviewCache,
}

impl std::fmt::Debug for Insights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Insights")
            .field("started_at", &self.started_at)
            .field("recorded", &self.recorded)
            .field("imported", &self.imported)
            .finish_non_exhaustive()
    }
}

impl Insights {
    pub fn new(monitor: Monitor) -> Insights {
        let started_at = monitor.started_at();
        Insights::with_clock(
            Box::new(move || monitor.writer_stats().last_commit.load(Ordering::Relaxed)),
            started_at,
        )
    }

    fn with_clock(last_commit: Box<dyn Fn() -> u64 + Send + Sync>, started_at: i64) -> Insights {
        Insights {
            last_commit,
            started_at,
            recorded: AtomicI64::new(0),
            imported: AtomicI64::new(-1),
            cache: OverviewCache::default(),
        }
    }

    fn bump(&self) {
        self.recorded.fetch_add(1, Ordering::AcqRel);
    }
}

impl InsightsSource for Insights {
    fn last_flush(&self) -> Option<i64> {
        match (self.last_commit)() {
            0 => None,
            t => Some(t as i64),
        }
    }

    fn started_at(&self) -> i64 {
        self.started_at
    }

    fn last_recorded(&self) -> i64 {
        self.recorded.load(Ordering::Acquire)
    }

    fn imported(&self) -> Option<u64> {
        u64::try_from(self.imported.load(Ordering::Acquire)).ok()
    }

    fn cache(&self) -> &OverviewCache {
        &self.cache
    }
}

/// What the recorder follows.
pub struct Follow {
    pub db: PathBuf,
    pub store: StoreConfig,
    /// The serving index's hook and counters; `None` with search off.
    pub index: Option<(Arc<CommitHook>, Arc<IndexStats>)>,
    /// Whether the crawler is paused now.
    pub paused: Box<dyn Fn() -> bool + Send + Sync>,
}

/// Starts the recorder. It runs until `stop` turns true, recording what
/// the index made searchable once more on the way out.
pub fn spawn(
    insights: Arc<Insights>,
    follow: Follow,
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let (tx, rx) = mpsc::channel(QUEUE);
    if let Some((hook, _)) = &follow.index {
        hook.add(move |c: Committed| {
            let _ = tx.try_send(c);
        });
    }
    tokio::spawn(async move {
        if let Err(e) = run(&insights, follow, rx, stop).await {
            tracing::error!("insights: the recorder stopped: {e}");
        }
    })
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn run(
    insights: &Arc<Insights>,
    follow: Follow,
    mut commits: mpsc::Receiver<Committed>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), BoxError> {
    let (db, config) = (follow.db.clone(), follow.store.clone());
    let mut store = tokio::task::spawn_blocking(move || Store::open(&db, config)).await??;
    let watermark = |f: &Follow| {
        f.index
            .as_ref()
            .map(|(_, stats)| stats.watermark.load(Ordering::Relaxed))
    };
    // Whatever the index committed before the hook was attached.
    if let Some(w) = watermark(&follow) {
        store = record(insights, store, w, crate::unix_now()).await?;
    }
    // The imported total: one scan of `torrents` over what's recorded
    // searchable so far, then what each later range adds (only once the
    // count is in, so nothing is counted twice).
    let counted;
    (store, counted) = blocking(store, |s| s.count_flagged_searchable(flags::IMPORTED)).await?;
    match counted {
        Ok(n) => insights.imported.store(n as i64, Ordering::Release),
        // The total stays null; everything else goes on.
        Err(e) => tracing::warn!("insights: counting imported torrents failed: {e}"),
    }
    // The sealed days' sums, so the first long range is quick too.
    let now = crate::unix_now();
    let warmed;
    (store, warmed) = blocking(store, {
        let insights = insights.clone();
        move |s| api::warm(s, &insights.cache, now)
    })
    .await?;
    if let Err(e) = warmed {
        tracing::warn!("insights: reading the sealed days failed: {e}");
    }
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut covered_to = crate::unix_now();
    loop {
        tokio::select! {
            c = commits.recv() => {
                let Some(c) = c else { break };
                store = record(insights, store, c.max_torrent_id, c.at as i64).await?;
            }
            _ = tick.tick() => {
                let now = crate::unix_now();
                let rows = coverage_rows(covered_to, now, follow.index.is_some(), !(follow.paused)());
                covered_to = now;
                let r;
                (store, r) = blocking(store, move |s| s.add_counters(&rows)).await?;
                match r {
                    Ok(()) => insights.bump(),
                    Err(e) => tracing::warn!("insights: coverage not recorded: {e}"),
                }
                if let Some(w) = watermark(&follow) {
                    store = record(insights, store, w, now).await?;
                }
            }
            _ = stop.changed() => break,
        }
    }
    if let Some(w) = watermark(&follow) {
        record(insights, store, w, crate::unix_now()).await?;
    }
    Ok(())
}

/// Runs `f` on the blocking pool with the store, and gives both back.
async fn blocking<T: Send + 'static>(
    mut store: Store,
    f: impl FnOnce(&mut Store) -> T + Send + 'static,
) -> Result<(Store, T), BoxError> {
    Ok(tokio::task::spawn_blocking(move || {
        let out = f(&mut store);
        (store, out)
    })
    .await?)
}

async fn record(
    insights: &Insights,
    store: Store,
    last_id: i64,
    at: i64,
) -> Result<Store, BoxError> {
    let (store, r) = blocking(store, move |s| s.record_searchable(last_id, at, CODES)).await?;
    match r {
        Ok(Some(r)) => {
            // An index backlog made old cohorts searchable: the sealed
            // days' sums are out of date.
            if r.first_cohort
                .is_some_and(|c| c < api::sealed_hour(at.max(crate::unix_now())))
            {
                insights.cache.forget_days();
            }
            if r.imported > 0 {
                // Only onto a count that's in.
                let _ = insights
                    .imported
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        (n >= 0).then(|| n + r.imported as i64)
                    });
            }
            insights.bump();
        }
        Ok(None) => {}
        // A busy or failed write: the next commit or tick covers the ids.
        Err(e) => tracing::warn!("insights: searchable time not recorded: {e}"),
    }
    Ok(store)
}

/// Coverage seconds for `from..to`, split at hour boundaries.
fn coverage_rows(from: i64, to: i64, indexing: bool, crawling: bool) -> Vec<Counter> {
    let mut out = Vec::new();
    let mut t = from;
    while t < to {
        let hour = t.div_euclid(3600);
        let end = ((hour + 1) * 3600).min(to);
        for (on, dim) in [(indexing, coverage::INDEX), (crawling, coverage::CRAWL)] {
            if on {
                out.push(Counter {
                    hour,
                    metric: metric::COVERAGE,
                    dim,
                    key: 0,
                    value: end - t,
                });
            }
        }
        t = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use lodestar_api::insights::codes;
    use lodestar_sources::Loss;

    use super::*;

    #[test]
    fn coverage_splits_at_hours() {
        let h = 500_000 * 3600;
        let rows = coverage_rows(h - 10, h + 20, true, false);
        let got: Vec<_> = rows.iter().map(|c| (c.hour, c.dim, c.value)).collect();
        assert_eq!(
            got,
            [
                (h / 3600 - 1, coverage::INDEX, 10),
                (h / 3600, coverage::INDEX, 20)
            ]
        );
        assert_eq!(coverage_rows(h, h + 5, true, true).len(), 2);
        assert!(coverage_rows(h, h, true, true).is_empty());
        assert!(coverage_rows(h, h + 5, false, false).is_empty());
    }

    /// The API's code tables are the pipeline's codes.
    #[test]
    fn the_api_names_the_pipelines_codes() {
        use lodestar_pipeline::event;
        let stages = [
            (Stage::Harvested, "harvested"),
            (Stage::PeerFound, "peer_found"),
            (Stage::Fetched, "fetched"),
            (Stage::Stored, "stored"),
            (Stage::Searchable, "searchable"),
            (Stage::Lost, "lost"),
        ];
        assert_eq!(codes::STAGES.len(), stages.len());
        for (stage, name) in stages {
            assert!(codes::STAGES.contains(&(stage as u8, name)), "{stage:?}");
        }
        assert_eq!(
            [
                codes::HARVESTED,
                codes::PEER_FOUND,
                codes::FETCHED,
                codes::STORED,
                codes::SEARCHABLE,
                codes::LOST
            ],
            stages.map(|(s, _)| s as u8)
        );
        let losses = [
            Loss::NoPeers,
            Loss::TimedOut,
            Loss::Refused,
            Loss::EncryptionFailed,
            Loss::Oversize,
            Loss::Invalid,
            Loss::Blocked,
            Loss::Evicted,
            Loss::GaveUp,
            Loss::Unreachable,
        ];
        assert_eq!(codes::LOSSES.len(), losses.len());
        for loss in losses {
            // The same names as the Prometheus labels.
            let label = crate::metrics::LOSSES
                .iter()
                .find(|(l, _)| *l == loss)
                .map(|(_, n)| *n);
            assert_eq!(codes::loss_name(loss as u8), label, "{loss:?}");
        }
        for (kind, name) in [
            (event::START, "restart"),
            (event::STOP, "stopped"),
            (event::PAUSE, "paused"),
            (event::RESUME, "resumed"),
            (event::INTENSITY, "intensity"),
        ] {
            assert_eq!(codes::event_name(kind), Some(name));
        }
        assert_eq!(codes::EVENTS.len(), 5);
        assert_eq!((codes::PAUSE, codes::RESUME), (event::PAUSE, event::RESUME));
        assert_eq!(codes::metric::STORED, metric::STORED);
        assert_eq!(codes::metric::NEWLY_SEARCHABLE, metric::NEWLY_SEARCHABLE);
        assert_eq!(
            codes::metric::TIME_TO_SEARCHABLE,
            metric::TIME_TO_SEARCHABLE
        );
        assert_eq!(codes::metric::COVERAGE, metric::COVERAGE);
        assert_eq!(codes::metric::COVERAGE_INDEX, coverage::INDEX);
        assert_eq!(codes::metric::COVERAGE_CRAWL, coverage::CRAWL);
    }

    /// Commits reach the store, the count of imports arrives, and the
    /// last watermark is recorded on the way out.
    #[tokio::test]
    async fn the_recorder_follows_commits() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        let hook = Arc::new(CommitHook::default());
        let stats = Arc::new(IndexStats::default());
        let insights = Arc::new(Insights::with_clock(Box::new(|| 0), 1));
        let (stop_tx, stop) = watch::channel(false);
        let (tx, rx) = mpsc::channel(QUEUE);
        let follow = Follow {
            db: db.clone(),
            store: StoreConfig::default(),
            index: Some((hook, stats.clone())),
            paused: Box::new(|| false),
        };
        let task = {
            let insights = insights.clone();
            tokio::spawn(async move { run(&insights, follow, rx, stop).await.unwrap() })
        };
        let start = std::time::Instant::now();
        let wait = |done: &dyn Fn() -> bool| {
            assert!(start.elapsed() < Duration::from_secs(10));
            !done()
        };
        while wait(&|| insights.imported().is_some()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(insights.imported(), Some(0));
        assert_eq!(insights.last_flush(), None);
        let before = insights.last_recorded();
        tx.send(Committed {
            docs_added: 1,
            adult_added: 0,
            max_torrent_id: 3,
            at: crate::unix_now() as u64,
        })
        .await
        .unwrap();
        while wait(&|| insights.last_recorded() != before) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A commit the channel dropped: the watermark covers it at stop.
        stats.watermark.store(9, Ordering::Relaxed);
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        let store = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let hour = crate::unix_now() / 3600;
        let w = store.index_watermarks(hour - 1, hour + 1).unwrap();
        assert_eq!(w.last().map(|r| r.1), Some(9));
    }
}
