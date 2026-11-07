//! The cluster port in `lodestar serve` (architecture §19.2, §19.6, §12;
//! workers plan WK-1 and Q3).
//!
//! **Off until allowed.** [`Cluster::start`] binds nothing and opens nothing
//! while "Allow workers" (`[cluster] enabled`) is off: no listener, no
//! cluster CA, no `cluster.` rows in `settings`, no secrets key opened for
//! it. Turning it on ([`ClusterHandle::set_allowed`], or the config at
//! start):
//! 1. binds `[cluster] listen` (default `0.0.0.0:8091`) first, so a port
//!    that's taken creates nothing;
//! 2. opens the cluster PKI ([`Coordinator::open`]: the CA and the
//!    coordinator's certificate are created on first use) on its own store
//!    connection, with the install's `secrets.key`;
//! 3. builds the [`ClusterServer`] on the pipeline's one
//!    [`LeaseAuthority`] (WK-0), with a store check for claimed keys, and
//!    serves the port. The server's own heartbeat tick reloads the PKI,
//!    runs `maintain` hourly, sweeps revoked and silent sessions and
//!    re-grants expired leases.
//!
//! Turning it off stops every session (`Stop(Shutdown)`) and closes the
//! port. Leases remote workers held stay in the authority and expire as
//! any silent worker's do. Nothing already created is deleted: the CA and
//! its workers' certificates are kept for when it's on again.
//!
//! **At start** a failure to bind or to open the PKI fails `serve` with the
//! reason, as any other bad configuration does. A later change that fails
//! is logged (`cluster.start_failed`) and counted; serving goes on.
//!
//! **Secrets-key rotation** ([`ClusterHandle::reseal`]) goes between
//! `SecretStore::rotate` and `SecretStore::retire_old_keys`. The server is
//! stopped first, so its hourly upkeep can't write a key sealed with the
//! old secrets key meanwhile; then the CA, previous CA and the
//! coordinator's keys are sealed again with the new primary key, and the
//! server is rebuilt on the key file as it is now. `serve` has no rotation
//! flow yet; whichever slice adds one calls this.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lodestar_cluster::{
    Clock, ClusterError, ClusterServer, Coordinator, FleetSnapshot, LeaseAuthority, Pki,
    ServerConfig, ServerHooks, StoredKeys,
};
use lodestar_integrations::SecretStore;
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::remote::RemoteIntake;
use lodestar_store::{Store, StoreConfig};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::config;
use crate::logging::events;

/// The cluster PKI's rows in `settings` (`lodestar_cluster::Coordinator`).
const PKI_PREFIX: &str = "cluster.";

/// What the cluster port needs from the rest of `serve`.
#[derive(Clone)]
pub struct Wiring {
    /// `lodestar.db` and its settings: the PKI and the store check each
    /// open their own connection.
    pub db: PathBuf,
    pub store: StoreConfig,
    /// Holds `secrets.key`, which seals the cluster's private keys.
    pub data_dir: PathBuf,
    /// The pipeline's lease authority (`Monitor::lease_authority`).
    pub authority: Arc<LeaseAuthority>,
    /// The pipeline's intake for what workers send (`Monitor::remote`,
    /// WK-9). Without one, results and counters are refused.
    pub remote: Option<Arc<RemoteIntake>>,
    pub settings: config::Cluster,
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("listen", &self.settings.listen)
            .finish_non_exhaustive()
    }
}

/// The server configuration `[cluster]` gives (its numbers were checked at
/// load, `config::Cluster::check`). Heartbeats, lease TTL and the rest keep
/// the protocol's defaults.
pub fn server_config(c: &config::Cluster) -> ServerConfig {
    ServerConfig {
        max_skew_ms: c.max_skew_ms,
        handshake_timeout: Duration::from_secs(c.handshake_timeout_secs),
        max_pending: c.max_pending,
        max_pending_per_ip: c.max_pending_per_ip,
        session_queue_bytes: c.session_queue_mib << 20,
        ..ServerConfig::default()
    }
}

/// Why the cluster port couldn't open, for `serve`'s error and the log.
/// Its text never holds key material: `ClusterError`'s doesn't.
#[derive(Debug)]
pub struct StartError {
    /// `bind`, `store`, `secrets`, `pki` or `server`.
    step: &'static str,
    /// A fixed kind for the log line.
    kind: &'static str,
    detail: String,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cluster port ({}): {}", self.step, self.detail)
    }
}

impl std::error::Error for StartError {}

