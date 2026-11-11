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
//!   coverage); `lodestar rollup --repair` recomputes searchable counts
//!   from `torrents` and `index_watermark_hour`, and runs by itself over
//!   the last 72 h at the next start ([`repair_after_unclean_stop`]).
//! - **Daily compaction** (B11b): a tick 10 minutes after start, and then
//!   once `last_compaction_at` is 24 h old, folds hourly rollups older
//!   than 30 days into daily rows, a day per transaction, for at most a
//!   second per tick (the loop records commits between steps), then
//!   prunes the query log and gives back the pages it freed.
//! - **The imported total** mirrors `insights_state.imported`, which the
//!   store keeps exact (no scan at start).
//! - **Host exceptions** (PK-exc): the data folder on a FUSE or network
//!   filesystem (`data_filesystem`) and, on Windows, a narrow ephemeral
//!   port range (`ephemeral_ports`). [`HostChecks`] re-checks them at most
//!   every [`HOST_RECHECK_S`], from the overview request that finds the
//!   last check old: one check at a time, never under a lock.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lodestar_api::insights::{
    self as api, InsightsException, InsightsSource, OverviewCache, StageSnapshot,
};
use lodestar_pipeline::Monitor;
use lodestar_pipeline::metric::{self, coverage};
use lodestar_search::{CommitHook, Committed, IndexStats};
use lodestar_sources::Stage;
use lodestar_store::{
    CompactOptions, Counter, QueuedKey, RepairCodes, RepairOptions, Repaired, SearchableCodes,
    Store, StoreConfig,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// The codes `record_searchable` writes under.
pub const CODES: SearchableCodes = SearchableCodes {
    stage: Stage::Searchable as u8,
    newly: metric::NEWLY_SEARCHABLE,
    delay: metric::TIME_TO_SEARCHABLE,
};

/// What `rollup --repair` writes under.
pub const REPAIR_CODES: RepairCodes = RepairCodes {
    searchable: CODES,
    stored_stage: Stage::Stored as u8,
    stored: metric::STORED,
};

/// Coverage is added, and the index's watermark checked, this often.
pub const TICK: Duration = Duration::from_secs(30);
/// Compaction is first considered this long after start, so start-up
/// isn't slowed.
const COMPACT_AFTER: Duration = Duration::from_secs(600);
/// ... and runs when the last finished run is this old.
const COMPACT_EVERY_S: i64 = 86_400;
/// Hourly rows are kept this many days (§7).
pub const KEEP_HOURLY_DAYS: i64 = 30;
/// A compaction step gives the loop back after this long.
const COMPACT_BUDGET: Duration = Duration::from_secs(1);
/// Free pages handed back per tick after a run, so no one vacuum holds
/// the write lock or fills the WAL (4 MB at 16 KB pages).
const VACUUM_CHUNK: u64 = 256;
/// The query log keeps 30 days (§10.9.5) and at most this many rows.
const QUERY_LOG_DAYS: i64 = 30;
pub const QUERY_LOG_ROWS: u64 = 100_000;
/// The automatic repair after an unclean stop recounts this far back:
/// the retry horizon.
pub const AUTO_REPAIR_HOURS: i64 = 72;
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
    /// Imported torrents, mirroring `insights_state`; -1 until it's read.
    imported: AtomicI64,
    /// The oldest cohort of the keys restored at start (`i64::MAX`:
    /// none), which keeps its days unsealed until the restored keys have
    /// resolved (`floor_until`, unix seconds).
    floor: AtomicI64,
    floor_until: AtomicI64,
    cache: OverviewCache,
    /// When compaction is first considered, after start.
    compact_after: Duration,
    /// The stage pages' snapshot ([`Insights::set_snapshot`]).
    snapshot: std::sync::OnceLock<Box<dyn Fn() -> StageSnapshot + Send + Sync>>,
    /// The host exceptions' checks, set by [`spawn`] (which knows the
    /// data folder).
    host: std::sync::OnceLock<HostChecks>,
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
            floor: AtomicI64::new(i64::MAX),
            floor_until: AtomicI64::new(0),
            cache: OverviewCache::default(),
            compact_after: COMPACT_AFTER,
            snapshot: std::sync::OnceLock::new(),
            host: std::sync::OnceLock::new(),
        }
    }

    /// Where the stage pages' snapshot comes from (the DHT's reply ratio);
    /// set once, at start.
    pub fn set_snapshot(&self, f: Box<dyn Fn() -> StageSnapshot + Send + Sync>) {
        let _ = self.snapshot.set(f);
    }

    /// Checks `data_dir`'s filesystem and the port range for the
    /// overview's exceptions; set once.
    pub fn set_host_checks(&self, data_dir: PathBuf) {
        self.set_host_probe(data_dir, crate::windows::ports::probe);
    }

    /// [`Insights::set_host_checks`] with the port probe given (a fake in
    /// tests, since the real one reads a process-wide count).
    fn set_host_probe(&self, data_dir: PathBuf, ports: PortProbe) {
        let _ = self.host.set(HostChecks::new(Box::new(move |now| {
            probe_host(&data_dir, now, ports)
        })));
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

    fn stage_snapshot(&self) -> StageSnapshot {
        self.snapshot
            .get()
            .map_or_else(StageSnapshot::default, |f| f())
    }

    fn host_exceptions(&self, now: i64) -> Vec<InsightsException> {
        self.host.get().map_or_else(Vec::new, |h| h.get(now))
    }

    fn seal_floor(&self) -> Option<i64> {
        let floor = self.floor.load(Ordering::Acquire);
        (floor != i64::MAX && crate::unix_now() < self.floor_until.load(Ordering::Acquire))
            .then_some(floor)
    }
}

