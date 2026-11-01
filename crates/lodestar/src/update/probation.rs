//! The child's side (`serve --child`, §12.2).
//!
//! - **Before `Services::start`** (so before the registry's first
//!   `Store::open`, where migrations run): if this version is in
//!   probation and the store's schema is older than this build's, it
//!   writes `<version>/migration.json` (or exits, if it can't), starts the
//!   heartbeat (every 10 s) and marks `probation.json` `migrating`.
//! - **After it,** `starting`, with `baseline_max_id`; then the self-test:
//!   the store opened with its migrations complete, the search index
//!   answers a query, the HTTP port is bound (and the DHT's unless it's
//!   off), and `/healthz` is green. Then `pending` and `probation.json` go.
//! - **Lost torrents:** `stored_since` (torrents stored after the
//!   baseline) is updated every 60 s and at shutdown, so a rollback can
//!   report what a restore loses; the supervisor never opens the store.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lodestar_log::{Count, log_event};

use super::control::{self, Hint};
use super::layout::{self, Bin};
use super::log_events::{
    UPDATE_MIGRATING, UPDATE_MIGRATION_UNRECORDED, UPDATE_PROBATION_FAILED, UPDATE_PROBATION_PASSED,
};
use super::pending::{self, Migration, Phase, Probation};
use crate::cli::util;

const COUNT_EVERY: Duration = Duration::from_secs(60);

/// What the child knows of its supervisor, for the updater (parts 4 and 5).
#[derive(Debug, Clone)]
pub struct Child {
    pub bin: Bin,
    pub hint: Option<Hint>,
    /// The parent is the supervisor named in the hint.
    pub supervised: bool,
    pub version: String,
    db: PathBuf,
    in_probation: bool,
    beat: Arc<Mutex<Option<Beat>>>,
}

/// The heartbeat thread and its stop flag.
type Beat = (Arc<AtomicBool>, std::thread::JoinHandle<()>);

static CHILD: OnceLock<Child> = OnceLock::new();
static FAILED: AtomicBool = AtomicBool::new(false);
static PASSED: AtomicBool = AtomicBool::new(false);
static PASSED_NOW: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Waits until this version's self-test has passed (for the update
/// engine's `done` event). Never returns when it fails.
pub async fn passed() {
    loop {
        let now = PASSED_NOW.notified();
        if PASSED.load(Ordering::SeqCst) {
            return;
        }
        now.await;
    }
}

/// This process's child state, once `serve --child` set it up.
pub fn current() -> Option<&'static Child> {
    CHILD.get()
}

/// The self-test failed: `serve` exits 1 after its graceful shutdown.
pub fn failed() -> bool {
    FAILED.load(Ordering::SeqCst)
}

/// What the self-test probes, from `Services::probe()`.
#[derive(Clone)]
pub struct Probe {
    pub http: SocketAddr,
    /// `None` with the DHT off; `Some(bound)` otherwise.
    pub dht: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub search: Option<lodestar_search::SearchHandle>,
}

/// Before `Services::start`. Refuses (an error) when `<data>/bin` can't be
/// used, when the supervisor is already gone, or when a migration can't be
/// recorded.
pub fn before_start(data_dir: &Path, db: &Path) -> Result<&'static Child, String> {
    let bin = Bin::open(data_dir).map_err(|e| e.to_string())?;
    let hint = std::env::var(control::ENV)
        .ok()
        .and_then(|t| Hint::decode(&t));
    let parent = || rustix::process::getppid().map(|p| p.as_raw_nonzero().get() as u32);
    let supervised = hint.as_ref().is_some_and(|h| Some(h.pid) == parent());
    // Stop with the supervisor: SIGTERM when it dies, then a recheck, in
    // case it died before the signal was set.
    #[cfg(target_os = "linux")]
    if supervised {
        let _ =
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM));
        if hint.as_ref().map(|h| h.pid) != parent() {
            return Err("the supervisor is gone".into());
        }
    }
    // A release child is its own build's version; debug builds take the
    // supervisor's word, so the tests can run copies of one binary as
    // several versions.
    let version = match &hint {
        Some(h) if cfg!(debug_assertions) => h.version.clone(),
        _ => super::build_version().to_string(),
    };
    let fault = super::testing::child_fault(&version);
    match fault {
        Some("crash") => std::process::exit(1),
        Some("hang") => hang(),
        Some("ignoreterm") => ignore_term(),
        Some("orphan") => {
            // A grandchild left behind for the supervisor to reap.
            let _ = std::process::Command::new("sh")
                .args(["-c", "sleep 2 &"])
                .spawn();
        }
        _ => {}
    }
    let pending = pending::read(&bin).ok().flatten();
    let in_probation = supervised && pending.is_some_and(|p| p.version == version);
    let child = Child {
        bin,
        hint,
        supervised,
        version,
        db: db.to_path_buf(),
        in_probation,
        beat: Arc::new(Mutex::new(None)),
    };
    if child.in_probation
        && let Err(e) = child.mark_migrating(fault)
    {
        log_event!(
            UPDATE_MIGRATION_UNRECORDED,
            error = lodestar_log::Error::io(&e)
        );
        return Err(format!("recording the store upgrade (migration.json): {e}"));
    }
    Ok(CHILD.get_or_init(|| child))
}