impl StartError {
    fn bind(listen: SocketAddr, e: &std::io::Error) -> StartError {
        StartError {
            step: "bind",
            kind: io_kind(e.kind()),
            detail: format!("binding cluster.listen {listen}: {e}"),
        }
    }

    fn pki(step: &'static str, e: &ClusterError) -> StartError {
        StartError {
            step,
            kind: cluster_kind(e),
            detail: e.to_string(),
        }
    }

    fn task() -> StartError {
        StartError {
            step: "pki",
            kind: "task_failed",
            detail: "the PKI task failed".into(),
        }
    }
}

fn io_kind(k: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match k {
        K::AddrInUse => "addr_in_use",
        K::AddrNotAvailable => "addr_not_available",
        K::PermissionDenied => "permission_denied",
        _ => "io",
    }
}

fn cluster_kind(e: &ClusterError) -> &'static str {
    match e {
        ClusterError::Store(_) => "store",
        ClusterError::Secret(_) => "secrets",
        ClusterError::Tls(_) | ClusterError::Verifier(_) => "tls",
        ClusterError::Io(..) => "io",
        ClusterError::Corrupt(_) => "corrupt",
        ClusterError::Random => "random",
        _ => "pki",
    }
}

/// Whether claimed keys are stored (§19.3's authoritative check before a
/// grant), on a read-only connection of its own. A read that fails counts
/// as not stored: the commit still refuses a duplicate, so the worst case
/// is one fetch too many, never a lost key.
struct StoreCheck {
    reader: Mutex<Store>,
    errors: Arc<AtomicU64>,
}

impl StoredKeys for StoreCheck {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        let reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        keys.iter()
            .map(|k| match reader.find(k) {
                Ok(found) => found.is_some(),
                Err(_) => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    false
                }
            })
            .collect()
    }
}

/// Opens the PKI (creating the CA on first use) and the store check's
/// connection. Blocking.
fn open_pki(w: &Wiring, errors: Arc<AtomicU64>) -> Result<(Pki, StoreCheck), StartError> {
    let store_err = |e: lodestar_store::Error| StartError::pki("store", &ClusterError::Store(e));
    let mut store = Store::open(&w.db, w.store.clone()).map_err(store_err)?;
    let secrets = SecretStore::open_or_create(&w.data_dir)
        .map_err(|e| StartError::pki("secrets", &ClusterError::Secret(e)))?;
    let coordinator = Coordinator::open(&mut store, &secrets, w.clock.now_secs())
        .map_err(|e| StartError::pki("pki", &e))?;
    let reader = Store::open_reader(&w.db, w.store.clone()).map_err(store_err)?;
    Ok((
        Pki {
            coordinator,
            store,
            secrets,
        },
        StoreCheck {
            reader: Mutex::new(reader),
            errors,
        },
    ))
}

/// Seals the cluster's keys again with the key file's primary key, if a
/// cluster PKI exists; `Ok(false)` (and nothing written) if it never did.
/// Blocking; no server may be running on this database (an offline
/// rotation, with `serve` stopped, can call it directly).
pub(crate) fn reseal_pki(w: &Wiring) -> Result<bool, ClusterError> {
    let mut store = Store::open(&w.db, w.store.clone())?;
    if store.settings_with_prefix(PKI_PREFIX)?.is_empty() {
        return Ok(false);
    }
    let secrets = SecretStore::open_or_create(&w.data_dir)?;
    let mut coordinator = Coordinator::open(&mut store, &secrets, w.clock.now_secs())?;
    coordinator.reseal(&mut store, &secrets)?;
    Ok(true)
}

/// The counters a server keeps, summed over every server this process
/// built (one per time "Allow workers" was turned on), so they never go
/// backwards.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    sessions: u64,
    refused_capacity: u64,
    refused_timeout: u64,
    refused_handshake: u64,
    join_requests: u64,
    granted: u64,
    regrants: u64,
    busy: u64,
    stale_reports: u64,
    revoked: u64,
    protocol_errors: u64,
    version_refused: u64,
    skew_flags: u64,
    pki_errors: u64,
}

impl Totals {
    fn of(s: &FleetSnapshot) -> Totals {
        Totals {
            sessions: s.sessions,
            refused_capacity: s.refused,
            refused_timeout: s.handshake_timeouts,
            refused_handshake: s.handshake_failures,
            join_requests: s.join_requests,
            granted: s.harvested,
            regrants: s.regrants,
            busy: s.busy,
            stale_reports: s.stale_reports,
            revoked: s.revoked,
            protocol_errors: s.protocol_errors,
            version_refused: s.version_refused,
            skew_flags: s.skew_flags,
            pki_errors: s.pki_errors,
        }
    }

