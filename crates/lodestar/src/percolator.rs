//! The percolator (architecture §8, "Saved searches → alerts"): after
//! each NRT commit, every active saved search runs against only the
//! torrents indexed since its watermark, and its hits become alerts.
//!
//! - **Never slows a commit** (owner §1.6.3): the commit hook's sink only
//!   raises an atomic `through` (the highest searchable id) and wakes the
//!   task. Commits close together coalesce into one run.
//! - **A run:** the searches due up to `through`, most behind first;
//!   [`lodestar_search::percolate`] on the serving index (taken afresh,
//!   so a swap is followed), within a time budget and a per-search range
//!   cap; [`Store::record_percolation`] on the task's own connection;
//!   then the `alerts` SSE events ([`lodestar_api::alerts::announce`]).
//!   What a run didn't reach, the next one does, straight away.
//! - **Restarts lose and repeat nothing:** at start `through` is the
//!   index's watermark, and each search resumes from its stored one. Hits
//!   and the watermark move in one transaction, at-or-below-watermark hits
//!   are skipped, and `alerts` is unique per (search, torrent).
//! - **Bounded:** [`Store::prune_alerts`] keeps each search's newest
//!   [`PercolatorConfig::keep`] alerts, at start and hourly (never per
//!   commit: each prune that deletes scrubs the WAL).
//! - **Logs** name saved-search ids and counts, never a query or a
//!   torrent name (§12.1).
//! - **Fixture mode** never starts it (`fixture::unless_offline`), so the
//!   fixture's alert counts hold.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lodestar_api::AdultVisibility;
use lodestar_api::events::EventHub;
use lodestar_search::{
    CancelToken, CommitHook, Due, Node, PercolateLimits, SearchHandle, SearchIndex, percolate,
};
use lodestar_store::{Alert, SearchHits, Store};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::services::BoxError;

/// How far one run goes, and how many alerts a search keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PercolatorConfig {
    /// A run stops starting searches after this long; the rest run next.
    pub budget: Duration,
    /// Ids one search covers per run, so a catch-up after downtime is
    /// several short runs.
    pub max_range: i64,
    /// Alerts kept per saved search: the newest, by torrent id. A run
    /// records at most this many per search too.
    pub keep: usize,
    /// How often alerts are pruned to `keep`.
    pub prune_every: Duration,
    /// After a failed run, the next waits this long (or for a commit).
    pub retry: Duration,
}

impl Default for PercolatorConfig {
    fn default() -> Self {
        PercolatorConfig {
            budget: Duration::from_secs(2),
            max_range: 100_000,
            keep: 1_000,
            prune_every: Duration::from_secs(3600),
            retry: Duration::from_secs(5),
        }
    }
}

/// What the commit hook hands the task.
#[derive(Debug, Default)]
struct Wake {
    /// The highest torrent id searchable, as reported; only grows.
    through: AtomicI64,
    notify: Notify,
}

/// Adds the percolator's sink to `hook`: it raises `through` and wakes
/// the task, nothing more, so it can't hold up the indexer's thread.
fn listen(hook: &CommitHook) -> Arc<Wake> {
    let wake = Arc::new(Wake::default());
    let w = wake.clone();
    hook.add(move |c| {
        w.through.fetch_max(c.max_torrent_id, Ordering::Relaxed);
        w.notify.notify_one();
    });
    wake
}

/// What one run did, for the log and the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    /// Searches that ran.
    pub searches: usize,
    /// Alerts recorded.
    pub alerts: usize,
    /// `alerts` events published.
    pub events: usize,
    /// Searches skipped because their stored query couldn't be read.
    pub unreadable: usize,
    /// Another run straight away would find more to do.
    pub more: bool,
    /// The alerts themselves (ids and torrent ids only).
    pub created: Vec<Alert>,
}

