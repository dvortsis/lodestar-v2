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
//! - **Live queries** (`POST /search/live`, B-search-live) join each run
//!   once its saved searches are recorded and announced, as count-only
//!   entries on what's left of the same budget, with the same range cap
//!   ([`lodestar_api::search_live::percolate_live`]). So they never
//!   delay an alert; when the budget runs out first they're `more`, and
//!   the next run (straight away) takes them.
//! - **Promoted torrents** (SRC-2b, `docs/decisions/2026-11-02-SRC-2b.md`)
//!   keep an id below the watermarks, so the range never reaches them.
//!   `promote` queues each one durably ([`Store::promoted_due`]); after
//!   the saved searches, a run takes up to [`PROMOTED_BATCH`] of them that
//!   the serving index has applied, matches each against the searches
//!   that existed when it was promoted and have passed its id, and
//!   records the hits and drops the rows in one transaction
//!   ([`Store::record_promoted`]). A merge adds no torrent, so it's not
//!   queued.
//! - **Logs** name saved-search ids and counts, never a query or a
//!   torrent name (§12.1).
//! - **Fixture mode** never starts it (`fixture::unless_offline`), so the
//!   fixture's alert counts hold.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lodestar_api::AdultVisibility;
use lodestar_api::events::EventHub;
use lodestar_search::{
    CancelToken, CommitHook, Due, Node, PercolateLimits, SearchHandle, SearchIndex, percolate,
};
use lodestar_store::{Alert, PromotedDue, SearchHits, Store};
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
    /// Alerts kept per saved search: the newest recorded (a promoted
    /// torrent's alert is new though its id is old, SRC-2b). A run
    /// records at most this many per search too.
    pub keep: usize,
    /// How often alerts are pruned to `keep`.
    pub prune_every: Duration,
    /// After a failed run, the next waits this long (or for a commit).
    pub retry: Duration,
    /// An idle task looks at the promoted queue this often: an index
    /// commit that only replays `reindex_log` (a promotion) doesn't wake
    /// it, since the commit hook reports new torrents only (SRC-2b).
    pub promoted_poll: Duration,
}

impl Default for PercolatorConfig {
    fn default() -> Self {
        PercolatorConfig {
            budget: Duration::from_secs(2),
            max_range: 100_000,
            keep: 1_000,
            prune_every: Duration::from_secs(3600),
            retry: Duration::from_secs(5),
            promoted_poll: Duration::from_secs(10),
        }
    }
}

/// Queued promotions one run takes at most: each is one tiny search per
/// saved search, so a batch stays well inside the budget.
pub const PROMOTED_BATCH: usize = 256;

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
    /// Live queries that ran, after the saved searches.
    pub live: usize,
    /// Promoted torrents percolated, and what that took.
    pub promoted: usize,
    pub promoted_took: Duration,
}