    fn plus(self, o: Totals) -> Totals {
        Totals {
            sessions: self.sessions + o.sessions,
            refused_capacity: self.refused_capacity + o.refused_capacity,
            refused_timeout: self.refused_timeout + o.refused_timeout,
            refused_handshake: self.refused_handshake + o.refused_handshake,
            join_requests: self.join_requests + o.join_requests,
            granted: self.granted + o.granted,
            regrants: self.regrants + o.regrants,
            busy: self.busy + o.busy,
            stale_reports: self.stale_reports + o.stale_reports,
            revoked: self.revoked + o.revoked,
            protocol_errors: self.protocol_errors + o.protocol_errors,
            version_refused: self.version_refused + o.version_refused,
            skew_flags: self.skew_flags + o.skew_flags,
            pki_errors: self.pki_errors + o.pki_errors,
        }
    }
}

#[derive(Default)]
struct State {
    /// The server serving the port now, and where.
    current: Option<(Arc<ClusterServer>, SocketAddr)>,
    /// Counters of the servers already stopped.
    base: Totals,
}

type ResealReply = oneshot::Sender<Result<bool, ClusterError>>;

struct Inner {
    wiring: Wiring,
    allowed: watch::Sender<bool>,
    state: Mutex<State>,
    // Read by `ClusterHandle::reseal`, which the rotation flow calls.
    #[cfg_attr(not(test), allow(dead_code))]
    reseal: mpsc::Sender<ResealReply>,
    start_failures: AtomicU64,
    store_errors: Arc<AtomicU64>,
}

/// A running server and its `serve` task.
struct Running {
    server: Arc<ClusterServer>,
    task: JoinHandle<()>,
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn open(&self) -> Result<Running, StartError> {
        let listen = self.wiring.settings.listen;
        // Bound first: a port that's taken creates no CA.
        let listener = TcpListener::bind(listen)
            .await
            .map_err(|e| StartError::bind(listen, &e))?;
        let addr = listener
            .local_addr()
            .map_err(|e| StartError::bind(listen, &e))?;
        let (w, errors) = (self.wiring.clone(), self.store_errors.clone());
        let (pki, check) = tokio::task::spawn_blocking(move || open_pki(&w, errors))
            .await
            .map_err(|_| StartError::task())??;
        let check: Arc<dyn StoredKeys> = Arc::new(check);
        let config = server_config(&self.wiring.settings);
        // The intake and the writer's backpressure (WK-9).
        let attached = self
            .wiring
            .remote
            .as_ref()
            .map(|r| super::intake::attach(r, check.clone(), config.hints_allow_private));
        let hooks = ServerHooks {
            authority: self.wiring.authority.clone(),
            stored: Some(check),
            sink: attached.as_ref().map(|a| a.sink.clone() as _),
            // Grants held back while the writer is behind: WK-9b.
            admission: None,
        };
        let server = ClusterServer::with_hooks(pki, self.wiring.clock.clone(), config, hooks)
            .map_err(|e| StartError::pki("server", &e))?;
        if let (Some(a), Some(r)) = (&attached, &self.wiring.remote) {
            a.serving(r, &server);
        }
        let task = tokio::spawn(server.clone().serve(listener));
        self.state().current = Some((server.clone(), addr));
        lodestar_log::log_event!(
            events::CLUSTER_LISTENING,
            address = lodestar_log::Listen(addr)
        );
        Ok(Running { server, task })
    }

    async fn close(&self, running: Running, reason: &'static str) {
        if let Some(r) = &self.wiring.remote {
            r.serving(None, false);
        }
        running.server.shutdown();
        let _ = running.task.await;
        {
            let mut state = self.state();
            state.base = state.base.plus(Totals::of(&running.server.snapshot()));
            state.current = None;
        }
        lodestar_log::log_event!(events::CLUSTER_CLOSED, reason = reason);
    }

    /// Opens the port, logging and counting a failure.
    async fn try_open(&self) -> Option<Running> {
        match self.open().await {
            Ok(r) => Some(r),
            Err(e) => {
                self.failed(&e);
                None
            }
        }
    }

    fn failed(&self, e: &StartError) {
        self.start_failures.fetch_add(1, Ordering::Relaxed);
        lodestar_log::log_event!(
            events::CLUSTER_START_FAILED,
            step = e.step,
            error = lodestar_log::Error::kind(e.kind)
        );
    }
}