/// One run up to `through` (see the module docs). `hub` gets the
/// `alerts` events when given.
pub fn run_once(
    index: &SearchIndex,
    store: &mut Store,
    hub: Option<(&EventHub, &AdultVisibility)>,
    through: i64,
    config: &PercolatorConfig,
    now: i64,
) -> Result<Run, BoxError> {
    let mut due = store.percolation_due(through)?;
    due.sort_by_key(|s| (s.watermark, s.id));
    let mut run = Run::default();
    let mut parsed = Vec::with_capacity(due.len());
    for s in &due {
        match serde_json::from_str::<Node>(&s.ast) {
            Ok(node) => parsed.push((s.id, s.watermark, node)),
            Err(_) => {
                // Skipped, not moved past: once its query is saved again
                // it catches up from where it was.
                run.unreadable += 1;
                tracing::warn!(
                    "percolator: saved search {} has a query this build can't read; skipped",
                    s.id
                );
            }
        }
    }
    let due: Vec<Due<'_>> = parsed
        .iter()
        .map(|(id, watermark, node)| Due {
            id: *id,
            watermark: *watermark,
            query: node,
        })
        .collect();
    let cancel = CancelToken::new().with_budget(config.budget);
    let limits = PercolateLimits {
        keep: config.keep,
        max_range: config.max_range,
    };
    let p = percolate(index, &due, through, limits, now, &cancel)?;
    run.searches = p.matched.len();
    run.more = p.more();
    // One transaction per range end: a search capped short of `through`
    // moves only as far as it ran.
    let mut by_end: BTreeMap<i64, Vec<SearchHits<'_>>> = BTreeMap::new();
    for m in &p.matched {
        by_end.entry(m.through).or_default().push(SearchHits {
            saved_search_id: m.id,
            torrent_ids: &m.torrent_ids,
        });
    }
    for (end, hits) in &by_end {
        run.created
            .extend(store.record_percolation(*end, hits, now)?);
    }
    run.alerts = run.created.len();
    if let Some((hub, adult)) = hub
        && !run.created.is_empty()
    {
        match lodestar_api::alerts::announce(hub, store, &run.created, adult) {
            Ok(n) => run.events = n,
            // `announce` logged it; the alerts are recorded either way.
            Err(_) => tracing::warn!("percolator: announcing new alerts failed"),
        }
    }
    if run.searches > 0 || run.unreadable > 0 {
        let mut per: BTreeMap<i64, usize> = BTreeMap::new();
        for a in &run.created {
            *per.entry(a.saved_search_id).or_default() += 1;
        }
        tracing::debug!(
            "percolator: {} searches up to {through}, {} alerts {per:?}, {} unreadable{}",
            run.searches,
            run.alerts,
            run.unreadable,
            if run.more { ", more to do" } else { "" }
        );
    }
    Ok(run)
}