/// What the task remembers between runs.
#[derive(Debug, Default)]
pub struct Seen {
    /// Saved searches already logged as unreadable, so each is logged
    /// once per process rather than every run.
    unreadable: HashSet<i64>,
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
    seen: &mut Seen,
) -> Result<Run, BoxError> {
    let mut due = store.percolation_due(through)?;
    due.sort_by_key(|s| (s.watermark, s.id));
    let mut run = Run::default();
    let mut parsed = Vec::with_capacity(due.len());
    for s in &due {
        match serde_json::from_str::<Node>(&s.ast) {
            Ok(node) => {
                seen.unreadable.remove(&s.id);
                parsed.push((s.id, s.watermark, alert_query(node)));
            }
            Err(_) => {
                // Skipped, not moved past: once its query is saved again
                // it catches up from where it was.
                run.unreadable += 1;
                if seen.unreadable.insert(s.id) {
                    lodestar_log::log_event!(
                        crate::logging::events::ALERTS_SEARCH_UNREADABLE,
                        saved_search = lodestar_log::Count(s.id.unsigned_abs())
                    );
                }
            }
        }
    }
    // The AST each search ran with: one paused or edited meanwhile is
    // left alone when the run is recorded.
    let asts: HashMap<i64, &str> = due.iter().map(|s| (s.id, s.ast.as_str())).collect();
    let due: Vec<Due<'_>> = parsed
        .iter()
        .map(|(id, watermark, node)| forward_due(*id, *watermark, node))
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
    let mut by_end: BTreeMap<i64, Vec<(SearchHits<'_>, &str)>> = BTreeMap::new();
    for m in &p.matched {
        let ast = asts.get(&m.id).copied().unwrap_or_default();
        by_end.entry(m.through).or_default().push((
            SearchHits {
                saved_search_id: m.id,
                torrent_ids: &m.torrent_ids,
            },
            ast,
        ));
    }
    let mut failed: Option<BoxError> = None;
    for (end, hits) in &by_end {
        match store.record_percolation_unchanged(*end, hits, now) {
            Ok(created) => run.created.extend(created),
            Err(e) => {
                failed = Some(e.into());
                break;
            }
        }
    }
    if failed.is_none() {
        let started = Instant::now();
        let promoted = run_promoted(index, store, &cancel, now, &mut run);
        run.promoted_took = started.elapsed();
        failed = promoted.err();
    }
    run.alerts = run.created.len();
    // Announced even when a later transaction failed: the earlier ones
    // are committed and already show in the list and the badge.
    if let Some((hub, adult)) = hub
        && !run.created.is_empty()
    {
        match lodestar_api::alerts::announce(hub, store, &run.created, adult) {
            Ok(n) => run.events = n,
            // `internal` logged the store error behind it. The alerts
            // are recorded and show in the list and the badge; only the
            // live event is lost.
            Err(_) => lodestar_log::log_event!(crate::logging::events::ALERTS_ANNOUNCE_FAILED),
        }
    }
    // Live queries, after the alerts are out, on what's left of the
    // budget. They keep no store state, so a failure here only costs
    // their counts this run.
    if let Some((hub, _)) = hub
        && let Ok(live) = lodestar_api::search_live::percolate_live(
            index,
            hub.live(),
            through,
            limits,
            now,
            &cancel,
            run.searches > 0,
        )
    {
        run.live = live.ran;
        run.more |= live.more;
    }
    if let Some(e) = failed {
        return Err(e);
    }
    if run.searches > 0 || run.unreadable > 0 || run.promoted > 0 {
        lodestar_log::log_event!(
            crate::logging::events::ALERTS_RUN,
            searches = lodestar_log::Count(run.searches as u64),
            through = lodestar_log::Count(through.unsigned_abs()),
            alerts = lodestar_log::Count(run.alerts as u64),
            unreadable = lodestar_log::Count(run.unreadable as u64),
            more = run.more,
            promoted = lodestar_log::Count(run.promoted as u64),
            promoted_ms = lodestar_log::Count(run.promoted_took.as_millis() as u64)
        );
    }
    Ok(run)
}

/// Percolates up to [`PROMOTED_BATCH`] queued promotions (see the module
/// docs) on what's left of `cancel`'s budget, adding their alerts to
/// `run`. Rows the budget didn't reach stay queued and `run.more` is set.
fn run_promoted(
    index: &SearchIndex,
    store: &mut Store,
    cancel: &CancelToken,
    now: i64,
    run: &mut Run,
) -> Result<(), BoxError> {
    // The entry first, then the reader: a torrent the entry names is in
    // what the refreshed reader sees.
    let applied = index.reindex_watermark()?;
    let rows = store.promoted_due(applied, PROMOTED_BATCH)?;
    // An index rebuilt from empty commits the log's head before its ids
    // reach a promoted one: the row waits until both indexes' ids do, or
    // it would be matched without its document (or its files, for a
    // `file:(…)` clause) and lost.
    let indexed = index.watermark()?.min(index.files_watermark()?);
    let Some(last) = rows.last().map(|r| r.seq) else {
        return Ok(());
    };
    index.refresh()?;
    // Every active search; one with an unreadable query gets nothing (the
    // forward run logs it).
    let searches = store.percolation_due(i64::MAX)?;
    let parsed: Vec<(i64, i64, &str, Node)> = searches
        .iter()
        .filter_map(|s| {
            let node = serde_json::from_str::<Node>(&s.ast).ok()?;
            Some((s.id, s.watermark, s.ast.as_str(), alert_query(node)))
        })
        .collect();
    let limits = PercolateLimits {
        keep: 1,
        max_range: 1,
    };
    let mut hits: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    // The run's first row always finishes, whatever the forward run left
    // of the budget, so the queue moves however busy the runs are.
    let first = CancelToken::new();
    let mut done = None;
    let mut stopped = false;
    for (i, row) in rows.iter().enumerate() {
        // A row the index hasn't reached waits for a later commit (or the
        // idle poll): not `more`, which would spin until it's indexed.
        if row.torrent_id > indexed {
            break;
        }
        let token = if i == 0 { &first } else { cancel };
        if !matched_promoted(index, &parsed, row, limits, now, token, &mut hits)? {
            stopped = true;
            break;
        }
        done = Some(row.seq);
        run.promoted += 1;
    }
    run.more |= stopped || (done == Some(last) && rows.len() == PROMOTED_BATCH);
    let Some(done) = done else {
        return Ok(());
    };
    let asts: HashMap<i64, &str> = parsed.iter().map(|p| (p.0, p.2)).collect();
    let results: Vec<(SearchHits<'_>, &str)> = hits
        .iter()
        .map(|(id, torrent_ids)| {
            (
                SearchHits {
                    saved_search_id: *id,
                    torrent_ids,
                },
                asts.get(id).copied().unwrap_or_default(),
            )
        })
        .collect();
    run.created
        .extend(store.record_promoted(done, &results, now)?);
    Ok(())
}

/// Matches one promoted torrent against the searches it's due for: those
/// that existed when it was promoted and whose watermark has passed its
/// id (a search below it meets it in its own range). Whether the budget
/// let them all run; a search that fails in the index is passed over, so
/// one bad query can't hold the queue.
fn matched_promoted(
    index: &SearchIndex,
    searches: &[(i64, i64, &str, Node)],
    row: &PromotedDue,
    limits: PercolateLimits,
    now: i64,
    cancel: &CancelToken,
    hits: &mut BTreeMap<i64, Vec<i64>>,
) -> Result<bool, BoxError> {
    let due: Vec<Due<'_>> = searches
        .iter()
        .filter(|(id, watermark, ..)| *id <= row.searches_through && *watermark >= row.torrent_id)
        .map(|(id, _, _, node)| promoted_due(*id, row.torrent_id, node))
        .collect();
    if due.is_empty() || row.torrent_id <= 0 {
        return Ok(true);
    }
    let p = percolate(index, &due, row.torrent_id, limits, now, cancel)?;
    if p.stopped {
        return Ok(false);
    }
    for m in p.matched {
        if m.torrent_ids.contains(&row.torrent_id) {
            hits.entry(m.id).or_default().push(row.torrent_id);
        }
    }
    Ok(true)
}

/// A saved search's forward run from its watermark. It's held while a
/// `sig:<info hash>` can't resolve every reference (GS-0): its watermark
/// would otherwise move past clones that then never alert.
fn forward_due(id: i64, watermark: i64, query: &Node) -> Due<'_> {
    Due {
        id,
        watermark,
        query,
        utc_offset: 0,
        count_only: false,
        stems: false,
        stoppable: false,
        hold_unresolved: true,
    }
}

/// A saved search's one-off pass over a promoted torrent. Not held: its
/// promotion row is consumed either way, so it runs and alerts on the
/// references already backfilled (the rest is X-percolate-promoted-hold).
fn promoted_due(id: i64, torrent_id: i64, query: &Node) -> Due<'_> {
    Due {
        id,
        watermark: torrent_id - 1,
        query,
        utc_offset: 0,
        count_only: false,
        stems: false,
        stoppable: false,
        hold_unresolved: false,
    }
}