/// The cluster port's controls and metrics, for `serve`'s other parts.
#[derive(Clone)]
pub struct ClusterHandle(Arc<Inner>);

impl std::fmt::Debug for ClusterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterHandle")
            .field("allowed", &self.allowed())
            .field("listening", &self.local_addr())
            .finish()
    }
}

impl ClusterHandle {
    /// Turns "Allow workers" on or off; the port follows in the
    /// background. Not saved: the config (and, later, Settings →
    /// Workers) decides at the next start.
    // Settings → Workers calls it (WK-14); tests do today.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_allowed(&self, allowed: bool) {
        self.0.allowed.send_replace(allowed);
    }

    pub fn allowed(&self) -> bool {
        *self.0.allowed.borrow()
    }

    /// Where the cluster port is listening, while it is.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.state().current.as_ref().map(|(_, a)| *a)
    }

    /// The server serving the port now (for the workers API, WK-14).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn server(&self) -> Option<Arc<ClusterServer>> {
        self.0.state().current.as_ref().map(|(s, _)| s.clone())
    }

    /// Secrets-key rotation's cluster step, between `SecretStore::rotate`
    /// (which saved the new key to `secrets.key`) and
    /// `SecretStore::retire_old_keys`: stops the server, seals the
    /// cluster's keys again with the new primary key, and serves again if
    /// workers are allowed. `Ok(false)` if there's no cluster PKI to seal
    /// (nothing is created). Retire the old keys only after `Ok`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn reseal(&self) -> Result<bool, ClusterError> {
        let (reply, answer) = oneshot::channel();
        let stopped = || ClusterError::Corrupt("the cluster task has stopped");
        self.0.reseal.send(reply).await.map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    /// The `lodestar_cluster_*` series (docs/metrics.md, "Cluster port").
    pub fn render(&self, out: &mut String) {
        let inner = &self.0;
        let (snapshot, base) = {
            let state = inner.state();
            (
                state.current.as_ref().map(|(s, _)| s.snapshot()),
                state.base,
            )
        };
        let totals = snapshot.as_ref().map_or(base, |s| base.plus(Totals::of(s)));
        let mut o = Out(out);
        o.one(
            "cluster_allowed",
            "gauge",
            "1 when Allow workers is on.",
            u8::from(self.allowed()),
        );
        o.one(
            "cluster_listening",
            "gauge",
            "1 while the cluster port is bound and served.",
            u8::from(snapshot.is_some()),
        );
        let connected = snapshot
            .as_ref()
            .map_or(0, |s| s.workers.iter().filter(|w| w.connected).count());
        o.one(
            "cluster_workers_connected",
            "gauge",
            "Remote workers with a live session.",
            connected,
        );
        o.one(
            "cluster_handshaking",
            "gauge",
            "Connections still in their pre-authentication handshake.",
            snapshot.as_ref().map_or(0, |s| s.handshaking),
        );
        o.one(
            "cluster_leases_held",
            "gauge",
            "Leases held by remote workers.",
            snapshot
                .as_ref()
                .map_or(0, |s| s.workers.iter().map(|w| w.leases).sum::<usize>()),
        );
        o.one(
            "cluster_leases_waiting",
            "gauge",
            "Keys whose lease ended without an outcome, waiting to be granted again.",
            snapshot.as_ref().map_or(0, |s| s.pending),
        );
        o.one(
            "cluster_sessions_total",
            "counter",
            "Worker sessions started.",
            totals.sessions,
        );
        o.family(
            "cluster_preauth_refused_total",
            "counter",
            "Connections closed before authentication, by reason.",
        );
        for (reason, n) in [
            ("capacity", totals.refused_capacity),
            ("handshake_timeout", totals.refused_timeout),
            ("handshake_failed", totals.refused_handshake),
        ] {
            o.value(
                "cluster_preauth_refused_total",
                &format!("reason=\"{reason}\""),
                n,
            );
        }
        for (name, help, n) in [
            (
                "cluster_join_requests_total",
                "Join requests answered (granted or refused).",
                totals.join_requests,
            ),
            (
                "cluster_granted_total",
                "Keys granted to remote workers for the first time.",
                totals.granted,
            ),
            (
                "cluster_regrants_total",
                "Keys granted again after their lease ended without an outcome.",
                totals.regrants,
            ),
            (
                "cluster_busy_total",
                "Claimed keys denied because grants were held back.",
                totals.busy,
            ),
            (
                "cluster_stale_reports_total",
                "Reports with a stale fencing token, ignored.",
                totals.stale_reports,
            ),
            (
                "cluster_revoked_total",
                "Sessions closed because their certificate was revoked or expired.",
                totals.revoked,
            ),
            (
                "cluster_protocol_errors_total",
                "Sessions closed for a protocol error.",
                totals.protocol_errors,
            ),
            (
                "cluster_version_refused_total",
                "Workers refused for a protocol major version mismatch.",
                totals.version_refused,
            ),
            (
                "cluster_skew_flags_total",
                "Times a worker's clock was flagged as too far off.",
                totals.skew_flags,
            ),
            (
                "cluster_pki_errors_total",
                "Failed PKI reloads or upkeep on the heartbeat tick.",
                totals.pki_errors,
            ),
            (
                "cluster_start_failures_total",
                "Times turning the cluster port on failed (bind or PKI).",
                inner.start_failures.load(Ordering::Relaxed),
            ),
            (
                "cluster_store_check_errors_total",
                "Claimed keys whose stored check failed (granted; the commit refuses duplicates).",
                inner.store_errors.load(Ordering::Relaxed),
            ),
        ] {
            o.one(name, "counter", help, n);
        }
    }
}