/// The host exceptions are re-checked at most this often (an hour): the
/// mount table is cheap to read but not free, and `netsh` is a process.
pub const HOST_RECHECK_S: i64 = 3600;
/// ... or this soon when a check couldn't see everything yet (the port
/// check before `services` has set the socket count).
pub const HOST_RETRY_S: i64 = 60;

/// What a host check finds at a time: the exceptions, `since` that time,
/// and whether every check could run.
type HostProbe = Box<dyn Fn(i64) -> HostFound + Send + Sync>;

/// [`HostProbe`]'s answer.
pub struct HostFound {
    pub exceptions: Vec<InsightsException>,
    /// False when a check couldn't run yet: the next is due after
    /// [`HOST_RETRY_S`], not [`HOST_RECHECK_S`].
    pub complete: bool,
}

/// The host exceptions, checked at most every [`HOST_RECHECK_S`] (every
/// [`HOST_RETRY_S`] while a check can't run yet).
///
/// The overview request that finds the last check old (or none) claims
/// the next one and runs it on its own blocking thread, with no lock
/// held; requests meanwhile get the last answer at once (none before the
/// first check ends), so concurrent requests never queue behind it or
/// start a second. An exception still found keeps the `since` it was
/// first found at.
pub struct HostChecks {
    probe: HostProbe,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    /// When the last check was claimed.
    checked_at: Option<i64>,
    /// How long after `checked_at` the next is due.
    interval: i64,
    /// A check is out.
    running: bool,
    found: Vec<InsightsException>,
}

impl HostChecks {
    fn new(probe: HostProbe) -> HostChecks {
        HostChecks {
            probe,
            state: Mutex::default(),
        }
    }

    /// The exceptions at `now`, checking first when that's due.
    pub fn get(&self, now: i64) -> Vec<InsightsException> {
        {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            // A clock set back makes the check due, rather than hold it
            // off for as long as the clock went back.
            let due = s
                .checked_at
                .is_none_or(|t| now < t || now - t >= s.interval);
            if !due || s.running {
                return s.found.clone();
            }
            // Claimed now, so a check that fails is tried again an hour on,
            // not by every request.
            s.running = true;
            s.checked_at = Some(now);
            s.interval = HOST_RECHECK_S;
        }
        let mut claim = Claim {
            state: &self.state,
            held: true,
        };
        let fresh = (self.probe)(now);
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !fresh.complete {
            s.interval = HOST_RETRY_S;
        }
        let found: Vec<InsightsException> = fresh
            .exceptions
            .into_iter()
            .map(|mut e| {
                if let Some(old) = s.found.iter().find(|o| o.kind == e.kind) {
                    e.since = old.since;
                }
                e
            })
            .collect();
        s.found = found.clone();
        s.running = false;
        claim.held = false;
        found
    }
}

/// Releases a claimed check if its probe panics.
struct Claim<'a> {
    state: &'a Mutex<HostState>,
    held: bool,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.held {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .running = false;
        }
    }
}

/// `ports::probe`: whether the port check can answer yet, and its warning.
type PortProbe = fn() -> (bool, Option<crate::windows::ports::Warning>);

/// The host exceptions at `now`: the data folder's filesystem (every OS)
/// and the port range (`ports`: Windows; never elsewhere). Incomplete
/// until the socket count is set (`ports::ready`), which `services` does
/// at start.
fn probe_host(data_dir: &Path, now: i64, ports: PortProbe) -> HostFound {
    let (complete, pressure) = ports();
    let filesystem = crate::fsdetect::detect(data_dir)
        .exception_detail()
        .map(|fs| api::data_filesystem(fs, now));
    let pressure = pressure.map(|w| api::ephemeral_ports(w.detail(), now));
    HostFound {
        exceptions: filesystem.into_iter().chain(pressure).collect(),
        complete,
    }
}