/// Catches SIGTERM and never acts on it, then hangs: the supervisor has
/// to escalate.
fn ignore_term() -> ! {
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        let caught = rt.block_on(async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        });
        std::mem::forget(caught);
        std::mem::forget(rt);
    }
    hang()
}

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// `probation.json` has two writers in this process (the heartbeat
/// thread and the rest); each read-modify-write holds this.
static PROBATION: Mutex<()> = Mutex::new(());

fn with_probation<T>(f: impl FnOnce() -> T) -> T {
    let _held = PROBATION.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

impl Child {
    fn run_id(&self) -> &str {
        self.hint.as_ref().map_or("", |h| h.run_id.as_str())
    }

    /// The schema is older than this build's: record the migration (with
    /// the snapshots that exist before it) and beat while it runs. If the
    /// record can't be made, nothing migrates: an error.
    fn mark_migrating(&self, fault: Option<&str>) -> std::io::Result<()> {
        // No store yet: it's created at the latest schema, nothing to
        // migrate. One whose schema can't be read may need migrating, so
        // nothing starts unrecorded.
        let from = match std::fs::symlink_metadata(&self.db) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            _ => {
                let conn = util::open_read_only(&self.db).map_err(|e| {
                    std::io::Error::other(format!("reading the store's schema: {e}"))
                })?;
                Some(util::schema_version(&conn).map_err(|e| {
                    std::io::Error::other(format!("reading the store's schema: {e}"))
                })?)
            }
        };
        let to = lodestar_store::latest_schema_version();
        let forced =
            fault.is_some_and(|f| f == "stall" || f == "unrecorded" || f.starts_with("migrate="));
        let Some(from) = from.filter(|f| (*f > 0 && *f < to) || forced) else {
            return Ok(());
        };
        let now = crate::unix_now().max(0) as u64;
        if let Some(v) = layout::parse_version(&self.version) {
            let dir = self.bin.overlay(&v);
            if dir.is_dir() && std::fs::symlink_metadata(dir.join("migration.json")).is_err() {
                let before = lodestar_store::snapshots::list(&self.db)?
                    .into_iter()
                    .filter(|s| {
                        matches!(s.kind, lodestar_store::snapshots::Kind::PreMigration { .. })
                    })
                    .map(|s| s.name)
                    .collect();
                let m = Migration {
                    from,
                    to,
                    started_at: now,
                    before,
                };
                let bytes = serde_json::to_vec_pretty(&m).map_err(std::io::Error::other)?;
                if super::testing::child_fault(&self.version) == Some("unrecorded") {
                    return Err(std::io::Error::other("test fault"));
                }
                layout::write_atomic(&dir, "migration.json", &bytes)?;
            }
        }
        log_event!(
            UPDATE_MIGRATING,
            from = Count(from as u64),
            to = Count(to as u64)
        );
        let p = Probation {
            run_id: self.run_id().to_string(),
            version: self.version.clone(),
            phase: Phase::Migrating,
            at: now,
            since: now,
            from,
            to,
            baseline_max_id: None,
            stored_since: None,
        };
        with_probation(|| self.bin.write("probation.json", &p).ok());
        if fault == Some("stall") {
            hang();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (bin, flag, beat) = (self.bin.clone(), stop.clone(), super::testing::beat());
        let handle = std::thread::spawn(move || {
            let mut last = Instant::now();
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
                if last.elapsed() < beat {
                    continue;
                }
                last = Instant::now();
                with_probation(|| {
                    if let Ok(Some(mut p)) = bin.read::<Probation>("probation.json")
                        && p.phase == Phase::Migrating
                    {
                        p.at = crate::unix_now().max(0) as u64;
                        let _ = bin.write("probation.json", &p);
                    }
                });
            }
        });
        *self.beat.lock().unwrap_or_else(|e| e.into_inner()) = Some((stop, handle));
        if let Some(secs) = fault.and_then(|f| f.strip_prefix("migrate=")?.parse().ok()) {
            std::thread::sleep(Duration::from_secs(secs));
        }
        Ok(())
    }

    /// Stops the heartbeat and waits for it, so no beat lands after.
    fn stop_beating(&self) {
        let taken = self.beat.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((stop, handle)) = taken {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
    }

    /// After `Services::start`: the self-test, in the background while
    /// the server serves. A failure asks for the graceful shutdown, and
    /// `serve` then exits 1.
    pub fn after_start(&'static self, probe: Probe, deadline: Duration) {
        self.stop_beating();
        let fault = super::testing::child_fault(&self.version);
        if fault == Some("restart") {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                crate::system::Restart::global().request();
            });
        }
        if !self.in_probation {
            if let Some(v) = fault.and_then(|f| f.strip_prefix("switch=")) {
                control::set_intent(control::Action::Switch, Some(v.to_string()), None);
                crate::system::Restart::global().request();
            }
            if fault == Some("rollback") {
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    control::request_rollback(None);
                });
            }
            return;
        }
        // The first attempt's baseline counts: a later attempt's would
        // miss what earlier ones stored.
        with_probation(|| {
            let earlier: Option<Probation> = self.bin.read("probation.json").ok().flatten();
            let baseline = earlier
                .filter(|p| p.version == self.version)
                .and_then(|p| p.baseline_max_id)
                .or_else(|| max_id(&self.db));
            let to = lodestar_store::latest_schema_version();
            let p = Probation {
                run_id: self.run_id().to_string(),
                version: self.version.clone(),
                phase: Phase::Starting,
                at: crate::unix_now().max(0) as u64,
                since: 0,
                from: to,
                to,
                baseline_max_id: baseline,
                stored_since: None,
            };
            let _ = self.bin.write("probation.json", &p);
        });
        self.record_stored();
        if fault == Some("restart") {
            // Restarts until stopped, never confirming: the test counts
            // the attempts it uses (none).
            return;
        }
        tokio::spawn(async move {
            let result = match fault {
                Some("selftest") => Err("test fault"),
                _ => self_test(&probe, deadline).await,
            };
            match result {
                Ok(()) => {
                    with_probation(|| {
                        // An install that passed is history (a restored
                        // version's rollback recorded its own entry).
                        if let Ok(Some(p)) = pending::read(&self.bin)
                            && p.kind == pending::Kind::Install
                            && p.version == self.version
                        {
                            let _ = super::history::append(
                                &self.bin,
                                super::history::Entry {
                                    version: p.version,
                                    from: p.previous,
                                    level: None,
                                    at: crate::unix_now().max(0) as u64,
                                    result: super::history::Outcome::Installed,
                                    reason: None,
                                    lost_torrents: None,
                                    journal: None,
                                },
                            );
                        }
                        let _ = self.bin.remove("pending");
                        let _ = self.bin.remove("probation.json");
                    });
                    PASSED.store(true, Ordering::SeqCst);
                    PASSED_NOW.notify_waiters();
                    log_event!(UPDATE_PROBATION_PASSED);
                }
                Err(check) => {
                    log_event!(UPDATE_PROBATION_FAILED, check = check);
                    FAILED.store(true, Ordering::SeqCst);
                    crate::system::Restart::global().request();
                }
            }
        });
        tokio::spawn(async move {
            let mut every = tokio::time::interval(COUNT_EVERY);
            every.tick().await;
            loop {
                every.tick().await;
                if !self.record_stored() {
                    return;
                }
            }
        });
    }

    /// Updates `stored_since` while probation lasts; `false` once it's over.
    pub fn record_stored(&self) -> bool {
        with_probation(|| {
            let Ok(Some(mut p)) = self.bin.read::<Probation>("probation.json") else {
                return false;
            };
            if p.run_id != self.run_id() {
                return false;
            }
            if let (Some(base), Some(now)) = (p.baseline_max_id, max_id(&self.db)) {
                p.stored_since = Some(now.saturating_sub(base).max(0) as u64);
                let _ = self.bin.write("probation.json", &p);
            }
            true
        })
    }

    /// What a rollback requested now would lose: torrents stored since
    /// the update, when this version migrated the store.
    pub fn lost_torrents(&self) -> Option<u64> {
        self.bin
            .read::<Probation>("probation.json")
            .ok()
            .flatten()
            .and_then(|p| p.stored_since)
    }
}