struct Out<'a>(&'a mut String);

impl Out<'_> {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        use std::fmt::Write;
        let _ = writeln!(self.0, "# HELP lodestar_{name} {help}");
        let _ = writeln!(self.0, "# TYPE lodestar_{name} {kind}");
    }

    fn value(&mut self, name: &str, labels: &str, v: impl std::fmt::Display) {
        use std::fmt::Write;
        let _ = writeln!(self.0, "lodestar_{name}{{{labels}}} {v}");
    }

    fn one(&mut self, name: &str, kind: &str, help: &str, v: impl std::fmt::Display) {
        use std::fmt::Write;
        self.family(name, kind, help);
        let _ = writeln!(self.0, "lodestar_{name} {v}");
    }
}

/// The cluster port, following "Allow workers" until [`shutdown`](Self::shutdown).
pub struct Cluster {
    handle: ClusterHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.handle.fmt(f)
    }
}

impl Cluster {
    /// Starts following `[cluster] enabled`. If it's on, the port is bound
    /// and the PKI opened before this returns, and a failure is returned.
    /// If it's off, nothing is bound, opened or created.
    pub async fn start(wiring: Wiring) -> Result<Cluster, StartError> {
        let enabled = wiring.settings.enabled;
        let (allowed, follow) = watch::channel(enabled);
        let (reseal, reseals) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            wiring,
            allowed,
            state: Mutex::new(State::default()),
            reseal,
            start_failures: AtomicU64::new(0),
            store_errors: Arc::new(AtomicU64::new(0)),
        });
        let running = if enabled {
            match inner.open().await {
                Ok(r) => Some(r),
                Err(e) => {
                    inner.failed(&e);
                    return Err(e);
                }
            }
        } else {
            None
        };
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(run(inner.clone(), follow, reseals, stopped, running));
        Ok(Cluster {
            handle: ClusterHandle(inner),
            stop,
            task,
        })
    }

    pub fn handle(&self) -> ClusterHandle {
        self.handle.clone()
    }

    /// Stops every session and closes the port. Before the pipeline stops,
    /// so no remote work arrives while it saves.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

