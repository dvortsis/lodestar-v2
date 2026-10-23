//! The child's side (`serve --child`, §12.2).
//!
//! - **Before `Services::start`** (so before the registry's first
//!   `Store::open`, where migrations run): if this version is in
//!   probation and the store's schema is older than this build's, it
//!   writes `<version>/migration.json`, starts the heartbeat (every 10 s)
//!   and marks `probation.json` `migrating`.
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
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lodestar_log::{Count, log_event};

use super::control::{self, Hint};
use super::layout::{self, Bin};
use super::log_events::{UPDATE_MIGRATING, UPDATE_PROBATION_FAILED, UPDATE_PROBATION_PASSED};
use super::pending::{Migration, Pending, Phase, Probation};
use crate::cli::util;

const BEAT: Duration = Duration::from_secs(10);
const COUNT_EVERY: Duration = Duration::from_secs(60);

/// What the child knows of its supervisor, for the updater (part 3).
#[derive(Debug, Clone)]
pub struct Child {
    pub bin: Bin,
    pub hint: Option<Hint>,
    /// The parent is the supervisor named in the hint.
    pub supervised: bool,
    pub version: String,
    db: PathBuf,
    in_probation: bool,
    beating: Arc<AtomicBool>,
}

static CHILD: OnceLock<Child> = OnceLock::new();
static FAILED: AtomicBool = AtomicBool::new(false);

/// This process's child state, once `serve --child` set it up.
#[allow(dead_code)] // The updater's (part 3).
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

/// Before `Services::start`. Refuses (an error) only when `<data>/bin`
/// can't be used.
pub fn before_start(data_dir: &Path, db: &Path) -> Result<&'static Child, String> {
    let bin = Bin::open(data_dir).map_err(|e| e.to_string())?;
    let hint = std::env::var(control::ENV)
        .ok()
        .and_then(|t| Hint::decode(&t));
    let parent = rustix::process::getppid().map(|p| p.as_raw_nonzero().get() as u32);
    let supervised = hint.as_ref().is_some_and(|h| Some(h.pid) == parent);
    // A release child is its own build's version; debug builds take the
    // supervisor's word, so the tests can run copies of one binary as
    // several versions.
    let version = match &hint {
        Some(h) if cfg!(debug_assertions) => h.version.clone(),
        _ => super::build_version().to_string(),
    };
    match super::testing::child_fault(&version) {
        Some("crash") => std::process::exit(1),
        Some("hang") => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => {}
    }
    let pending: Option<Pending> = bin.read("pending").ok().flatten();
    let in_probation = supervised && pending.is_some_and(|p| p.version == version);
    let child = Child {
        bin,
        hint,
        supervised,
        version,
        db: db.to_path_buf(),
        in_probation,
        beating: Arc::new(AtomicBool::new(false)),
    };
    if child.in_probation {
        child.mark_migrating();
    }
    Ok(CHILD.get_or_init(|| child))
}

impl Child {
    fn run_id(&self) -> &str {
        self.hint.as_ref().map_or("", |h| h.run_id.as_str())
    }

    /// The schema is older than this build's: record the migration and
    /// beat while it runs.
    fn mark_migrating(&self) {
        let from = util::open_read_only(&self.db)
            .ok()
            .and_then(|c| util::schema_version(&c).ok());
        let to = lodestar_store::latest_schema_version();
        let Some(from) = from.filter(|f| *f > 0 && *f < to) else {
            return;
        };
        let now = crate::unix_now().max(0) as u64;
        if let Some(v) = layout::parse_version(&self.version) {
            let dir = self.bin.overlay(&v);
            if dir.is_dir() && !dir.join("migration.json").exists() {
                let m = Migration {
                    from,
                    to,
                    started_at: now,
                };
                let bytes = serde_json::to_vec_pretty(&m).unwrap_or_default();
                if let Err(e) = layout::write_atomic(&dir, "migration.json", &bytes) {
                    eprintln!("update: writing migration.json: {e}");
                }
            }
        }
        log_event!(
            UPDATE_MIGRATING,
            from = Count(from as u64),
            to = Count(to as u64)
        );
        let p = Probation {
            run_id: self.run_id().to_string(),
            phase: Phase::Migrating,
            at: now,
            since: now,
            from,
            to,
            baseline_max_id: None,
            stored_since: None,
        };
        let _ = self.bin.write("probation.json", &p);
        self.beating.store(true, Ordering::SeqCst);
        let (bin, beating) = (self.bin.clone(), self.beating.clone());
        std::thread::spawn(move || {
            while beating.load(Ordering::SeqCst) {
                std::thread::sleep(BEAT);
                if !beating.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(Some(mut p)) = bin.read::<Probation>("probation.json") {
                    if p.phase == Phase::Migrating {
                        p.at = crate::unix_now().max(0) as u64;
                        let _ = bin.write("probation.json", &p);
                    }
                }
            }
        });
    }

    /// After `Services::start`: the self-test, in the background while
    /// the server serves. A failure asks for the graceful shutdown, and
    /// `serve` then exits 1.
    pub fn after_start(&'static self, probe: Probe, deadline: Duration) {
        self.beating.store(false, Ordering::SeqCst);
        if !self.in_probation {
            return;
        }
        let baseline = max_id(&self.db);
        let to = lodestar_store::latest_schema_version();
        let p = Probation {
            run_id: self.run_id().to_string(),
            phase: Phase::Starting,
            at: crate::unix_now().max(0) as u64,
            since: 0,
            from: to,
            to,
            baseline_max_id: baseline,
            stored_since: baseline.map(|_| 0),
        };
        let _ = self.bin.write("probation.json", &p);
        tokio::spawn(async move {
            let started = Instant::now();
            let result = match super::testing::child_fault(&self.version) {
                Some("selftest") => Err("test fault"),
                _ => self_test(&probe, deadline).await,
            };
            match result {
                Ok(()) => {
                    let _ = self.bin.remove("pending");
                    let _ = self.bin.remove("probation.json");
                    log_event!(UPDATE_PROBATION_PASSED);
                    // Keep counting for a rollback the user asks for later.
                    let _ = started;
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
    }

    /// What a rollback requested now would lose: torrents stored since
    /// the update, when this version migrated the store.
    #[allow(dead_code)] // The updater's (part 3).
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