/// The self-test's checks, retried until `deadline`. The failing check's
/// name on error.
async fn self_test(probe: &Probe, deadline: Duration) -> Result<(), &'static str> {
    let until = Instant::now() + deadline;
    let mut last = "the HTTP port";
    while Instant::now() < until {
        match check_once(probe).await {
            Ok(()) => return Ok(()),
            Err(what) => last = what,
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last)
}

async fn check_once(probe: &Probe) -> Result<(), &'static str> {
    if let Some(search) = &probe.search {
        let index = search.index();
        let node = lodestar_search::query::parse("lodestar").map_err(|_| "the search index")?;
        let compiled = index.query(&node, crate::unix_now());
        index.search(&compiled, 1).map_err(|_| "the search index")?;
    }
    if probe.dht.as_ref().is_some_and(|bound| !bound()) {
        return Err("the DHT port");
    }
    let addr = probe.http;
    tokio::task::spawn_blocking(move || crate::healthcheck(addr))
        .await
        .map_err(|_| "/healthz")?
        .map_err(|_| "/healthz")
}

/// The highest torrent id, read on a connection of its own.
fn max_id(db: &Path) -> Option<i64> {
    let conn = util::open_read_only(db).ok()?;
    conn.query_row("SELECT coalesce(max(id), 0) FROM torrents", [], |r| {
        r.get(0)
    })
    .ok()
}