/// Keeps each saved search's newest `keep` alerts.
fn prune(store: &mut Store, keep: usize) {
    match store.prune_alerts(keep) {
        Ok(0) => {}
        Ok(n) => tracing::info!("percolator: pruned {n} old alerts"),
        Err(e) => tracing::warn!("percolator: pruning alerts failed: {e}"),
    }
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the percolator over `search`'s serving index, reporting through
/// `hook` (the serving index's commit hook, which outlives swaps). It
/// stops when `stopped` turns true or its sender is gone, and returns at
/// once without doing anything if it's already stopped (fixture mode).
pub fn spawn(
    search: SearchHandle,
    hook: &CommitHook,
    store: Store,
    hub: Arc<EventHub>,
    adult: AdultVisibility,
    mut stopped: watch::Receiver<bool>,
    config: PercolatorConfig,
) -> JoinHandle<()> {
    if *stopped.borrow() {
        return tokio::spawn(async {});
    }
    // Before the watermark is read, so no commit falls between the two.
    let wake = listen(hook);
    let store = Arc::new(Mutex::new(store));
    tokio::spawn(async move {
        {
            let (search, store, wake) = (search.clone(), store.clone(), wake.clone());
            let keep = config.keep;
            let _ = tokio::task::spawn_blocking(move || {
                match search.index().watermark() {
                    Ok(w) => {
                        wake.through.fetch_max(w, Ordering::Relaxed);
                    }
                    Err(e) => tracing::warn!("percolator: reading the index watermark: {e}"),
                }
                prune(&mut lock(&store), keep);
            })
            .await;
        }
        let mut next_prune = tokio::time::Instant::now() + config.prune_every;
        // Catch up at start: searches may be behind the index.
        let mut more = true;
        loop {
            if !more {
                tokio::select! {
                    biased;
                    _ = stopped.wait_for(|s| *s) => return,
                    () = wake.notify.notified() => {}
                    () = tokio::time::sleep_until(next_prune) => {}
                }
            } else if *stopped.borrow() || stopped.has_changed().is_err() {
                return;
            }
            let prune_now = tokio::time::Instant::now() >= next_prune;
            if prune_now {
                next_prune = tokio::time::Instant::now() + config.prune_every;
            }
            let through = wake.through.load(Ordering::Relaxed);
            let (search, store, hub, adult) =
                (search.clone(), store.clone(), hub.clone(), adult.clone());
            let ran = tokio::task::spawn_blocking(move || {
                let mut store = lock(&store);
                if prune_now {
                    prune(&mut store, config.keep);
                }
                let started = Instant::now();
                let run = run_once(
                    &search.index(),
                    &mut store,
                    Some((&hub, &adult)),
                    through,
                    &config,
                    crate::unix_now(),
                );
                (run, started.elapsed())
            })
            .await;
            more = match ran {
                Ok((Ok(run), took)) => {
                    if run.alerts > 0 {
                        tracing::info!(
                            "percolator: {} new alerts across {} saved searches in {} ms",
                            run.alerts,
                            run.events,
                            took.as_millis()
                        );
                    }
                    run.more
                }
                Ok((Err(e), _)) => {
                    tracing::warn!("percolator: a run failed: {e}; retrying");
                    retry(&mut stopped, &wake, config.retry).await
                }
                Err(e) => {
                    tracing::warn!("percolator: a run panicked: {e}; retrying");
                    retry(&mut stopped, &wake, config.retry).await
                }
            };
        }
    })
}

/// Waits out `delay` (or a commit) after a failure; whether to run again
/// at once. A stop ends the wait and the task notices it next.
async fn retry(stopped: &mut watch::Receiver<bool>, wake: &Wake, delay: Duration) -> bool {
    tokio::select! {
        biased;
        _ = stopped.wait_for(|s| *s) => {}
        () = wake.notify.notified() => {}
        () = tokio::time::sleep(delay) => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use lodestar_search::{LiveConfig, LiveIndexer, Serving, ServingConfig, parse};
    use lodestar_store::{NewTorrent, SavedSearchInput, SourceId, StoreConfig};

    use super::*;

    const NOW: i64 = 1_700_000_100;

    /// A single-file torrent's info dictionary, bencoded.
    fn info(name: &str) -> Vec<u8> {
        let mut raw = b"d6:lengthi2000000000e4:name".to_vec();
        raw.extend(format!("{}:{name}", name.len()).bytes());
        raw.extend(b"12:piece lengthi65536e6:pieces20:");
        raw.extend([0u8; 20]);
        raw.push(b'e');
        raw
    }

    fn add(store: &mut Store, names: &[String]) -> Vec<i64> {
        let raws: Vec<Vec<u8>> = names.iter().map(|n| info(n)).collect();
        let infos: Vec<_> = raws
            .iter()
            .map(|r| lodestar_metainfo::Info::parse(r).unwrap())
            .collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .map(|(info, raw)| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: NOW,
                metadata_at: NOW,
                extra_flags: 0,
                source: SourceId::new(0).unwrap(),
            })
            .collect();
        store
            .insert(&batch)
            .unwrap()
            .into_iter()
            .map(|i| i.id())
            .collect()
    }

    /// Torrent `i` is `Show.{i}`, with `Even` when `i` is even.
    fn names(from: usize, to: usize) -> Vec<String> {
        (from..to)
            .map(|i| {
                if i % 2 == 0 {
                    format!("Show.{i}.Even")
                } else {
                    format!("Show.{i}")
                }
            })
            .collect()
    }

    fn save(store: &mut Store, name: &str, q: &str, watermark: i64) -> i64 {
        let ast = serde_json::to_string(&parse(q).unwrap()).unwrap();
        store
            .create_saved_search(
                &SavedSearchInput {
                    name,
                    query: q,
                    ast: &ast,
                    sort: None,
                    alert: true,
                    rss: false,
                    channels: 0,
                    paused: false,
                },
                watermark,
                0,
            )
            .unwrap()
    }

    struct Setup {
        _dir: tempfile::TempDir,
        db: std::path::PathBuf,
        store: Store,
        index: SearchIndex,
    }

    /// A store with `n` torrents, all indexed.
    fn setup(n: usize) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        add(&mut store, &names(0, n));
        let index = SearchIndex::in_memory().unwrap();
        index_all(&index, &mut store);
        Setup {
            _dir: dir,
            db,
            store,
            index,
        }
    }

    fn index_all(index: &SearchIndex, store: &mut Store) {
        let mut w = index.writer(15 << 20).unwrap();
        while w.catch_up(store, 10_000).unwrap() > 0 {}
        w.commit().unwrap();
    }

    fn alert_ids(store: &Store, id: i64) -> Vec<i64> {
        let mut t: Vec<i64> = store
            .alerts(id, None, usize::MAX)
            .unwrap()
            .iter()
            .map(|a| a.torrent_id)
            .collect();
        t.sort_unstable();
        t
    }

    fn evens(from: i64, to: i64) -> Vec<i64> {
        // Ids start at 1 for torrent 0, so even names have odd ids.
        (from..=to).filter(|id| (id - 1) % 2 == 0).collect()
    }

    fn quick() -> PercolatorConfig {
        PercolatorConfig::default()
    }

    #[test]
    fn only_the_new_range_alerts_and_a_paused_search_waits() {
        let mut s = setup(40);
        let even = save(&mut s.store, "even", "Even", 20);
        let paused = save(&mut s.store, "paused", "Even", 20);
        let mut p = s.store.saved_search(paused).unwrap().unwrap();
        p.paused = true;
        s.store
            .update_saved_search(
                paused,
                &SavedSearchInput {
                    name: &p.name,
                    query: &p.query,
                    ast: &p.ast,
                    sort: None,
                    alert: p.alert,
                    rss: p.rss,
                    channels: 0,
                    paused: true,
                },
                0,
                0,
            )
            .unwrap();
        let run = run_once(&s.index, &mut s.store, None, 30, &quick(), NOW).unwrap();
        assert_eq!((run.searches, run.more), (1, false));
        assert_eq!(alert_ids(&s.store, even), evens(21, 30));
        assert!(alert_ids(&s.store, paused).is_empty());
        assert_eq!(s.store.saved_search(paused).unwrap().unwrap().watermark, 20);
        // Up to the end.
        run_once(&s.index, &mut s.store, None, 40, &quick(), NOW).unwrap();
        assert_eq!(alert_ids(&s.store, even), evens(21, 40));
    }

    #[test]
    fn rerunning_or_a_fresh_task_adds_nothing() {
        let mut s = setup(30);
        let id = save(&mut s.store, "even", "Even", 0);
        let first = run_once(&s.index, &mut s.store, None, 30, &quick(), NOW).unwrap();
        assert_eq!(first.alerts, 15);
        let again = run_once(&s.index, &mut s.store, None, 30, &quick(), NOW).unwrap();
        assert_eq!((again.searches, again.alerts), (0, 0));
        // A crash after the run but before the watermark was seen: the
        // same range recorded again adds nothing (B8's unique key).
        let hits: Vec<i64> = evens(1, 30);
        assert!(
            s.store
                .record_percolation(
                    30,
                    &[SearchHits {
                        saved_search_id: id,
                        torrent_ids: &hits
                    }],
                    NOW
                )
                .unwrap()
                .is_empty()
        );
        // A fresh connection, as after a restart.
        let mut fresh = Store::open(&s.db, StoreConfig::default()).unwrap();
        let run = run_once(&s.index, &mut fresh, None, 30, &quick(), NOW).unwrap();
        assert_eq!(run.alerts, 0);
        assert_eq!(alert_ids(&fresh, id), evens(1, 30));
    }

    #[test]
    fn a_capped_catch_up_loses_nothing() {
        let mut s = setup(250);
        let a = save(&mut s.store, "even", "Even", 0);
        let b = save(&mut s.store, "all", "Show", 100);
        let config = PercolatorConfig {
            max_range: 40,
            ..quick()
        };
        let mut runs = 0;
        loop {
            let run = run_once(&s.index, &mut s.store, None, 250, &config, NOW).unwrap();
            runs += 1;
            if !run.more {
                break;
            }
            assert!(runs < 20);
        }
        assert_eq!(runs, 7, "250 ids at 40 a run, most behind first");
        assert_eq!(alert_ids(&s.store, a), evens(1, 250));
        assert_eq!(alert_ids(&s.store, b), (101..=250).collect::<Vec<_>>());
        for id in [a, b] {
            assert_eq!(s.store.saved_search(id).unwrap().unwrap().watermark, 250);
        }
    }

    #[test]
    fn keep_bounds_a_run_and_pruning_bounds_the_rest() {
        let mut s = setup(60);
        let id = save(&mut s.store, "all", "Show", 0);
        let config = PercolatorConfig {
            keep: 10,
            ..quick()
        };
        run_once(&s.index, &mut s.store, None, 30, &config, NOW).unwrap();
        assert_eq!(alert_ids(&s.store, id), (21..=30).collect::<Vec<_>>());
        run_once(&s.index, &mut s.store, None, 60, &config, NOW).unwrap();
        assert_eq!(alert_ids(&s.store, id).len(), 20);
        prune(&mut s.store, 10);
        assert_eq!(alert_ids(&s.store, id), (51..=60).collect::<Vec<_>>());
    }

    #[test]
    fn the_budget_stops_a_run_and_the_next_resumes_it() {
        let mut s = setup(20);
        let ids: Vec<i64> = (0..5)
            .map(|i| save(&mut s.store, &format!("s{i}"), "Even", 0))
            .collect();
        let spent = PercolatorConfig {
            budget: Duration::ZERO,
            ..quick()
        };
        let run = run_once(&s.index, &mut s.store, None, 20, &spent, NOW).unwrap();
        assert_eq!((run.searches, run.alerts, run.more), (0, 0, true));
        for id in &ids {
            assert_eq!(s.store.saved_search(*id).unwrap().unwrap().watermark, 0);
        }
        let run = run_once(&s.index, &mut s.store, None, 20, &quick(), NOW).unwrap();
        assert_eq!((run.searches, run.alerts, run.more), (5, 50, false));
    }

    #[test]
    fn an_unreadable_query_is_skipped_and_kept_where_it_was() {
        let mut s = setup(10);
        let good = save(&mut s.store, "good", "Even", 0);
        let bad = s
            .store
            .create_saved_search(
                &SavedSearchInput {
                    name: "bad",
                    query: "x",
                    ast: r#"{"op":"no such op"}"#,
                    sort: None,
                    alert: true,
                    rss: false,
                    channels: 0,
                    paused: false,
                },
                0,
                0,
            )
            .unwrap();
        let run = run_once(&s.index, &mut s.store, None, 10, &quick(), NOW).unwrap();
        assert_eq!((run.searches, run.unreadable, run.more), (1, 1, false));
        assert_eq!(alert_ids(&s.store, good).len(), 5);
        assert_eq!(s.store.saved_search(bad).unwrap().unwrap().watermark, 0);
    }

    /// A rebuild swaps a new index in partway through a capped catch-up:
    /// every match is recorded once.
    #[test]
    fn a_swap_midway_neither_loses_nor_repeats() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let root = tmp.path().join("index");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        add(&mut store, &names(0, 300));
        let old = lodestar_search::IndexVersion::CURRENT;
        {
            let index = SearchIndex::open(&root.join(old.dir_name())).unwrap();
            index_all(&index, &mut store);
        }
        let new = lodestar_search::IndexVersion {
            tokenizer: old.tokenizer + 1,
            ..old
        };
        let hold = Arc::new(AtomicBool::new(true));
        let serving = Arc::new(
            Serving::open_or_rebuild(
                &root,
                &db,
                ServingConfig {
                    live: LiveConfig {
                        heap_bytes: 15 << 20,
                        poll: Duration::from_millis(20),
                        commit_every: Duration::from_millis(50),
                        ..LiveConfig::default()
                    },
                    version: new,
                    hold_before_finish: Some(hold.clone()),
                    ..ServingConfig::default()
                },
            )
            .unwrap(),
        );
        let handle = SearchHandle::serving(serving.clone());
        let id = save(&mut store, "even", "Even", 0);
        let config = PercolatorConfig {
            max_range: 70,
            ..quick()
        };
        let before = handle.index();
        run_once(&handle.index(), &mut store, None, 300, &config, NOW).unwrap();
        run_once(&handle.index(), &mut store, None, 300, &config, NOW).unwrap();
        assert_eq!(store.saved_search(id).unwrap().unwrap().watermark, 140);
        // Let the rebuild finish and swap.
        hold.store(false, Ordering::Relaxed);
        let start = Instant::now();
        while serving.rebuilding() || Arc::ptr_eq(&before, &handle.index()) {
            assert!(start.elapsed() < Duration::from_secs(60), "the swap");
            std::thread::sleep(Duration::from_millis(10));
        }
        while run_once(&handle.index(), &mut store, None, 300, &config, NOW)
            .unwrap()
            .more
        {}
        assert_eq!(alert_ids(&store, id), evens(1, 300));
        drop((before, handle));
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }

    /// The sink only raises an atomic and wakes: with the task never
    /// running at all, the indexer commits everything as fast as without.
    #[test]
    fn a_stalled_task_never_delays_a_commit() {
        let mut s = setup(0);
        let hook = Arc::new(CommitHook::default());
        let wake = listen(&hook);
        // Nobody ever waits on `wake`, and the task's store is locked.
        let stalled = Mutex::new(Store::open(&s.db, StoreConfig::default()).unwrap());
        let _held = lock(&stalled);
        let reader = Store::open_reader(&s.db, StoreConfig::default()).unwrap();
        let live = LiveIndexer::spawn_with_stats(
            s.index.clone(),
            reader,
            LiveConfig {
                heap_bytes: 15 << 20,
                poll: Duration::from_millis(10),
                commit_every: Duration::from_millis(20),
                commit_docs: 50,
                batch: 50,
            },
            Arc::default(),
            hook.clone(),
        )
        .unwrap();
        let stats = live.stats();
        let start = Instant::now();
        for chunk in 0..10 {
            add(&mut s.store, &names(chunk * 100, chunk * 100 + 100));
        }
        while stats.watermark.load(Ordering::Relaxed) < 1000 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "the indexer stalled"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stats.commits.load(Ordering::Relaxed) >= 2);
        live.stop().unwrap();
        assert_eq!(wake.through.load(Ordering::Relaxed), 1000);
    }

    /// The log names saved-search ids and counts, never a query or a
    /// torrent name.
    #[test]
    fn logs_carry_no_query_or_name() {
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Buf {
                self.clone()
            }
        }

        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(buf.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut s = setup(0);
            add(&mut s.store, &["Secret.Torrent.Name".to_string()]);
            index_all(&s.index, &mut s.store);
            save(&mut s.store, "mine", "Secret", 0);
            s.store
                .create_saved_search(
                    &SavedSearchInput {
                        name: "broken",
                        query: "PrivateQueryText",
                        ast: r#"{"op":"PrivateQueryText"}"#,
                        sort: None,
                        alert: true,
                        rss: false,
                        channels: 0,
                        paused: false,
                    },
                    0,
                    0,
                )
                .unwrap();
            let run = run_once(&s.index, &mut s.store, None, 1, &quick(), NOW).unwrap();
            assert_eq!((run.alerts, run.unreadable), (1, 1));
            prune(&mut s.store, 0);
        });
        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("saved search 2"), "{log}");
        assert!(log.contains("pruned 1"), "{log}");
        for secret in ["Secret", "PrivateQueryText", "mine", "broken"] {
            assert!(!log.contains(secret), "{secret} in {log}");
        }
    }

    /// The task over a live index: a commit becomes alerts and an event
    /// within the 5 s target (§3), here with 200 active searches; and
    /// in fixture mode it never starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_turns_commits_into_alerts_quickly() {
        use lodestar_api::events::{EventSources, EventsConfig};

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let root = tmp.path().join("index");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        add(&mut store, &names(0, 100));
        let serving = Arc::new(
            Serving::open_or_rebuild(
                &root,
                &db,
                ServingConfig {
                    // The real commit cadence: every 2 s.
                    live: LiveConfig {
                        heap_bytes: 15 << 20,
                        ..LiveConfig::default()
                    },
                    ..ServingConfig::default()
                },
            )
            .unwrap(),
        );
        let handle = SearchHandle::serving(serving.clone());
        // Caught up: the index has the first 100.
        while handle.index().watermark().unwrap() < 100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mark = handle.index().watermark().unwrap();
        let mut searches = Vec::new();
        for i in 0..200 {
            let q = if i == 0 { "Even" } else { "Show" };
            searches.push(save(&mut store, &format!("s{i}"), q, mark));
        }
        let hub = EventHub::start(EventsConfig::default(), EventSources::default());

        // Fixture mode hands it a receiver already stopped (`fixture::gate`):
        // nothing is added to the hook, read or run.
        let stopped = watch::channel(true).1;
        let noop = spawn(
            handle.clone(),
            serving.commit_hook(),
            Store::open(&db, StoreConfig::default()).unwrap(),
            hub.clone(),
            AdultVisibility::default(),
            stopped,
            quick(),
        );
        noop.await.unwrap();
        assert!(format!("{:?}", serving.commit_hook()).contains("extra: 0"));

        let (stop, stopped) = watch::channel(false);
        let task = spawn(
            handle.clone(),
            serving.commit_hook(),
            Store::open(&db, StoreConfig::default()).unwrap(),
            hub.clone(),
            AdultVisibility::default(),
            stopped,
            quick(),
        );
        let start = Instant::now();
        add(&mut store, &names(100, 110));
        let first = searches[0];
        loop {
            if alert_ids(&store, first).len() == 5 {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(30));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let took = start.elapsed();
        // Store-to-alert, including up to one 2 s commit interval.
        eprintln!("commit-to-alert at 200 searches: {took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
        // Every search got there; the last ones recorded too.
        let last = *searches.last().unwrap();
        while alert_ids(&store, last).len() < 10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        task.await.unwrap();
        hub.close();
        drop(handle);
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }
}