/// Keeps each saved search's newest `keep` alerts.
fn prune(store: &mut Store, keep: usize) {
    match store.prune_alerts(keep) {
        Ok(0) => {}
        Ok(n) => lodestar_log::log_event!(
            crate::logging::events::ALERTS_PRUNED,
            alerts = lodestar_log::Count(n as u64)
        ),
        Err(e) => lodestar_log::log_event!(
            crate::logging::events::ALERTS_PRUNE_FAILED,
            error = lodestar_log::Error::new(&e)
        ),
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
                    Err(e) => lodestar_log::log_event!(
                        crate::logging::events::ALERTS_WATERMARK_FAILED,
                        error = lodestar_log::Error::new(&e)
                    ),
                }
                prune(&mut lock(&store), keep);
            })
            .await;
        }
        let mut next_prune = tokio::time::Instant::now() + config.prune_every;
        let seen = Arc::new(Mutex::new(Seen::default()));
        // Catch up at start: searches may be behind the index.
        let mut more = true;
        loop {
            if !more {
                tokio::select! {
                    biased;
                    _ = stopped.wait_for(|s| *s) => return,
                    () = wake.notify.notified() => {}
                    () = tokio::time::sleep_until(next_prune) => {}
                    () = tokio::time::sleep(config.promoted_poll) => {}
                }
            } else if *stopped.borrow() || stopped.has_changed().is_err() {
                return;
            }
            let prune_now = tokio::time::Instant::now() >= next_prune;
            if prune_now {
                next_prune = tokio::time::Instant::now() + config.prune_every;
            }
            let through = wake.through.load(Ordering::Relaxed);
            let (search, store, hub, adult, seen) = (
                search.clone(),
                store.clone(),
                hub.clone(),
                adult.clone(),
                seen.clone(),
            );
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
                    &mut seen.lock().unwrap_or_else(PoisonError::into_inner),
                );
                (run, started.elapsed())
            })
            .await;
            more = match ran {
                Ok((Ok(run), took)) => {
                    if run.alerts > 0 {
                        let mut searches: Vec<i64> =
                            run.created.iter().map(|a| a.saved_search_id).collect();
                        searches.dedup();
                        searches.sort_unstable();
                        searches.dedup();
                        lodestar_log::log_event!(
                            crate::logging::events::ALERTS_FOUND,
                            alerts = lodestar_log::Count(run.alerts as u64),
                            searches = lodestar_log::Count(searches.len() as u64),
                            events = lodestar_log::Count(run.events as u64),
                            took_ms = lodestar_log::Count(took.as_millis() as u64)
                        );
                    }
                    run.more
                }
                Ok((Err(e), _)) => {
                    lodestar_log::log_event!(
                        crate::logging::events::ALERTS_RUN_FAILED,
                        panicked = false,
                        error = lodestar_log::Error::new(&e)
                    );
                    retry(&mut stopped, &wake, config.retry).await
                }
                Err(e) => {
                    lodestar_log::log_event!(
                        crate::logging::events::ALERTS_RUN_FAILED,
                        panicked = true,
                        error = lodestar_log::Error::new(&e)
                    );
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

/// A saved search's query as alerts run it: unverified items stay out
/// unless it names `verified:` itself, as in a search (§18.6, SRC-3).
fn alert_query(node: Node) -> Node {
    node.without_unverified()
}

#[cfg(test)]
mod tests {
    /// GS-0: the forward run waits for the `payload_sig` backfill, and a
    /// promoted torrent's pass doesn't (each flip brings back a lost
    /// alert, see `docs/decisions/2026-11-12-GS-0.md`).
    #[test]
    fn forward_runs_hold_and_promoted_passes_dont() {
        let q = Node::All;
        let f = super::forward_due(3, 40, &q);
        assert!(f.hold_unresolved);
        assert_eq!((f.id, f.watermark, f.count_only), (3, 40, false));
        let p = super::promoted_due(3, 41, &q);
        assert!(!p.hold_unresolved);
        assert_eq!((p.id, p.watermark, p.count_only), (3, 40, false));
    }

    #[test]
    fn alerts_leave_unverified_items_out_unless_asked() {
        use lodestar_search::parse;
        assert_eq!(
            super::alert_query(parse("movie").unwrap()),
            parse("movie verified:yes").unwrap()
        );
        assert_eq!(
            super::alert_query(Node::All),
            parse("verified:yes").unwrap()
        );
        let asked = parse("movie verified:yes,no").unwrap();
        assert_eq!(super::alert_query(asked.clone()), asked);
    }

    use std::sync::atomic::AtomicBool;

    use lodestar_search::{LiveConfig, LiveIndexer, Serving, ServingConfig, parse};
    use lodestar_store::claims::Claims;
    use lodestar_store::unverified::{Network, NewUnverified, Promoted};
    use lodestar_store::{
        NewTorrent, PreparedTorrent, SavedSearchInput, SearchableCodes, SourceId, StoreConfig,
        flags,
    };

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
        let run = run_once(
            &s.index,
            &mut s.store,
            None,
            30,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!((run.searches, run.more), (1, false));
        assert_eq!(alert_ids(&s.store, even), evens(21, 30));
        assert!(alert_ids(&s.store, paused).is_empty());
        assert_eq!(s.store.saved_search(paused).unwrap().unwrap().watermark, 20);
        // Up to the end.
        run_once(
            &s.index,
            &mut s.store,
            None,
            40,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(alert_ids(&s.store, even), evens(21, 40));
    }

    #[test]
    fn rerunning_or_a_fresh_task_adds_nothing() {
        let mut s = setup(30);
        let id = save(&mut s.store, "even", "Even", 0);
        let first = run_once(
            &s.index,
            &mut s.store,
            None,
            30,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(first.alerts, 15);
        let again = run_once(
            &s.index,
            &mut s.store,
            None,
            30,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
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
        let run = run_once(
            &s.index,
            &mut fresh,
            None,
            30,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
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
            let run = run_once(
                &s.index,
                &mut s.store,
                None,
                250,
                &config,
                NOW,
                &mut Seen::default(),
            )
            .unwrap();
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
        run_once(
            &s.index,
            &mut s.store,
            None,
            30,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(alert_ids(&s.store, id), (21..=30).collect::<Vec<_>>());
        run_once(
            &s.index,
            &mut s.store,
            None,
            60,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
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
        let run = run_once(
            &s.index,
            &mut s.store,
            None,
            20,
            &spent,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!((run.searches, run.alerts, run.more), (0, 0, true));
        for id in &ids {
            assert_eq!(s.store.saved_search(*id).unwrap().unwrap().watermark, 0);
        }
        let run = run_once(
            &s.index,
            &mut s.store,
            None,
            20,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
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
        let run = run_once(
            &s.index,
            &mut s.store,
            None,
            10,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
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
        run_once(
            &handle.index(),
            &mut store,
            None,
            300,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        run_once(
            &handle.index(),
            &mut store,
            None,
            300,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(store.saved_search(id).unwrap().unwrap().watermark, 140);
        // Let the rebuild finish and swap.
        hold.store(false, Ordering::Relaxed);
        let start = Instant::now();
        while serving.rebuilding() || Arc::ptr_eq(&before, &handle.index()) {
            assert!(start.elapsed() < Duration::from_secs(60), "the swap");
            std::thread::sleep(Duration::from_millis(10));
        }
        while run_once(
            &handle.index(),
            &mut store,
            None,
            300,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap()
        .more
        {}
        assert_eq!(alert_ids(&store, id), evens(1, 300));
        drop((before, handle));
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }

    const CODES: SearchableCodes = SearchableCodes {
        stage: 5,
        newly: 10,
        delay: 11,
    };

    /// A feed's unverified item for `name`: its id, below every torrent
    /// stored after it.
    fn unverified(store: &mut Store, name: &str) -> i64 {
        let feed = store.register_source("feed", "1", NOW).unwrap();
        let raw = info(name);
        let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
        let hash = store.preparer().prepare(&parsed, None).unwrap().info_hash.0;
        let claims = Claims {
            name: Some(name.into()),
            ..Claims::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &hash,
                source: feed,
                private_source: false,
                claims: Some(&claims),
                at: NOW,
            }])
            .unwrap();
        store.unverified(Network::Bt, &hash).unwrap().unwrap().id
    }

    /// Its verified metadata arrives: `name`'s info dict promotes item `id`.
    fn promote(store: &mut Store, id: i64, name: &str, extra_flags: i64) -> Promoted {
        let raw = info(name);
        let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
        let prepared = store.preparer().prepare(&parsed, None).unwrap();
        store
            .promote(
                id,
                &PreparedTorrent {
                    torrent: &prepared,
                    discovered_at: NOW,
                    metadata_at: NOW,
                    extra_flags,
                    source: SourceId::new(0).unwrap(),
                    verified: true,
                },
                CODES,
                NOW,
            )
            .unwrap()
    }

    /// An unverified item `Promo.Even` below 20 indexed torrents, and a
    /// search for `Promo` (and one for `Even`) whose watermark has passed
    /// it: the forward run will never reach the item's id.
    fn promo_setup() -> (Setup, i64, i64, i64) {
        let mut s = setup(0);
        let item = unverified(&mut s.store, "Promo.Even");
        add(&mut s.store, &names(0, 20));
        index_all(&s.index, &mut s.store);
        let w = s.index.watermark().unwrap();
        assert!(w > item);
        let promo = save(&mut s.store, "promo", "Promo", w);
        let even = save(&mut s.store, "even", "Even", w);
        (s, item, promo, even)
    }

    fn run(s: &mut Setup, through: i64) -> Run {
        run_once(
            &s.index,
            &mut s.store,
            None,
            through,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_promoted_id_below_the_watermark_alerts_once() {
        let (mut s, item, promo, even) = promo_setup();
        let w = s.index.watermark().unwrap();
        assert_eq!(
            promote(&mut s.store, item, "Promo.Even", 0),
            Promoted::Kept(item)
        );
        // Not before the index has applied the promotion.
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.alerts), (0, 0));
        assert!(alert_ids(&s.store, promo).is_empty());
        index_all(&s.index, &mut s.store);
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.alerts, r.more), (1, 2, false));
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
        assert_eq!(alert_ids(&s.store, even), vec![item]);
        assert!(s.store.promoted_due(i64::MAX, 10).unwrap().is_empty());
        // Nothing twice.
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.alerts), (0, 0));
        assert_eq!(s.store.saved_search(promo).unwrap().unwrap().watermark, w);
    }

    #[test]
    fn a_crash_between_hit_and_cursor_replays_without_duplicates() {
        let (mut s, item, promo, even) = promo_setup();
        let w = s.index.watermark().unwrap();
        promote(&mut s.store, item, "Promo.Even", 0);
        index_all(&s.index, &mut s.store);
        let due = s.store.promoted_due(i64::MAX, 10).unwrap();
        assert_eq!(due.len(), 1);
        // The state a crash would leave if the hits and the row weren't one
        // transaction (they are): `promo`'s hit committed, its queue row
        // not. The row stays, and the replay matches the same id again.
        let ast = s.store.saved_search(promo).unwrap().unwrap().ast;
        let created = s
            .store
            .record_promoted(
                due[0].seq - 1,
                &[(
                    SearchHits {
                        saved_search_id: promo,
                        torrent_ids: &[item],
                    },
                    ast.as_str(),
                )],
                NOW,
            )
            .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(s.store.promoted_due(i64::MAX, 10).unwrap(), due);
        // A restart: the row is read from the file again.
        s.store = Store::open(&s.db, StoreConfig::default()).unwrap();
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.alerts), (1, 1), "only `even` is new");
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
        assert_eq!(alert_ids(&s.store, even), vec![item]);
        assert!(s.store.promoted_due(i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn restart_continuity_and_no_backfill() {
        let (mut s, item, promo, _) = promo_setup();
        let w = s.index.watermark().unwrap();
        promote(&mut s.store, item, "Promo.Even", 0);
        // Created after the promotion, at the index's watermark: like any
        // new search, it doesn't alert on what came before it.
        let later = save(&mut s.store, "later", "Promo", w);
        index_all(&s.index, &mut s.store);
        // Restart before the percolator ran.
        s.store = Store::open(&s.db, StoreConfig::default()).unwrap();
        run(&mut s, w);
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
        assert!(alert_ids(&s.store, later).is_empty());
    }

    #[test]
    fn a_merge_held_or_hidden_promotion_does_not_alert() {
        let (mut s, item, promo, even) = promo_setup();
        let w = s.index.watermark().unwrap();
        // Merged: the crawler stored the hash before its item was fetched.
        let merged = unverified(&mut s.store, "Promo.Merged.Even");
        let stored = add(&mut s.store, &["Promo.Merged.Even".to_string()])[0];
        assert_eq!(
            promote(&mut s.store, merged, "Promo.Merged.Even", 0),
            Promoted::Merged(stored)
        );
        // Held: a CS-1 hash arrived before its metadata.
        // Held: a hybrid whose other hash is CS-1 (holding the item's own
        // key would take the item with it, V14).
        let held = unverified(&mut s.store, "Promo.Held.Even");
        let raw = info("Promo.Held.Even");
        let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
        let mut hybrid = s.store.preparer().prepare(&parsed, None).unwrap();
        hybrid.info_hash_v2 = Some(lodestar_metainfo::InfoHashV2([0xcd; 32]));
        s.store.add_cs_hashes(&[[0xcd; 20]], 0, NOW).unwrap();
        let got = s
            .store
            .promote(
                held,
                &PreparedTorrent {
                    torrent: &hybrid,
                    discovered_at: NOW,
                    metadata_at: NOW,
                    extra_flags: 0,
                    source: SourceId::new(0).unwrap(),
                    verified: true,
                },
                CODES,
                NOW,
            )
            .unwrap();
        assert_eq!(got, Promoted::Held);
        // Hidden by a filter rule.
        assert_eq!(
            promote(&mut s.store, item, "Promo.Even", flags::HIDDEN),
            Promoted::Kept(item)
        );
        assert_eq!(s.store.promoted_due(i64::MAX, 10).unwrap().len(), 1);
        index_all(&s.index, &mut s.store);
        let through = s.index.watermark().unwrap();
        run(&mut s, through);
        // The merged torrent alerts once, in its own range, as any new
        // torrent; the promotion that found it adds nothing.
        assert_eq!(alert_ids(&s.store, promo), vec![item, stored]);
        assert!(stored > w);
        // A hidden torrent's alert is never shown or counted (B8).
        let shown: Vec<i64> = s
            .store
            .shown_alerts(even, None, 10, None, false)
            .unwrap()
            .iter()
            .map(|a| a.torrent_id)
            .collect();
        assert_eq!(shown, vec![stored]);
        assert_eq!(s.store.unread_shown(None).unwrap(), 2, "`stored` twice");
        assert!(s.store.promoted_due(i64::MAX, 10).unwrap().is_empty());
    }

    /// The runner's case: the index's log seq passed the promotion before
    /// its ids reached the promoted one (a rebuild from empty, a backlog).
    #[test]
    fn a_row_the_index_has_not_reached_waits_without_spinning() {
        let mut s = setup(0);
        add(&mut s.store, &names(0, 5));
        let item = unverified(&mut s.store, "Promo.Even");
        add(&mut s.store, &names(5, 20));
        let promo = save(&mut s.store, "promo", "Promo", 21);
        assert_eq!(
            promote(&mut s.store, item, "Promo.Even", 0),
            Promoted::Kept(item)
        );
        s.index = SearchIndex::in_memory().unwrap();
        let mut w = s.index.writer(15 << 20).unwrap();
        w.catch_up(&mut s.store, 2).unwrap();
        w.commit().unwrap();
        assert!(s.index.watermark().unwrap() < item);
        let applied = s.index.reindex_watermark().unwrap();
        assert_eq!(
            s.store.promoted_due(applied, 10).unwrap().len(),
            1,
            "applied"
        );
        for _ in 0..3 {
            let r = run(&mut s, 2);
            assert_eq!((r.promoted, r.more), (0, false), "it waits, not spins");
        }
        assert_eq!(s.store.promoted_due(i64::MAX, 10).unwrap().len(), 1);
        while w.catch_up(&mut s.store, 100).unwrap() > 0 {}
        w.commit().unwrap();
        let r = run(&mut s, 2);
        assert_eq!((r.promoted, r.more), (1, false));
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
    }

    /// The file index lost its documents (a crash between the two
    /// commits): it takes the log's head at once but its ids start again,
    /// so a promoted id it hasn't reached waits, then alerts once.
    #[test]
    fn a_row_the_file_index_has_not_reached_waits() {
        let mut s = setup(0);
        add(&mut s.store, &names(0, 5));
        let item = unverified(&mut s.store, "Promo.Even");
        add(&mut s.store, &names(5, 20));
        let promo = save(&mut s.store, "promo", "Promo", 21);
        let path = s._dir.path().join("index");
        {
            let index = SearchIndex::open(&path).unwrap();
            index_all(&index, &mut s.store);
        }
        promote(&mut s.store, item, "Promo.Even", 0);
        std::fs::remove_dir_all(path.join("files")).unwrap();
        s.index = SearchIndex::open(&path).unwrap();
        let mut w = s.index.writer(15 << 20).unwrap();
        w.catch_up(&mut s.store, 2).unwrap();
        w.commit().unwrap();
        let seq = s.store.promoted_due(i64::MAX, 1).unwrap()[0].seq;
        assert!(s.index.reindex_watermark().unwrap() >= seq, "applied");
        assert!(s.index.watermark().unwrap() >= item);
        assert!(s.index.files_watermark().unwrap() < item);
        let r = run(&mut s, 2);
        assert_eq!((r.promoted, r.more), (0, false), "it waits for the files");
        while w.catch_up(&mut s.store, 100).unwrap() > 0 {}
        w.commit().unwrap();
        let r = run(&mut s, 2);
        assert_eq!(r.promoted, 1);
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
    }

    /// A search still below the promoted id meets it in its own range,
    /// once, and the promoted pass adds nothing to it.
    #[test]
    fn a_search_below_the_promoted_id_alerts_once() {
        let (mut s, item, promo, _) = promo_setup();
        let w = s.index.watermark().unwrap();
        let low = save(&mut s.store, "low", "Promo", 0);
        promote(&mut s.store, item, "Promo.Even", 0);
        index_all(&s.index, &mut s.store);
        let r = run(&mut s, w);
        assert_eq!(r.promoted, 1);
        assert_eq!(alert_ids(&s.store, low), vec![item]);
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
        let again = run(&mut s, w);
        assert_eq!((again.alerts, again.promoted), (0, 0));
    }

    #[test]
    fn an_unreadable_search_does_not_hold_the_queue() {
        let (mut s, item, promo, even) = promo_setup();
        let w = s.index.watermark().unwrap();
        let e = s.store.saved_search(even).unwrap().unwrap();
        s.store
            .update_saved_search(
                even,
                &SavedSearchInput {
                    name: &e.name,
                    query: &e.query,
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
        promote(&mut s.store, item, "Promo.Even", 0);
        index_all(&s.index, &mut s.store);
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.unreadable), (1, 0));
        assert_eq!(alert_ids(&s.store, promo), vec![item]);
        assert!(alert_ids(&s.store, even).is_empty());
        assert!(s.store.promoted_due(i64::MAX, 10).unwrap().is_empty());
    }

    /// Quarantined after promotion, before the percolator ran: it's
    /// deleted, so nothing alerts and the row is still consumed.
    #[test]
    fn a_quarantined_promotion_does_not_alert() {
        let (mut s, item, promo, _) = promo_setup();
        let w = s.index.watermark().unwrap();
        promote(&mut s.store, item, "Promo.Even", 0);
        index_all(&s.index, &mut s.store);
        let raw = info("Promo.Even");
        let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
        let hash = s
            .store
            .preparer()
            .prepare(&parsed, None)
            .unwrap()
            .info_hash
            .0;
        assert_eq!(s.store.delete_torrents(&[hash]).unwrap(), vec![item]);
        index_all(&s.index, &mut s.store);
        let r = run(&mut s, w);
        assert_eq!((r.promoted, r.alerts), (1, 0));
        assert!(alert_ids(&s.store, promo).is_empty());
        assert!(s.store.promoted_due(i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn a_full_batch_leaves_the_rest_for_the_next_run() {
        let mut s = setup(0);
        let items: Vec<i64> = (0..PROMOTED_BATCH + 3)
            .map(|i| unverified(&mut s.store, &format!("Promo.{i}")))
            .collect();
        add(&mut s.store, &names(0, 5));
        index_all(&s.index, &mut s.store);
        let w = s.index.watermark().unwrap();
        let promo = save(&mut s.store, "promo", "Promo", w);
        for (i, id) in items.iter().enumerate() {
            promote(&mut s.store, *id, &format!("Promo.{i}"), 0);
        }
        index_all(&s.index, &mut s.store);
        let first = run(&mut s, w);
        assert_eq!((first.promoted, first.more), (PROMOTED_BATCH, true));
        let second = run(&mut s, w);
        assert_eq!((second.promoted, second.more), (3, false));
        assert_eq!(alert_ids(&s.store, promo), items);
    }

    /// The cost of one full batch, for the decisions file:
    /// `cargo test --release -p lodestar promoted_batch_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement"]
    fn promoted_batch_cost() {
        let mut s = setup(0);
        let items: Vec<i64> = (0..PROMOTED_BATCH)
            .map(|i| unverified(&mut s.store, &format!("Promo.{i}")))
            .collect();
        add(&mut s.store, &names(0, 10_000));
        index_all(&s.index, &mut s.store);
        let w = s.index.watermark().unwrap();
        for i in 0..20 {
            let q = if i % 2 == 0 { "Promo" } else { "Show Even" };
            save(&mut s.store, &format!("s{i}"), q, w);
        }
        for (i, id) in items.iter().enumerate() {
            promote(&mut s.store, *id, &format!("Promo.{i}"), 0);
        }
        index_all(&s.index, &mut s.store);
        let config = PercolatorConfig {
            budget: Duration::from_secs(60),
            ..quick()
        };
        let r = run_once(
            &s.index,
            &mut s.store,
            None,
            w,
            &config,
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(r.promoted, PROMOTED_BATCH);
        println!(
            "{} promoted x 20 searches: {:?}, {} alerts",
            r.promoted, r.promoted_took, r.alerts
        );
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
    /// torrent name. The capture is [`crate::test_log`]'s, so every
    /// thread's events reach it; the secrets are spelled so no other test
    /// logs them.
    #[test]
    fn logs_carry_no_query_or_name() {
        crate::test_log::install();

        let mut s = setup(0);
        add(&mut s.store, &["Zq7SecretTorrent.1080p".to_string()]);
        index_all(&s.index, &mut s.store);
        save(&mut s.store, "Zq7MineSearch", "Zq7SecretTorrent", 0);
        // Ids are never reused: this gives the broken search an id no
        // other test's search has, so its lines here are its own.
        for _ in 0..500 {
            let id = save(&mut s.store, "filler", "x", 0);
            s.store.delete_saved_search(id).unwrap();
        }
        let broken = s
            .store
            .create_saved_search(
                &SavedSearchInput {
                    name: "Zq7BrokenSearch",
                    query: "Zq7PrivateQueryText",
                    ast: r#"{"op":"Zq7PrivateQueryText"}"#,
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
        let mut seen = Seen::default();
        let run = run_once(&s.index, &mut s.store, None, 1, &quick(), NOW, &mut seen).unwrap();
        assert_eq!((run.alerts, run.unreadable), (1, 1));
        // Still unreadable next run: counted, not logged again.
        let run = run_once(&s.index, &mut s.store, None, 1, &quick(), NOW, &mut seen).unwrap();
        assert_eq!(run.unreadable, 1);
        prune(&mut s.store, 0);
        let log = crate::test_log::text();
        let line = format!("saved_search={broken} event=\"alerts.search_unreadable\"");
        assert_eq!(log.matches(&line).count(), 1, "{log}");
        assert!(log.contains("alerts=1 event=\"alerts.pruned\""), "{log}");
        // No name, query or torrent name: each holds `Zq7`.
        assert!(!log.contains("Zq7"), "{log}");
    }

    /// Live queries join a run after its saved searches are recorded and
    /// announced, and count what each would show. Measured at 200 saved
    /// searches and the most live queries a hub holds (256): the saved
    /// part of the run is timed alone, then the live part.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_queries_follow_the_saved_searches() {
        use lodestar_api::events::{EventSources, EventsConfig};
        use lodestar_api::search_live::percolate_live;

        let mut s = setup(4000);
        let mut saved = Vec::new();
        for i in 0..200 {
            let q = if i == 0 { "Even" } else { "Show" };
            saved.push(save(&mut s.store, &format!("s{i}"), q, 2000));
        }
        let hub = EventHub::start(EventsConfig::default(), EventSources::default());
        let live = hub.live();
        for i in 0..256u64 {
            let q = parse(if i % 2 == 0 { "Even" } else { "Show" }).unwrap();
            live.register(i / 4, &q, false, 0, 2000).unwrap();
        }
        assert_eq!(live.len(), 256);
        // The saved part alone: what a run without live queries costs.
        let started = Instant::now();
        let run = run_once(
            &s.index,
            &mut s.store,
            None,
            4000,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        let saved_took = started.elapsed();
        assert_eq!((run.searches, run.live, run.more), (200, 0, false));
        assert_eq!(alert_ids(&s.store, saved[0]), evens(2001, 4000));
        // The live part, over the same range.
        let started = Instant::now();
        let limits = PercolateLimits {
            keep: quick().keep,
            max_range: quick().max_range,
        };
        let token = CancelToken::new().with_budget(quick().budget);
        let lr = percolate_live(&s.index, live, 4000, limits, NOW, &token, false).unwrap();
        let live_took = started.elapsed();
        assert_eq!((lr.ran, lr.more), (256, false));
        // 128 x 1000 evens and 128 x 2000 of everything.
        assert_eq!(lr.matched, 128 * 1000 + 128 * 2000);
        eprintln!(
            "2000 new torrents: 200 saved searches {saved_took:?}, then 256 live queries {live_took:?}"
        );
        // Through run_once: saved searches first, then the live ones.
        hub.close();
        let hub = EventHub::start(EventsConfig::default(), EventSources::default());
        for i in 0..256u64 {
            let q = parse("Show").unwrap();
            hub.live().register(i / 4, &q, false, 0, 3000).unwrap();
        }
        save(&mut s.store, "late", "Show", 3000);
        let run = run_once(
            &s.index,
            &mut s.store,
            Some((&hub, &AdultVisibility::default())),
            4000,
            &quick(),
            NOW,
            &mut Seen::default(),
        )
        .unwrap();
        assert_eq!(
            (run.searches, run.alerts, run.live, run.more),
            (1, 1000, 256, false)
        );
        hub.close();
    }

    /// The task over a live index: a commit becomes alerts within the
    /// 5 s target (§3), here with 200 active searches, timed from the
    /// commit that makes the new torrents searchable, as the percolator
    /// hears of it; and in fixture mode it never starts. (What the events
    /// carry is tested with `announce`, in the API's `tests/alerts.rs`.)
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

        // When the commit carrying the new torrents is reported: through
        // the hook the task listens on, after the reader has reloaded.
        // Added before the task's sink, so it runs first.
        let landed = Arc::new(std::sync::OnceLock::new());
        let l = landed.clone();
        serving.commit_hook().add(move |c| {
            if c.max_torrent_id > mark {
                let _ = l.set(Instant::now());
            }
        });
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
        let alerted = Instant::now();
        // Commit-to-alert. Store-to-commit (up to one 2 s commit interval
        // and the commit itself) is the indexer's share, "new torrent →
        // searchable", which `search-bench --gate` holds to 5 s on a quiet
        // machine: reported here, not asserted (X-flake-shutdown-percolator).
        let landed = *landed.get().expect("the commit was reported");
        let (indexed, took) = (landed - start, alerted - landed);
        eprintln!("at 200 searches: store-to-commit {indexed:?}, commit-to-alert {took:?}");
        assert!(
            took < Duration::from_secs(5),
            "commit-to-alert {took:?} (store-to-commit {indexed:?})"
        );
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
    /// A quiet instance: a promotion's commit adds no torrent, so the
    /// hook doesn't wake the task; the idle poll finds it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_alerts_on_a_promotion_with_no_new_torrents() {
        use lodestar_api::events::{EventSources, EventsConfig};

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let item = unverified(&mut store, "Promo.Even");
        add(&mut store, &names(0, 20));
        let serving = Arc::new(
            Serving::open_or_rebuild(
                &tmp.path().join("index"),
                &db,
                ServingConfig {
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
        let deadline = Instant::now() + Duration::from_secs(30);
        while handle.index().watermark().unwrap() < 21 {
            assert!(Instant::now() < deadline, "the index caught up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let promo = save(&mut store, "promo", "Promo", 21);
        let hub = EventHub::start(EventsConfig::default(), EventSources::default());
        let (stop, stopped) = watch::channel(false);
        let task = spawn(
            handle.clone(),
            serving.commit_hook(),
            Store::open(&db, StoreConfig::default()).unwrap(),
            hub.clone(),
            AdultVisibility::default(),
            stopped,
            PercolatorConfig {
                promoted_poll: Duration::from_millis(200),
                ..quick()
            },
        );
        assert_eq!(
            promote(&mut store, item, "Promo.Even", 0),
            Promoted::Kept(item)
        );
        while alert_ids(&store, promo).is_empty() {
            assert!(Instant::now() < deadline, "the promotion alerted");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(alert_ids(&store, promo), vec![item]);
        stop.send(true).unwrap();
        task.await.unwrap();
        hub.close();
        drop(handle);
        Arc::try_unwrap(serving).unwrap().shutdown().unwrap();
    }
}