/// The data folder of the store at `db`: `Config::db_path` is
/// `<data>/lodestar.db`, and a bare file name is in the working directory.
fn data_dir_of(db: &Path) -> PathBuf {
    db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// What the recorder follows.
pub struct Follow {
    pub db: PathBuf,
    pub store: StoreConfig,
    /// The serving index's hook and counters; `None` with search off.
    pub index: Option<(Arc<CommitHook>, Arc<IndexStats>)>,
    /// Whether the crawler is paused now.
    pub paused: Box<dyn Fn() -> bool + Send + Sync>,
    /// Documents in the index now; `None` with search off.
    pub searchable: Box<dyn Fn() -> Option<u64> + Send + Sync>,
    /// How often coverage is added ([`TICK`]).
    pub tick: Duration,
}

/// Starts the recorder. It runs until `stop` turns true, recording what
/// the index made searchable once more on the way out.
pub fn spawn(
    insights: Arc<Insights>,
    follow: Follow,
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    insights.set_host_checks(data_dir_of(&follow.db));
    let (tx, rx) = mpsc::channel(QUEUE);
    if let Some((hook, _)) = &follow.index {
        hook.add(move |c: Committed| {
            let _ = tx.try_send(c);
        });
    }
    tokio::spawn(async move {
        if run(&insights, follow, rx, stop).await.is_err() {
            lodestar_log::log_event!(
                crate::logging::events::INSIGHTS_RECORDER_STOPPED,
                error = lodestar_log::Error::kind("recorder")
            );
        }
    })
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The oldest cohort the saved fetch queue holds back from sealing. A
/// refetch isn't the funnel's, and the pipeline drops saved ones at start,
/// so one saved by an older build (under a real hour) holds nothing back.
fn queue_floor(queue: &[QueuedKey]) -> Option<i64> {
    queue
        .iter()
        .filter(|k| k.strategy != lodestar_pipeline::strategy::REFETCH)
        .map(|k| k.cohort)
        .min()
}

async fn run(
    insights: &Arc<Insights>,
    follow: Follow,
    mut commits: mpsc::Receiver<Committed>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), BoxError> {
    let (db, config) = (follow.db.clone(), follow.store.clone());
    let mut store = tokio::task::spawn_blocking(move || Store::open(&db, config)).await??;
    // Keys restored from the fetch queue keep their cohort, however old
    // (a long downtime): those days take Stored and lost counts until the
    // keys resolve, within the retry horizon, so they aren't sealed till
    // then.
    let queued;
    (store, queued) = blocking(store, |s| s.load_fetch_queue()).await?;
    match queued {
        Ok(q) => {
            if let Some(oldest) = queue_floor(&q) {
                insights.floor.store(oldest, Ordering::Release);
                insights.floor_until.store(
                    crate::unix_now() + (api::RETRY_HORIZON_HOURS + 1) * 3600,
                    Ordering::Release,
                );
            }
        }
        Err(e) => lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_STEP_FAILED,
            step = "fetch_queue",
            error = lodestar_log::Error::new(&e)
        ),
    }
    // The sealed days' sums first, so the first long range is quick.
    store = warm(insights, store).await?;
    let watermark = |f: &Follow| {
        f.index
            .as_ref()
            .map(|(_, stats)| stats.watermark.load(Ordering::Relaxed))
    };
    // Whatever the index committed before the hook was attached.
    if let Some(w) = watermark(&follow) {
        store = record(insights, store, w, crate::unix_now()).await?;
    }
    // Imports found in `indexing` at start (an offline `lodestar import`)
    // the watermark has already passed, or that added no rows.
    store = finish_imports(store).await?;
    store = mirror_state(insights, store).await?;
    let mut compaction = Compaction {
        from: crate::unix_now() + insights.compact_after.as_secs() as i64,
        running: false,
        freed: 0,
        unknown_logged: false,
    };
    let mut tick = tokio::time::interval(follow.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut covered_to = crate::unix_now();
    loop {
        tokio::select! {
            // With search off nothing sends: only the tick and the stop.
            c = commits.recv(), if follow.index.is_some() => {
                let Some(c) = c else { break };
                store = record(insights, store, c.max_torrent_id, c.at as i64).await?;
            }
            _ = tick.tick() => {
                let now = crate::unix_now();
                let rows = coverage_rows(covered_to, now, follow.index.is_some(), !(follow.paused)());
                covered_to = now;
                // The hour's searchable snapshot: the last one in it wins.
                let gauge: Vec<Counter> = (follow.searchable)()
                    .map(|n| Counter {
                        hour: now.div_euclid(3600),
                        metric: metric::SEARCHABLE,
                        dim: 0,
                        key: 0,
                        value: i64::try_from(n).unwrap_or(i64::MAX),
                    })
                    .into_iter()
                    .collect();
                let r;
                (store, r) = blocking(store, move |s| {
                    s.add_counters(&rows).and_then(|()| s.set_gauges(&gauge))
                })
                .await?;
                match r {
                    Ok(()) => insights.bump(),
                    Err(e) => lodestar_log::log_event!(
                        crate::logging::events::INSIGHTS_STEP_FAILED,
                        step = "coverage",
                        error = lodestar_log::Error::new(&e)
                    ),
                }
                if let Some(w) = watermark(&follow) {
                    store = record(insights, store, w, now).await?;
                }
                // A job that reached `indexing` while the server runs with
                // no new rows, or one paused at `indexing` and resumed,
                // is due without the watermark moving.
                store = finish_imports(store).await?;
                store = mirror_state(insights, store).await?;
                if now >= compaction.from {
                    store = compact(insights, store, &mut compaction, now).await?;
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

/// Reads the sealed days' sums the cache lacks.
async fn warm(insights: &Arc<Insights>, store: Store) -> Result<Store, BoxError> {
    let i = insights.clone();
    let (store, warmed) = blocking(store, move |s| {
        api::warm(s, &i.cache, crate::unix_now(), i.seal_floor())
    })
    .await?;
    if let Err(e) = warmed {
        lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_STEP_FAILED,
            step = "sealed_days",
            error = lodestar_log::Error::new(&e)
        );
    }
    Ok(store)
}

async fn record(
    insights: &Arc<Insights>,
    store: Store,
    last_id: i64,
    at: i64,
) -> Result<Store, BoxError> {
    let (store, r) = blocking(store, move |s| s.record_searchable(last_id, at, CODES)).await?;
    match r {
        Ok(Some(r)) => {
            // An index backlog made old cohorts searchable: those sealed
            // days' sums are out of date. Read them again.
            let sealed = api::sealed_hour(crate::unix_now(), insights.seal_floor());
            let stale = r.first_cohort.filter(|&c| c < sealed);
            // The watermark moved: an import it passed is searchable now.
            // Before the bump, so a reader that saw it sees the jobs too.
            let store = finish_imports(store).await?;
            insights.bump();
            let store = if r.imported > 0 {
                mirror_state(insights, store).await?
            } else {
                store
            };
            if let Some(c) = stale {
                insights.cache.forget_days_from(c);
                return warm(insights, store).await;
            }
            return Ok(store);
        }
        Ok(None) => {}
        // A busy or failed write: the next commit or tick covers the ids.
        Err(e) => lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_STEP_FAILED,
            step = "searchable_time",
            error = lodestar_log::Error::new(&e)
        ),
    }
    Ok(store)
}

/// Moves the `indexing` import jobs the index watermark has passed to
/// `complete` (or `complete_with_issues`, which the store picks). The
/// watermark is the table [`Store::record_searchable`] raises, so a job
/// completes only once its rows are searchable. A failed write is retried
/// at the next commit or tick.
async fn finish_imports(store: Store) -> Result<Store, BoxError> {
    let (store, r) = blocking(store, |s| s.complete_indexed_import_jobs(crate::unix_now())).await?;
    if let Err(e) = r {
        lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_STEP_FAILED,
            step = "imports",
            error = lodestar_log::Error::new(&e)
        );
    }
    Ok(store)
}

/// Copies `insights_state.imported` (kept exact by the store) into
/// [`Insights`]; on a failed read the last value stays.
async fn mirror_state(insights: &Arc<Insights>, store: Store) -> Result<Store, BoxError> {
    let (store, st) = blocking(store, |s| s.insights_state()).await?;
    match st {
        Ok(st) => insights
            .imported
            .store(st.imported.max(0), Ordering::Release),
        Err(e) => lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_STEP_FAILED,
            step = "state",
            error = lodestar_log::Error::new(&e)
        ),
    }
    Ok(store)
}

/// Where the daily compaction is.
#[derive(Debug)]
struct Compaction {
    /// Not before this (unix seconds): 10 minutes after start.
    from: i64,
    /// A run is under way, cut short by its budget.
    running: bool,
    /// Free pages the runs gave up, still to hand back.
    freed: u64,
    unknown_logged: bool,
}

/// One compaction step, when one is due: a budgeted call, and when the
/// run is done, the query log's prune. Between runs, each tick hands back
/// up to [`VACUUM_CHUNK`] of the pages they freed.
async fn compact(
    insights: &Arc<Insights>,
    mut store: Store,
    c: &mut Compaction,
    now: i64,
) -> Result<Store, BoxError> {
    if !c.running && c.freed > 0 {
        let n = c.freed.min(VACUUM_CHUNK);
        let r;
        (store, r) = blocking(store, move |s| s.incremental_vacuum(n)).await?;
        match r {
            Ok(()) => c.freed -= n,
            Err(e) => {
                lodestar_log::log_event!(
                    crate::logging::events::INSIGHTS_STEP_FAILED,
                    step = "vacuum",
                    error = lodestar_log::Error::new(&e)
                );
                c.freed = 0;
            }
        }
    }
    let (store, due) = blocking(store, move |s| s.insights_state()).await?;
    let due = match due {
        Ok(st) => c.running || now - st.last_compaction_at >= COMPACT_EVERY_S,
        Err(e) => {
            lodestar_log::log_event!(
                crate::logging::events::INSIGHTS_STEP_FAILED,
                step = "state",
                error = lodestar_log::Error::new(&e)
            );
            false
        }
    };
    if !due {
        return Ok(store);
    }
    let hold = insights.seal_floor().map(|h| h.div_euclid(24));
    let (mut store, r) = blocking(store, move |s| {
        s.compact_rollups(CompactOptions {
            now,
            keep_days: KEEP_HOURLY_DAYS,
            hold_from_day: hold,
            budget: COMPACT_BUDGET,
            kind: &metric::aggregation,
        })
    })
    .await?;
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            // The day it was on is whole either way; the next tick goes on.
            lodestar_log::log_event!(
                crate::logging::events::INSIGHTS_STEP_FAILED,
                step = "compaction",
                error = lodestar_log::Error::new(&e)
            );
            return Ok(store);
        }
    };
    if !r.unknown_metrics.is_empty() && !c.unknown_logged {
        c.unknown_logged = true;
        // Their names come from the store: counted, never named.
        lodestar_log::log_event!(
            crate::logging::events::INSIGHTS_UNKNOWN_METRICS,
            count = r.unknown_metrics.len()
        );
    }
    c.freed += r.freed_pages;
    c.running = !r.done;
    if r.days + r.stragglers > 0 {
        insights.cache.forget_days_before(r.compacted_before_day);
        insights.bump();
    }
    if r.done {
        let pruned;
        (store, pruned) = blocking(store, move |s| {
            s.prune_query_log(now - QUERY_LOG_DAYS * 86_400, QUERY_LOG_ROWS)
        })
        .await?;
        match pruned {
            Ok(_) => lodestar_log::log_event!(
                crate::logging::events::INSIGHTS_COMPACTED,
                day = lodestar_log::Count(r.compacted_before_day as u64),
                rows = r.rows as u64
            ),
            Err(e) => lodestar_log::log_event!(
                crate::logging::events::INSIGHTS_STEP_FAILED,
                step = "query_log_prune",
                error = lodestar_log::Error::new(&e)
            ),
        }
    }
    Ok(store)
}

/// `lodestar rollup --repair` by itself at start, on the writer's store
/// before the pipeline takes it (nothing else writes rollups yet), when
/// the last run didn't stop cleanly: the newest start/stop event is a
/// start. It recounts what was stored in the 72 h before the store's last
/// activity (the crash, not now: a box that lost power Friday and starts
/// Tuesday repairs Friday), which takes seconds, not a full scan.
pub fn repair_after_unclean_stop(store: &mut Store) -> lodestar_store::Result<Option<Repaired>> {
    use lodestar_pipeline::event;
    if !store.stopped_uncleanly(event::START, event::STOP)? {
        return Ok(None);
    }
    let now = crate::unix_now();
    let last = store.last_activity()?.map_or(now, |t| t.min(now));
    let since_hour = last.div_euclid(3600) - AUTO_REPAIR_HOURS;
    store
        .repair_rollups(RepairOptions {
            codes: REPAIR_CODES,
            since_hour,
            recount_imported: false,
        })
        .map(Some)
}

/// Coverage seconds for `from..to`, split at hour boundaries. Nothing
/// counts as measured while the crawler is paused (§10.9.3: a paused
/// stretch is a gap, not a zero), so `INDEX` needs both.
fn coverage_rows(from: i64, to: i64, indexing: bool, crawling: bool) -> Vec<Counter> {
    let mut out = Vec::new();
    let mut t = from;
    while t < to {
        let hour = t.div_euclid(3600);
        let end = ((hour + 1) * 3600).min(to);
        for (on, dim) in [
            (indexing && crawling, coverage::INDEX),
            (crawling, coverage::CRAWL),
        ] {
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
mod host_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc as std_mpsc;

    use lodestar_api::insights::{DataFilesystem, DataFsKind};

    use super::*;

    const T: i64 = 1_800_000_000;
    /// Generous for the slowest CI runner; a passing run never waits it out.
    const DEADLINE: Duration = Duration::from_secs(10);

    fn shfs(since: i64) -> InsightsException {
        api::data_filesystem(
            DataFilesystem {
                kind: DataFsKind::UnraidUserShare,
                path: "/data".into(),
                fs_type: Some("fuse.shfs".into()),
            },
            since,
        )
    }

    /// A probe that counts its runs and answers what `answer` holds.
    fn counted(answer: Arc<Mutex<Vec<InsightsException>>>) -> (HostChecks, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let checks = HostChecks::new(Box::new(move |now| {
            r.fetch_add(1, Ordering::SeqCst);
            let mut exceptions = answer.lock().unwrap().clone();
            for e in &mut exceptions {
                e.since = now;
            }
            HostFound {
                exceptions,
                complete: true,
            }
        }));
        (checks, runs)
    }

    #[test]
    fn checks_at_most_hourly_on_the_clock_given() {
        let answer = Arc::new(Mutex::new(vec![shfs(0)]));
        let (checks, runs) = counted(answer.clone());
        assert_eq!(checks.get(T), vec![shfs(T)]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // Within the hour: the cached answer, even after the cause is fixed.
        answer.lock().unwrap().clear();
        assert_eq!(checks.get(T + 1), vec![shfs(T)]);
        assert_eq!(checks.get(T + HOST_RECHECK_S - 1), vec![shfs(T)]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // An hour on, checked again: fixed, so gone.
        assert_eq!(checks.get(T + HOST_RECHECK_S), vec![]);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // Found again later: since is the new time, not the first one.
        *answer.lock().unwrap() = vec![shfs(0)];
        assert_eq!(
            checks.get(T + 2 * HOST_RECHECK_S),
            vec![shfs(T + 2 * HOST_RECHECK_S)]
        );
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn an_exception_still_found_keeps_its_since() {
        let answer = Arc::new(Mutex::new(vec![shfs(0)]));
        let (checks, _) = counted(answer);
        assert_eq!(checks.get(T)[0].since, T);
        assert_eq!(checks.get(T + HOST_RECHECK_S)[0].since, T);
        assert_eq!(checks.get(T + 5 * HOST_RECHECK_S)[0].since, T);
    }

    #[test]
    fn a_clock_set_back_checks_again() {
        let (checks, runs) = counted(Arc::new(Mutex::new(Vec::new())));
        checks.get(T);
        checks.get(T - 1);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // ... and the hour then counts from the new time.
        checks.get(T - 1 + HOST_RECHECK_S - 1);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn concurrent_requests_run_one_check_and_never_wait_for_it() {
        let (entered_tx, entered) = std_mpsc::channel();
        let (release, release_rx) = std_mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let checks = Arc::new(HostChecks::new(Box::new(move |now| {
            r.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            // The check is slow (a stuck `netsh`) until the test lets it go.
            release_rx.lock().unwrap().recv_timeout(DEADLINE).unwrap();
            HostFound {
                exceptions: vec![shfs(now)],
                complete: true,
            }
        })));
        let first = {
            let c = checks.clone();
            std::thread::spawn(move || c.get(T))
        };
        entered.recv_timeout(DEADLINE).unwrap();
        // While it runs, others get the last answer (none yet) at once,
        // and don't start a second check, now or after the hour.
        for now in [T, T + 1, T + HOST_RECHECK_S] {
            assert_eq!(checks.get(now), vec![]);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap(), vec![shfs(T)]);
        assert_eq!(checks.get(T + 1), vec![shfs(T)]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_check_that_panics_is_tried_again_an_hour_on() {
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let checks = Arc::new(HostChecks::new(Box::new(move |now| {
            if r.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("the first check fails");
            }
            HostFound {
                exceptions: vec![shfs(now)],
                complete: true,
            }
        })));
        let c = checks.clone();
        assert!(std::thread::spawn(move || c.get(T)).join().is_err());
        // Released, but not due again within the hour.
        assert_eq!(checks.get(T + 1), vec![]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            checks.get(T + HOST_RECHECK_S),
            vec![shfs(T + HOST_RECHECK_S)]
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_incomplete_check_is_retried_within_a_minute() {
        let ready = Arc::new(AtomicBool::new(false));
        let runs = Arc::new(AtomicUsize::new(0));
        let (r, ready_now) = (runs.clone(), ready.clone());
        let checks = HostChecks::new(Box::new(move |now| {
            r.fetch_add(1, Ordering::SeqCst);
            HostFound {
                exceptions: vec![shfs(now)],
                complete: ready_now.load(Ordering::SeqCst),
            }
        }));
        assert_eq!(checks.get(T), vec![shfs(T)]);
        checks.get(T + HOST_RETRY_S - 1);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        checks.get(T + HOST_RETRY_S);
        assert_eq!(runs.load(Ordering::SeqCst), 2, "retried after a minute");
        // Complete now: back to hourly, and the first `since` is kept.
        ready.store(true, Ordering::SeqCst);
        assert_eq!(checks.get(T + 2 * HOST_RETRY_S), vec![shfs(T)]);
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        checks.get(T + 2 * HOST_RETRY_S + HOST_RECHECK_S - 1);
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        checks.get(T + 2 * HOST_RETRY_S + HOST_RECHECK_S);
        assert_eq!(runs.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn the_data_folder_is_the_stores_folder() {
        assert_eq!(
            data_dir_of(Path::new("/data/lodestar.db")),
            Path::new("/data")
        );
        assert_eq!(
            data_dir_of(Path::new("data/lodestar.db")),
            Path::new("data")
        );
        assert_eq!(data_dir_of(Path::new("lodestar.db")), Path::new("."));
    }

    #[test]
    fn the_source_lists_the_data_folders_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let filesystem: Vec<InsightsException> = crate::fsdetect::detect(dir.path())
            .exception_detail()
            .map(|fs| api::data_filesystem(fs, T))
            .into_iter()
            .collect();
        let insights = Insights::with_clock(Box::new(|| 0), T);
        // Before `spawn` sets the checks: none.
        assert_eq!(insights.host_exceptions(T), vec![]);
        insights.set_host_probe(dir.path().to_path_buf(), || (true, None));
        assert_eq!(insights.host_exceptions(T), filesystem);

        // A narrow range comes after the filesystem, with its detail.
        fn narrow() -> (bool, Option<crate::windows::ports::Warning>) {
            use crate::windows::ports::{Family, Range, assess};
            let range = Range {
                start: 49152,
                count: 16384,
                excluded: 15000,
            };
            (true, assess(Family::V4, range, 400))
        }
        let insights = Insights::with_clock(Box::new(|| 0), T);
        insights.set_host_probe(dir.path().to_path_buf(), narrow);
        let mut expected = filesystem.clone();
        expected.push(api::ephemeral_ports(narrow().1.unwrap().detail(), T));
        assert_eq!(insights.host_exceptions(T), expected);
    }

    #[cfg(not(windows))]
    #[test]
    fn no_port_exception_off_windows() {
        // Whatever count is set: there's no range to read.
        assert_eq!(crate::windows::ports::probe(), (true, None));
    }
}

#[cfg(test)]
mod tests {
    use lodestar_api::insights::codes;
    use lodestar_sources::Loss;

    use super::*;

    #[test]
    fn a_saved_refetch_holds_no_cohort_back() {
        use lodestar_pipeline::strategy;
        let key = |cohort, strategy| QueuedKey {
            info_hash: lodestar_metainfo::InfoHash([1; 20]),
            source: lodestar_store::SourceId::new(1).unwrap(),
            cohort,
            strategy,
            priority: 0,
            attempts: 0,
            private_source: false,
        };
        // Saved by a build before refetches had their own cohort.
        let old_refetch = key(400_000, strategy::REFETCH);
        assert_eq!(queue_floor(&[old_refetch]), None);
        assert_eq!(
            queue_floor(&[old_refetch, key(400_010, strategy::SAMPLE)]),
            Some(400_010)
        );
        assert_eq!(
            queue_floor(&[
                key(400_020, strategy::LOOKUP),
                key(400_010, strategy::SAMPLE)
            ]),
            Some(400_010)
        );
        assert_eq!(queue_floor(&[]), None);
    }

    #[test]
    fn coverage_splits_at_hours() {
        let h = 500_000 * 3600;
        let rows = coverage_rows(h - 10, h + 20, false, true);
        let got: Vec<_> = rows.iter().map(|c| (c.hour, c.dim, c.value)).collect();
        assert_eq!(
            got,
            [
                (h / 3600 - 1, coverage::CRAWL, 10),
                (h / 3600, coverage::CRAWL, 20)
            ]
        );
        assert_eq!(coverage_rows(h - 10, h + 20, true, true).len(), 4);
        assert!(coverage_rows(h, h, true, true).is_empty());
        // Paused: nothing is measured, so the trend shows a gap.
        assert!(coverage_rows(h, h + 5, true, false).is_empty());
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
        // After the pipeline's, the store's import annotations (IMP-5).
        for (kind, name) in [
            (lodestar_store::import::event::STARTED, "import_started"),
            (lodestar_store::import::event::FINISHED, "import_finished"),
            (lodestar_store::import::event::REMOVED, "import_removed"),
        ] {
            assert!(kind > event::INTENSITY);
            assert_eq!(codes::event_name(kind), Some(name));
        }
        assert_eq!(codes::EVENTS.len(), 8);
        assert_eq!((codes::PAUSE, codes::RESUME), (event::PAUSE, event::RESUME));
        assert_eq!(codes::metric::STORED, metric::STORED);
        assert_eq!(codes::metric::NEWLY_SEARCHABLE, metric::NEWLY_SEARCHABLE);
        assert_eq!(
            codes::metric::TIME_TO_SEARCHABLE,
            metric::TIME_TO_SEARCHABLE
        );
        assert_eq!(codes::metric::COVERAGE, metric::COVERAGE);
        assert_eq!(codes::metric::SEARCHABLE, metric::SEARCHABLE);
        assert_eq!(codes::metric::COVERAGE_INDEX, coverage::INDEX);
        assert_eq!(codes::metric::COVERAGE_CRAWL, coverage::CRAWL);
        assert_eq!(codes::metric::FUNNEL_EVENTS, metric::FUNNEL_EVENTS);
        assert_eq!(codes::metric::REMOVED, metric::REMOVED);
        assert_eq!(codes::metric::HARVESTED, metric::HARVESTED);
        assert_eq!(codes::metric::REDISCOVERED, metric::REDISCOVERED);
        assert_eq!(codes::metric::DROPPED, metric::DROPPED);
        assert_eq!(codes::metric::BY_WORKER, metric::BY_WORKER);
        assert_eq!(codes::metric::WORKER_LEASES, metric::WORKER_LEASES);
        {
            use lodestar_pipeline::marginal::key;
            assert_eq!(
                [
                    codes::WORKER_GRANTED,
                    codes::WORKER_MARGINAL,
                    codes::WORKER_COMMITTED,
                    codes::WORKER_LOST,
                    codes::WORKER_REDISCOVERED,
                    codes::WORKER_DROPPED
                ],
                [
                    key::GRANTED,
                    key::MARGINAL,
                    key::COMMITTED,
                    key::LOST,
                    key::REDISCOVERED,
                    key::DROPPED
                ]
            );
        }
        assert_eq!(codes::BLOCKED, Loss::Blocked as u8);
        use lodestar_pipeline::strategy;
        for (code, name) in [
            (strategy::SAMPLE, "sample"),
            (strategy::GET_PEERS, "get_peers"),
            (strategy::ANNOUNCE, "announce"),
            (strategy::LOOKUP, "lookup"),
            (strategy::REFETCH, "refetch"),
        ] {
            assert_eq!(codes::strategy_name(i64::from(code)), Some(name));
        }
        assert_eq!(codes::STRATEGIES.len(), 5);
    }

    /// The store's delete trigger writes `metric::REMOVED` and
    /// `UNCLASSIFIED` as literals (V11): codes are persisted.
    #[test]
    fn the_removal_trigger_writes_the_pipelines_code() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let conn = store.connection();
        conn.execute_batch(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count, file_count,
                 info_size, flags, discovered_at, metadata_at, last_seen_at, content_type)
             VALUES (1, randomblob(20), 'a', 0, 0, 0, 0, 0, 0, 0, 0, 0, NULL);",
        )
        .unwrap();
        store.record_index_watermark(1, 1).unwrap();
        store
            .connection()
            .execute_batch("DELETE FROM torrents WHERE id = 1")
            .unwrap();
        let (m, dim): (u32, u32) = store
            .connection()
            .query_row("SELECT metric, dim FROM rollup_hour", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((m, dim), (metric::REMOVED, lodestar_store::UNCLASSIFIED));
        assert_eq!(
            metric::aggregation(metric::REMOVED),
            Some(lodestar_store::Aggregation::Sum)
        );
    }

    /// The repair runs at start only after a start with no stop after it,
    /// and only over the last 72 h.
    #[test]
    fn the_automatic_repair_runs_only_after_an_unclean_stop() {
        use lodestar_pipeline::event;
        use lodestar_store::{Event, WriteBatch};
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let now = crate::unix_now();
        // Two torrents, one stored 10 days ago and one an hour ago, and no
        // rollups: what a crash before the flush leaves.
        store
            .connection()
            .execute_batch(&format!(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                     first_source)
                 VALUES (1, randomblob(20), 'a', 0, 0, 0, 0, 0, 0, {old}, {old}, {old}, 0),
                        (2, randomblob(20), 'b', 0, 0, 0, 0, 0, 0, {new}, {new}, {new}, 0);",
                old = now - 10 * 86_400,
                new = now - 3600,
            ))
            .unwrap();
        assert_eq!(
            repair_after_unclean_stop(&mut store).unwrap(),
            None,
            "no run yet"
        );
        let event = |kind| Event {
            at: now - 60,
            kind,
            detail: String::new(),
        };
        for kind in [event::START, event::STOP] {
            store
                .write(&WriteBatch {
                    events: &[event(kind)],
                    ..WriteBatch::default()
                })
                .unwrap();
        }
        assert_eq!(
            repair_after_unclean_stop(&mut store).unwrap(),
            None,
            "a clean stop"
        );
        store
            .write(&WriteBatch {
                events: &[event(event::START)],
                ..WriteBatch::default()
            })
            .unwrap();
        let r = repair_after_unclean_stop(&mut store).unwrap().unwrap();
        assert_eq!((r.cohorts_stored, r.hours_stored), (1, 1), "{r:?}");
        assert_eq!(r.scanned, 1, "only ids stored in the last 72 h are read");
        let stored: i64 = store
            .connection()
            .query_row(
                "SELECT coalesce(sum(count), 0) FROM cohort_hour WHERE stage = ?1",
                [Stage::Stored as u8],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, 1, "only the torrent from the last 72 h");

        // Power lost 5 days ago, started now: the window ends at the
        // store's last activity, not at now.
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let crash = now - 5 * 86_400;
        store
            .connection()
            .execute_batch(&format!(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                     first_source)
                 VALUES (1, randomblob(20), 'a', 0, 0, 0, 0, 0, 0, {t}, {t}, {t}, 0);",
                t = crash - 600,
            ))
            .unwrap();
        store
            .write(&WriteBatch {
                events: &[Event {
                    at: crash - 86_400,
                    kind: event::START,
                    detail: String::new(),
                }],
                ..WriteBatch::default()
            })
            .unwrap();
        let r = repair_after_unclean_stop(&mut store).unwrap().unwrap();
        assert_eq!((r.scanned, r.cohorts_stored), (1, 1), "{r:?}");
    }

    /// The recorder compacts once its first delay has passed, and the run
    /// leaves the hourly rows of the last 30 days alone.
    #[tokio::test]
    async fn the_recorder_compacts_old_hours() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let now_h = crate::unix_now().div_euclid(3600);
        {
            let mut store = Store::open(&db, StoreConfig::default()).unwrap();
            let rows: Vec<Counter> = [now_h - 40 * 24, now_h - 1]
                .into_iter()
                .map(|hour| Counter {
                    hour,
                    metric: metric::STORED,
                    dim: 0,
                    key: 0,
                    value: 3,
                })
                .collect();
            store.add_counters(&rows).unwrap();
            // A search logged 40 days ago, and one today: the daily job
            // prunes the old one (§10.9.5, 30 days).
            let now = crate::unix_now();
            let entry = |at| lodestar_store::QueryLogEntry {
                at,
                query: "ubuntu",
                results: Some(1),
                latency_us: 10,
            };
            store
                .log_queries(
                    lodestar_store::QueryLogging::On,
                    &[entry(now - 40 * 86_400), entry(now)],
                )
                .unwrap();
        }
        let mut insights = Insights::with_clock(Box::new(|| 0), 1);
        insights.compact_after = Duration::ZERO;
        let insights = Arc::new(insights);
        let (stop_tx, stop) = watch::channel(false);
        let task = spawn(
            insights.clone(),
            Follow {
                db: db.clone(),
                store: StoreConfig::default(),
                index: None,
                paused: Box::new(|| false),
                searchable: Box::new(|| None),
                tick: Duration::from_millis(50),
            },
            stop,
        );
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let start = std::time::Instant::now();
        while reader.insights_state().unwrap().last_compaction_at == 0 {
            assert!(start.elapsed() < Duration::from_secs(10), "no compaction");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        let day = (now_h - 40 * 24).div_euclid(24);
        let daily = reader
            .rollup_day_sums(
                metric::STORED,
                day,
                day + 1,
                lodestar_store::RollupBy::Total,
            )
            .unwrap();
        assert_eq!(daily[0].value, 3);
        let hourly = reader
            .rollup_sums(
                metric::STORED,
                now_h - 41 * 24,
                now_h,
                lodestar_store::RollupBy::Hour,
            )
            .unwrap();
        assert_eq!(hourly.len(), 1, "the last 30 days stay hourly");
        assert_eq!(reader.query_log_len().unwrap(), 1, "pruned to 30 days");
        assert_eq!(insights.imported(), Some(0));
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
            searchable: Box::new(|| Some(7)),
            tick: TICK,
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

    /// Imports in `indexing` complete when the watermark passes their
    /// highest new id, not before: one with no new rows at start, another
    /// at the commit that reaches it. A reader opened before sees each
    /// change (`lodestar import status` while the server runs). One still
    /// waiting at shutdown stays in `indexing` for the next start.
    #[tokio::test]
    async fn the_recorder_completes_indexed_imports() {
        use lodestar_store::import::{NewImportJob, Phase};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let ids: Vec<i64> = {
            let mut store = Store::open(&db, StoreConfig::default()).unwrap();
            [(b"ds-empty", 0), (b"ds-first", 5), (b"ds-later", 1000)]
                .into_iter()
                .map(|(dataset, max_new_id)| {
                    let j = store
                        .create_import_job(&NewImportJob {
                            adapter: "magnetico",
                            adapter_ver: 1,
                            dataset: dataset.as_slice(),
                            schema_sig: b"sig",
                            options: b"{}",
                            credential: None,
                            at: 1,
                        })
                        .unwrap();
                    let j = store
                        .set_import_phase(j.id, j.revision, Phase::Indexing, 1)
                        .unwrap();
                    // The rows a job inserted end at its highest new id.
                    store
                        .connection()
                        .execute(
                            "UPDATE import_jobs SET max_new_id = ?1 WHERE id = ?2",
                            [max_new_id, j.id],
                        )
                        .unwrap();
                    j.id
                })
                .collect()
        };
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let phase = |id: i64| reader.import_job(id).unwrap().unwrap().phase;
        let hook = Arc::new(CommitHook::default());
        let stats = Arc::new(IndexStats::default());
        let insights = Arc::new(Insights::with_clock(Box::new(|| 0), 1));
        let (stop_tx, stop) = watch::channel(false);
        let (tx, rx) = mpsc::channel(QUEUE);
        let follow = Follow {
            db: db.clone(),
            store: StoreConfig::default(),
            index: Some((hook, stats)),
            paused: Box::new(|| false),
            searchable: Box::new(|| None),
            tick: TICK,
        };
        let task = {
            let insights = insights.clone();
            tokio::spawn(async move { run(&insights, follow, rx, stop).await.unwrap() })
        };
        let start = std::time::Instant::now();
        let until = |done: &dyn Fn() -> bool| {
            let phases: Vec<_> = ids.iter().map(|&id| phase(id)).collect();
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out: phases {phases:?}, recorded {}",
                insights.last_recorded()
            );
            !done()
        };
        while until(&|| phase(ids[0]) == Phase::Complete) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let commit = |max_torrent_id| Committed {
            docs_added: 1,
            adult_added: 0,
            max_torrent_id,
            at: crate::unix_now() as u64,
        };
        let before = insights.last_recorded();
        tx.send(commit(4)).await.unwrap();
        while until(&|| insights.last_recorded() != before) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(phase(ids[1]), Phase::Indexing, "not before its last id");
        tx.send(commit(5)).await.unwrap();
        while until(&|| phase(ids[1]) == Phase::Complete) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(phase(ids[2]), Phase::Indexing, "still waiting");
    }

    /// With search off the recorder keeps running and records crawl
    /// coverage (nothing ever sends it a commit).
    #[tokio::test]
    async fn without_search_it_records_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        let insights = Arc::new(Insights::with_clock(Box::new(|| 0), 1));
        let (stop_tx, stop) = watch::channel(false);
        let task = spawn(
            insights.clone(),
            Follow {
                db: db.clone(),
                store: StoreConfig::default(),
                index: None,
                paused: Box::new(|| false),
                searchable: Box::new(|| None),
                tick: Duration::from_millis(50),
            },
            stop,
        );
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let start = std::time::Instant::now();
        let crawl = || {
            let h = crate::unix_now() / 3600;
            reader
                .rollup_sums(
                    metric::COVERAGE,
                    h - 1,
                    h + 1,
                    lodestar_store::RollupBy::Dim,
                )
                .unwrap()
        };
        loop {
            let rows = crawl();
            if rows.iter().any(|c| c.dim == coverage::CRAWL && c.value > 0) {
                assert!(
                    rows.iter().all(|c| c.dim != coverage::INDEX),
                    "search is off"
                );
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "no coverage");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!task.is_finished(), "the recorder keeps running");
        stop_tx.send(true).unwrap();
        task.await.unwrap();
    }
}