/// Follows "Allow workers" and reseal requests until `stopped`.
async fn run(
    inner: Arc<Inner>,
    mut allowed: watch::Receiver<bool>,
    mut reseals: mpsc::Receiver<ResealReply>,
    mut stopped: watch::Receiver<bool>,
    mut running: Option<Running>,
) {
    loop {
        tokio::select! {
            _ = stopped.changed() => break,
            changed = allowed.changed() => {
                if changed.is_err() {
                    break;
                }
                let want = *allowed.borrow_and_update();
                match (want, running.take()) {
                    (false, Some(r)) => inner.close(r, "workers_off").await,
                    (true, None) => running = inner.try_open().await,
                    (_, r) => running = r,
                }
            }
            Some(reply) = reseals.recv() => {
                if let Some(r) = running.take() {
                    inner.close(r, "reseal").await;
                }
                let w = inner.wiring.clone();
                let resealed = tokio::task::spawn_blocking(move || reseal_pki(&w))
                    .await
                    .unwrap_or(Err(ClusterError::Corrupt("the reseal task failed")));
                if *allowed.borrow_and_update() {
                    running = inner.try_open().await;
                }
                let _ = reply.send(resealed);
            }
        }
    }
    if let Some(r) = running {
        inner.close(r, "shutdown").await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use lodestar_cluster::{Credentials, JoinToken, Joining, NoEvents, SystemClock};

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(20);
    const WAIT: Duration = Duration::from_secs(15);

    fn wiring(dir: &std::path::Path, enabled: bool, listen: SocketAddr) -> Wiring {
        Wiring {
            db: dir.join("lodestar.db"),
            store: StoreConfig::default(),
            data_dir: dir.to_path_buf(),
            authority: Arc::new(LeaseAuthority::new(1, 600, Arc::new(NoEvents))),
            remote: None,
            settings: config::Cluster {
                enabled,
                listen,
                ..config::Cluster::default()
            },
            clock: Arc::new(SystemClock),
        }
    }

    fn any_port() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    /// Polls `what` until it holds; fails with what it last saw.
    async fn until<T: std::fmt::Debug>(name: &str, mut what: impl FnMut() -> (bool, T)) -> T {
        let start = Instant::now();
        loop {
            let (done, seen) = what();
            if done {
                return seen;
            }
            assert!(start.elapsed() < DEADLINE, "{name}: still {seen:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn pki_rows(dir: &std::path::Path) -> usize {
        let store = Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
        store.settings_with_prefix(PKI_PREFIX).unwrap().len()
    }

    fn metrics(h: &ClusterHandle) -> String {
        let mut out = String::new();
        h.render(&mut out);
        out
    }

    fn series(text: &str, name: &str) -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
            .unwrap_or_else(|| panic!("{name} missing:\n{text}"))
    }

    async fn join(h: &ClusterHandle, name: &str) -> Credentials {
        let server = h.server().expect("serving");
        let token = server.create_join_token(name).await.unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        let addr = h.local_addr().unwrap().to_string();
        lodestar_cluster::join(&addr, joining, Arc::new(SystemClock), WAIT)
            .await
            .unwrap()
    }

    /// A mutual-TLS link, upgraded, before `Hello`.
    async fn link(h: &ClusterHandle, creds: &Credentials) -> Option<lodestar_cluster::WorkerLink> {
        let addr = h.local_addr()?.to_string();
        lodestar_cluster::connect(&addr, creds, Arc::new(SystemClock), WAIT)
            .await
            .ok()
    }

    /// `Hello`, then `Welcome`, on `link`.
    async fn hello(
        link: lodestar_cluster::WorkerLink,
        creds: &Credentials,
    ) -> Result<lodestar_cluster::WorkerSession, lodestar_cluster::ClientError> {
        let hello = lodestar_cluster::proto::Hello::new(creds.worker().0, Default::default());
        let config = lodestar_cluster::WorkerConfig {
            auto_heartbeat: false,
            ..lodestar_cluster::WorkerConfig::default()
        };
        lodestar_cluster::WorkerSession::start(link, hello, Arc::new(SystemClock), config).await
    }

    /// Whether `creds` gets a session (TLS, the upgrade, then `Hello` and
    /// `Welcome`).
    async fn connects(h: &ClusterHandle, creds: &Credentials) -> bool {
        match link(h, creds).await {
            Some(l) => hello(l, creds).await.is_ok(),
            None => false,
        }
    }

    #[tokio::test]
    async fn off_binds_and_creates_nothing_until_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        assert_eq!(h.local_addr(), None);
        assert!(h.server().is_none());
        // Not even the database or the secrets key is touched.
        assert!(!dir.path().join("lodestar.db").exists());
        assert!(!dir.path().join(SecretStore::KEY_FILE).exists());
        let off = metrics(&h);
        assert_eq!(series(&off, "lodestar_cluster_allowed"), 0);
        assert_eq!(series(&off, "lodestar_cluster_listening"), 0);
        assert_eq!(series(&off, "lodestar_cluster_sessions_total"), 0);

        h.set_allowed(true);
        let addr = until("listening", || (h.local_addr().is_some(), h.local_addr()))
            .await
            .unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
        assert!(pki_rows(dir.path()) >= 3, "the CA is created on first use");
        assert!(dir.path().join(SecretStore::KEY_FILE).exists());
        let on = metrics(&h);
        assert_eq!(series(&on, "lodestar_cluster_allowed"), 1);
        assert_eq!(series(&on, "lodestar_cluster_listening"), 1);

        // A worker's session is counted, and the count survives the port
        // closing.
        let creds = join(&h, "w1").await;
        assert!(connects(&h, &creds).await);
        until("a session", || {
            let n = series(&metrics(&h), "lodestar_cluster_sessions_total");
            (n >= 1, n)
        })
        .await;

        h.set_allowed(false);
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        until("port refused", || {
            let refused = std::net::TcpStream::connect(addr).is_err();
            (refused, refused)
        })
        .await;
        let closed = metrics(&h);
        assert_eq!(series(&closed, "lodestar_cluster_listening"), 0);
        assert!(series(&closed, "lodestar_cluster_sessions_total") >= 1);
        // The PKI is kept for the next time: the same worker comes back.
        let rows = pki_rows(dir.path());
        h.set_allowed(true);
        until("listening again", || {
            (h.local_addr().is_some(), h.local_addr())
        })
        .await;
        assert!(connects(&h, &creds).await);
        assert!(pki_rows(dir.path()) >= rows);
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn refuses_clients_without_this_clusters_certificate() {
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = Cluster::start(wiring(a_dir.path(), true, any_port()))
            .await
            .unwrap();
        let b = Cluster::start(wiring(b_dir.path(), true, any_port()))
            .await
            .unwrap();
        let (ah, bh) = (a.handle(), b.handle());
        // Bytes that aren't TLS are dropped before anything is read.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut raw = tokio::net::TcpStream::connect(ah.local_addr().unwrap())
                .await
                .unwrap();
            raw.write_all(b"GET /cluster HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            let _ = tokio::time::timeout(WAIT, raw.read_to_end(&mut buf)).await;
            assert!(!String::from_utf8_lossy(&buf).contains("HTTP/1.1 101"));
        }
        until("handshake refused", || {
            let m = metrics(&ah);
            let n = series(
                &m,
                "lodestar_cluster_preauth_refused_total{reason=\"handshake_failed\"}",
            );
            (n >= 1, n)
        })
        .await;
        // A worker of another cluster has a certificate, but not one this
        // cluster's CA issued.
        let stranger = join(&bh, "stranger").await;
        assert!(connects(&bh, &stranger).await, "its own cluster takes it");
        assert!(!connects(&ah, &stranger).await);
        assert_eq!(
            series(&metrics(&ah), "lodestar_cluster_sessions_total"),
            0,
            "no session for an unauthenticated client"
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// Turning workers off ends live sessions with `Stop(Shutdown)`, and a
    /// connection still before its `Hello` gets no session afterwards.
    #[tokio::test]
    async fn off_ends_every_session_and_every_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        let (one, two) = (join(&h, "w1").await, join(&h, "w2").await);
        let held = link(&h, &one).await.expect("a link");
        let mut live = hello(link(&h, &two).await.unwrap(), &two).await.unwrap();
        until("connected", || {
            let n = series(&metrics(&h), "lodestar_cluster_workers_connected");
            (n == 1, n)
        })
        .await;

        h.set_allowed(false);
        let ended = tokio::time::timeout(DEADLINE, async {
            loop {
                match live.next_event().await {
                    Some(lodestar_cluster::WorkerEvent::Ended(reason)) => return reason,
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await
        .expect("the live session ended");
        assert_eq!(
            ended,
            Some(lodestar_cluster::proto::StopReason::Shutdown),
            "told why"
        );
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        // The link that was through TLS and the upgrade, but hadn't said
        // `Hello`, is refused now.
        assert!(hello(held, &one).await.is_err());
        let m = metrics(&h);
        assert_eq!(series(&m, "lodestar_cluster_workers_connected"), 0);
        assert_eq!(series(&m, "lodestar_cluster_sessions_total"), 1, "{m}");
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn a_taken_port_fails_the_start_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let taken = std::net::TcpListener::bind(any_port()).unwrap();
        let addr = taken.local_addr().unwrap();
        let e = Cluster::start(wiring(dir.path(), true, addr))
            .await
            .unwrap_err();
        assert_eq!(e.step, "bind");
        assert!(e.to_string().contains("cluster.listen"), "{e}");
        assert!(!dir.path().join("lodestar.db").exists());
        assert!(!dir.path().join(SecretStore::KEY_FILE).exists());

        // Turned on later, a failure is counted and serving goes on.
        let cluster = Cluster::start(wiring(dir.path(), false, addr))
            .await
            .unwrap();
        let h = cluster.handle();
        h.set_allowed(true);
        until("a start failure", || {
            let n = series(&metrics(&h), "lodestar_cluster_start_failures_total");
            (n >= 1, n)
        })
        .await;
        assert_eq!(h.local_addr(), None);
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn reseal_between_rotate_and_retire_keeps_the_pki_open() {
        let dir = tempfile::tempdir().unwrap();
        // Never turned on: nothing to seal, and nothing is created.
        let off = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        // The database exists (another part of serve made it) but holds no
        // cluster rows.
        drop(Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap());
        assert!(!off.handle().reseal().await.unwrap());
        assert_eq!(pki_rows(dir.path()), 0);
        off.shutdown().await;

        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        let creds = join(&h, "w1").await;
        let fingerprint = |secrets: &SecretStore| {
            let mut store =
                Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
            Coordinator::open(&mut store, secrets, SystemClock.now_secs())
                .map(|c| c.ca_fingerprint())
        };
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        let before = fingerprint(&secrets).unwrap();
        let old_key = secrets.primary_key_id();

        secrets.rotate().unwrap();
        assert!(h.reseal().await.unwrap());
        secrets.retire_old_keys().unwrap();

        // Only the new key is left, and the PKI still opens with it: the
        // same CA, so joined workers keep working.
        let fresh = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(fresh.key_count(), 1);
        assert_ne!(fresh.primary_key_id(), old_key);
        assert_eq!(fingerprint(&fresh).unwrap(), before);
        until("serving again", || {
            (h.local_addr().is_some(), h.local_addr())
        })
        .await;
        assert!(connects(&h, &creds).await);

        // Off with a PKI: it's resealed, and the port stays closed.
        h.set_allowed(false);
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        secrets.rotate().unwrap();
        assert!(h.reseal().await.unwrap());
        secrets.retire_old_keys().unwrap();
        assert_eq!(h.local_addr(), None);
        let fresh = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(fingerprint(&fresh).unwrap(), before);
        cluster.shutdown().await;
    }

    /// "Off" right after "on" (before `serve` was first polled) still
    /// closes the port, and shutdown returns: the server notices a
    /// shutdown flag set before it subscribed.
    #[tokio::test]
    async fn off_right_after_on_closes_and_shuts_down() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
                .await
                .unwrap();
            let h = cluster.handle();
            h.set_allowed(true);
            until("the PKI opening", || {
                let made = dir.path().join("lodestar.db").exists();
                (made, made)
            })
            .await;
            h.set_allowed(false);
            tokio::time::timeout(DEADLINE, cluster.shutdown())
                .await
                .expect("shutdown returned");
            assert_eq!(h.local_addr(), None);
        }
    }

    /// A rotation that forgets the cluster's reseal doesn't pass quietly:
    /// once the old key is retired the PKI no longer opens, and turning
    /// workers on fails with the reason.
    #[tokio::test]
    async fn retire_without_reseal_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        cluster.shutdown().await;
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        secrets.rotate().unwrap();
        secrets.retire_old_keys().unwrap();
        let e = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap_err();
        assert_eq!(e.step, "pki", "{e}");
        assert!(e.to_string().starts_with("cluster port (pki): "), "{e}");
    }

    /// Every family rendered has its row in docs/metrics.md, and the text
    /// is valid exposition (one TYPE per family, every sample under one).
    #[tokio::test]
    async fn every_family_is_documented() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        let text = metrics(&cluster.handle());
        let doc = include_str!("../../../../docs/metrics.md");
        let families: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("# TYPE lodestar_"))
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(families.len(), 20, "{text}");
        for family in &families {
            assert!(
                doc.lines().any(|l| l.starts_with(&format!("| `{family}"))),
                "docs/metrics.md has no row for {family}"
            );
            assert_eq!(
                text.matches(&format!("# TYPE lodestar_{family} ")).count(),
                1
            );
        }
        for sample in text.lines().filter(|l| !l.starts_with('#')) {
            let name = sample.split(['{', ' ']).next().unwrap();
            assert!(
                families.iter().any(|f| name == format!("lodestar_{f}")),
                "{sample}"
            );
        }
        cluster.shutdown().await;
    }

    #[test]
    fn the_section_sets_the_server() {
        let c = config::Cluster {
            max_skew_ms: 5_000,
            handshake_timeout_secs: 7,
            max_pending: 32,
            max_pending_per_ip: 4,
            session_queue_mib: 2,
            ..config::Cluster::default()
        };
        let s = server_config(&c);
        assert_eq!(s.max_skew_ms, 5_000);
        assert_eq!(s.handshake_timeout, Duration::from_secs(7));
        assert_eq!((s.max_pending, s.max_pending_per_ip), (32, 4));
        assert_eq!(s.session_queue_bytes, 2 << 20);
        // The protocol's own defaults stay.
        let d = ServerConfig::default();
        assert_eq!(
            (s.heartbeat, s.lease_ttl_secs),
            (d.heartbeat, d.lease_ttl_secs)
        );
        assert!(s.auto_tick);
    }
}
