//! The workers API's side in the binary (architecture §19.6, §19.7;
//! workers plan WK-14): [`Workers`] answers `lodestar_api::workers` from
//! the cluster server and the placement planner, so the API crate never
//! reaches into cluster internals.
//!
//! - **The list** is the server's fleet snapshot joined with the
//!   certificates on record (for names, and for workers that haven't
//!   connected since this start) and the saved settings. Revoked workers
//!   aren't listed. Nothing in it is a peer address or key material.
//! - **Settings are saved** (pin, intensity, budget, pool cap and a
//!   pause) as one `settings` row per worker, `workers.prefs.<id>`, and
//!   re-applied when the coordinator starts: settings to the planner, and
//!   a pause to every cluster server as soon as it's built (turning
//!   workers off and on builds a new one), so a restart never un-pauses
//!   a worker the admin stopped. They're kept outside `cluster.`: any row
//!   there counts as "the cluster PKI exists". A row that doesn't read
//!   back is skipped with a warning (`workers.prefs_skipped`).
//! - **Before the worker runtime (WK-10)** pins, budgets and intensity are
//!   saved and planned, but nothing sends `Assign` yet, so workers don't
//!   apply them until WK-10's tick runs [`Placement::sync`]. Pause, drain,
//!   resume and revoke act at once: they're the cluster server's.
//! - **Revoke** stops the session and takes the leases back
//!   (`ClusterServer::revoke_worker`), then forgets the worker in the
//!   planner and deletes its row: disconnect first, so the next sync
//!   can't rejoin it with the defaults (#560's delta review).
//! - **No token is kept or logged.** `create_token` hands the one copy to
//!   the API, which returns it once.
//!
//! WK-13 adds quarantine, release and purge beside `revoke`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use lodestar_api::dto::workers::{
    DhtPin as PinDto, Worker, WorkerBudget, WorkerIntensity, WorkerList, WorkerPatch,
    WorkerRevoked, WorkerStatus as StatusDto,
};
use lodestar_api::ops::BoxFuture;
use lodestar_api::workers::{NewToken, WorkersApi, WorkersError};
use lodestar_cluster::assign::{Budget, Intensity};
use lodestar_cluster::placement::{DhtPin, WorkerSettings};
use lodestar_cluster::{
    ClusterError, ClusterServer, IssuedCert, JOIN_TOKEN_TTL_SECS, WorkerId, WorkerStatus,
};
use lodestar_store::{Store, StoreConfig};
use serde::{Deserialize, Serialize};

use super::placement::{Placement, PrefsFull};
use super::serve::ClusterHandle;
use crate::logging::events;

/// How often the follower checks for a newly built cluster server, to
/// re-apply saved pauses to it.
const FOLLOW_EVERY: Duration = Duration::from_millis(100);

/// Each worker's saved settings: `workers.prefs.<id>`.
pub const PREFS_PREFIX: &str = "workers.prefs.";

fn prefs_key(worker: WorkerId) -> String {
    format!("{PREFS_PREFIX}{}", worker.0)
}

/// One worker's saved settings, as its row holds them (JSON, version 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Saved {
    v: u8,
    dht_off: bool,
    intensity: u8,
    cpu_permille: u16,
    ram_mb: u32,
    up_kbps: u32,
    down_kbps: u32,
    pool_cap: u32,
    paused: bool,
}

impl Default for Saved {
    fn default() -> Saved {
        Saved::from_parts(DhtPin::Auto, WorkerSettings::default())
    }
}

impl Saved {
    fn from_parts(pin: DhtPin, s: WorkerSettings) -> Saved {
        Saved {
            v: 1,
            dht_off: pin == DhtPin::Off,
            intensity: match s.intensity {
                Intensity::Low => 0,
                Intensity::Balanced => 1,
                Intensity::Aggressive => 2,
            },
            cpu_permille: s.budget.cpu_permille,
            ram_mb: s.budget.ram_mb,
            up_kbps: s.budget.up_kbps,
            down_kbps: s.budget.down_kbps,
            pool_cap: s.pool_cap,
            paused: false,
        }
    }

    fn pin(&self) -> DhtPin {
        if self.dht_off {
            DhtPin::Off
        } else {
            DhtPin::Auto
        }
    }

    fn intensity(&self) -> Intensity {
        match self.intensity {
            0 => Intensity::Low,
            2 => Intensity::Aggressive,
            _ => Intensity::Balanced,
        }
    }

    fn settings(&self) -> WorkerSettings {
        WorkerSettings {
            budget: Budget {
                cpu_permille: self.cpu_permille,
                ram_mb: self.ram_mb,
                up_kbps: self.up_kbps,
                down_kbps: self.down_kbps,
            },
            intensity: self.intensity(),
            pool_cap: self.pool_cap,
        }
    }

    /// A row this version wrote, within the API's ranges; anything else is
    /// skipped (the worker gets the defaults) rather than trusted.
    pub(crate) fn parse(text: &str) -> Option<Saved> {
        let s: Saved = serde_json::from_str(text).ok()?;
        let ok = s.v == 1
            && s.intensity <= 2
            && lodestar_api::workers::CPU_PERMILLE.contains(&s.cpu_permille)
            && lodestar_api::workers::RAM_MB.contains(&s.ram_mb)
            && lodestar_api::workers::KBPS.contains(&s.up_kbps)
            && lodestar_api::workers::KBPS.contains(&s.down_kbps)
            && lodestar_api::workers::POOL_CAP.contains(&s.pool_cap);
        ok.then_some(s)
    }

    /// Saved paused (the cluster tick seeds a new server with it, WK-10d).
    pub(crate) fn paused(&self) -> bool {
        self.paused
    }

    fn text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn apply(&mut self, p: &WorkerPatch) {
        if let Some(pin) = p.dht {
            self.dht_off = pin == PinDto::Off;
        }
        if let Some(i) = p.intensity {
            self.intensity = match i {
                WorkerIntensity::Low => 0,
                WorkerIntensity::Balanced => 1,
                WorkerIntensity::Aggressive => 2,
            };
        }
        if let Some(b) = p.budget {
            self.cpu_permille = b.cpu_permille;
            self.ram_mb = b.ram_mb;
            self.up_kbps = b.up_kbps;
            self.down_kbps = b.down_kbps;
        }
        if let Some(cap) = p.pool_cap {
            self.pool_cap = cap;
        }
    }
}

/// The workers API over the cluster port and the planner.
pub struct Workers {
    cluster: ClusterHandle,
    placement: Arc<Placement>,
    /// A read-write connection for the settings rows.
    store: Arc<Mutex<Store>>,
    saved: Mutex<BTreeMap<WorkerId, Saved>>,
    /// The server saved pauses were last applied to.
    applied_to: Mutex<Weak<ClusterServer>>,
    /// Held across every change to a worker's settings, from reading them
    /// to the row being written, so concurrent PATCH, pause, resume and
    /// revoke calls can't lose each other's changes or land out of order.
    edits: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workers").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn failed(e: impl std::fmt::Display) -> WorkersError {
    WorkersError::Failed(e.to_string())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl Workers {
    /// Opens its own store connection on `db`, reads every saved worker's
    /// settings and applies them to `placement`, so a restarted
    /// coordinator plans each worker as the admin left it. Rows that
    /// don't parse, or past `MAX_WORKERS`, are skipped.
    ///
    /// It spawns a follower (on the current Tokio runtime) that applies
    /// saved pauses to each new cluster server; it ends with the last
    /// `Arc`.
    pub fn open(
        cluster: ClusterHandle,
        placement: Arc<Placement>,
        db: &Path,
        config: StoreConfig,
    ) -> Result<Arc<Workers>, lodestar_store::Error> {
        let store = Store::open(db, config)?;
        let mut saved = BTreeMap::new();
        for (key, text) in store.settings_with_prefix(PREFS_PREFIX)? {
            let Some(id) = key
                .strip_prefix(PREFS_PREFIX)
                .and_then(|id| id.parse::<u32>().ok())
                .filter(|&id| id != 0)
            else {
                continue;
            };
            let Some(s) = Saved::parse(&text) else {
                lodestar_log::log_event!(
                    events::WORKERS_PREFS_SKIPPED,
                    worker = lodestar_log::Count(u64::from(id))
                );
                continue;
            };
            let w = WorkerId(id);
            if placement.set_settings(w, s.settings()).is_ok() && placement.pin(w, s.pin()).is_ok()
            {
                saved.insert(w, s);
            }
        }
        let workers = Arc::new(Workers {
            cluster,
            placement,
            store: Arc::new(Mutex::new(store)),
            saved: Mutex::new(saved),
            applied_to: Mutex::new(Weak::new()),
            edits: tokio::sync::Mutex::new(()),
        });
        // A worker over its floor budget: the sink has paused it and taken
        // its leases back; this saves the pause (#607's review). With
        // `Workers` gone, or no runtime, the in-memory pause still stands.
        let floor = Arc::downgrade(&workers);
        workers.cluster.on_floor_over_budget(Arc::new(move |w| {
            let (Some(workers), Ok(rt)) = (floor.upgrade(), tokio::runtime::Handle::try_current())
            else {
                return;
            };
            rt.spawn(async move {
                workers.save_floor_pause(w).await;
            });
        }));
        let weak = Arc::downgrade(&workers);
        tokio::spawn(async move {
            while let Some(w) = weak.upgrade() {
                w.follow();
                drop(w);
                tokio::time::sleep(FOLLOW_EVERY).await;
            }
        });
        workers.follow();
        Ok(workers)
    }

    /// Applies every saved pause to the cluster server, once per server.
    /// A server is built whenever workers are turned on, with an empty
    /// fleet; `ClusterServer::pause` marks a worker before it connects.
    fn follow(&self) -> Option<Arc<ClusterServer>> {
        let server = self.cluster.server()?;
        let mut last = lock(&self.applied_to);
        if !std::ptr::eq(last.as_ptr(), Arc::as_ptr(&server)) {
            for (w, s) in lock(&self.saved).iter() {
                if s.paused {
                    server.pause(*w);
                }
            }
            *last = Arc::downgrade(&server);
        }
        Some(server)
    }

    /// The planner this API pins and budgets; WK-10's tick syncs it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn placement(&self) -> &Arc<Placement> {
        &self.placement
    }

    /// The cluster server, or `Off` while "Allow workers" is off: there's
    /// nothing to act on then (turn workers on to revoke one).
    fn server(&self) -> Result<Arc<ClusterServer>, WorkersError> {
        self.follow().ok_or(WorkersError::Off)
    }

    fn saved(&self, w: WorkerId) -> Saved {
        lock(&self.saved).get(&w).copied().unwrap_or_default()
    }

    /// Writes (or with `None`, deletes) a worker's row.
    /// Deletes `w`'s written floor reports (`worker-floor:<w>`).
    async fn purge_floor_holds(&self, w: WorkerId) -> Result<usize, WorkersError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || lock(&store).purge_worker_floor(w.0))
            .await
            .map_err(failed)?
            .map_err(failed)
    }

    async fn persist(&self, w: WorkerId, s: Option<Saved>) -> Result<(), WorkersError> {
        let store = self.store.clone();
        let key = prefs_key(w);
        tokio::task::spawn_blocking(move || {
            let mut store = lock(&store);
            match s {
                Some(s) => store.update_settings_text(&[(&key, &s.text())], &[]),
                None => store.update_settings_text(&[], &[&key]),
            }
        })
        .await
        .map_err(failed)?
        .map_err(failed)
    }

    /// Every worker this coordinator knows: connected since it started, or
    /// holding a certificate that isn't revoked. With the newest name.
    async fn known(&self, server: &ClusterServer) -> BTreeMap<WorkerId, Known> {
        let mut out: BTreeMap<WorkerId, Known> = BTreeMap::new();
        let certs: Vec<IssuedCert> = server.certificates().await;
        for c in certs.into_iter().filter(|c| !c.revoked) {
            // Sorted by expiry: the last is the newest.
            out.entry(c.worker).or_default().name = Some(c.name);
        }
        for s in server.snapshot().workers {
            if s.worker == WorkerId::LOCAL {
                continue;
            }
            // Connected now, or holding a certificate that stands: an entry
            // with neither (revoked, expired, or a saved pause for a worker
            // whose certificate is gone) isn't a worker any more.
            if !s.connected && !out.contains_key(&s.worker) {
                continue;
            }
            let w = s.worker;
            out.entry(w).or_default().status = Some(s);
        }
        out
    }

    async fn one(&self, server: &ClusterServer, w: WorkerId) -> Result<Worker, WorkersError> {
        let known = self.known(server).await;
        let k = known.get(&w).ok_or(WorkersError::UnknownWorker)?;
        Ok(self.dto(w, k))
    }

    fn dto(&self, w: WorkerId, k: &Known) -> Worker {
        let s = self.saved(w);
        let st = k.status.as_ref();
        let live = st.filter(|s| s.last_seen_ms > 0);
        Worker {
            budget: WorkerBudget {
                cpu_permille: s.cpu_permille,
                down_kbps: s.down_kbps,
                ram_mb: s.ram_mb,
                up_kbps: s.up_kbps,
            },
            claimed: live.map(|s| s.stats.claimed),
            clock_offset_ms: live.map(|s| s.offset_ms),
            clock_skewed: st.is_some_and(|s| s.skewed),
            cpu_permille: live.map(|s| s.stats.cpu_permille),
            dht: match s.pin() {
                DhtPin::Auto => PinDto::Auto,
                DhtPin::Off => PinDto::Off,
            },
            external_ips: st
                .map(|s| s.external_ips.iter().map(ToString::to_string).collect())
                .unwrap_or_default(),
            fetched: live.map(|s| s.stats.fetched),
            id: w.0,
            intensity: match s.intensity() {
                Intensity::Low => WorkerIntensity::Low,
                Intensity::Balanced => WorkerIntensity::Balanced,
                Intensity::Aggressive => WorkerIntensity::Aggressive,
            },
            last_seen_at: live.map(|s| s.last_seen_ms.div_euclid(1000)),
            leases: st.map_or(0, |s| s.leases as u64),
            name: k.name.clone(),
            paused: s.paused || st.is_some_and(|s| s.paused),
            pool_cap: s.pool_cap,
            processed: live.map(|s| s.stats.processed),
            rss_mb: live.map(|s| s.stats.rss_mb),
            spool_bytes: live.map(|s| s.stats.spool_bytes),
            // Draining wins over paused: it's the one that ends the session.
            status: match st {
                Some(s) if s.connected && s.draining => StatusDto::Draining,
                Some(s) if s.connected && s.paused => StatusDto::Paused,
                Some(s) if s.connected => StatusDto::Online,
                _ => StatusDto::Offline,
            },
        }
    }

    /// The worker, if this coordinator knows it, else `UnknownWorker`.
    async fn check(&self, server: &ClusterServer, w: WorkerId) -> Result<Known, WorkersError> {
        self.known(server)
            .await
            .remove(&w)
            .ok_or(WorkersError::UnknownWorker)
    }

    async fn list_now(&self) -> Result<WorkerList, WorkersError> {
        let Some(server) = self.follow() else {
            return Ok(WorkerList {
                allowed: false,
                workers: Vec::new(),
            });
        };
        let workers = self
            .known(&server)
            .await
            .iter()
            .map(|(w, k)| self.dto(*w, k))
            .collect();
        Ok(WorkerList {
            allowed: self.cluster.allowed(),
            workers,
        })
    }

    async fn token_now(&self, name: String) -> Result<NewToken, WorkersError> {
        let (Some(server), Some(addr)) = (self.cluster.server(), self.cluster.local_addr()) else {
            return Err(WorkersError::Off);
        };
        let token = server.create_join_token(&name).await.map_err(|e| match e {
            ClusterError::InvalidName => WorkersError::BadName,
            ClusterError::NameInUse => WorkersError::NameInUse,
            ClusterError::TooManyTokens => WorkersError::TooManyTokens,
            other => failed(other),
        })?;
        lodestar_log::log_event!(events::WORKERS_TOKEN_CREATED);
        Ok(NewToken {
            token: token.expose().to_string(),
            name,
            expires_at: unix_now() + JOIN_TOKEN_TTL_SECS,
            port: addr.port(),
        })
    }

    async fn drain_now(&self, w: WorkerId) -> Result<Worker, WorkersError> {
        let server = self.server()?;
        self.check(&server, w).await?;
        if !server.drain(w) {
            return Err(WorkersError::NotConnected);
        }
        self.one(&server, w).await
    }

    /// Pauses a known worker, connected or not, and saves it.
    async fn pause_now(&self, w: WorkerId) -> Result<Worker, WorkersError> {
        let _edit = self.edits.lock().await;
        let server = self.server()?;
        self.check(&server, w).await?;
        self.set_paused(w, true).await?;
        // The server serving now: one built meanwhile already had the
        // saved pause applied by `follow`.
        self.server()?.pause(w);
        self.one(&server, w).await
    }

    /// Ends a drain and a pause, and saves it.
    async fn resume_now(&self, w: WorkerId) -> Result<Worker, WorkersError> {
        let _edit = self.edits.lock().await;
        let server = self.server()?;
        self.check(&server, w).await?;
        self.set_paused(w, false).await?;
        self.server()?.resume(w);
        self.one(&server, w).await
    }

    /// Saves the pause of a worker the sink paused for too many floor
    /// reports. A failure is logged; the server's pause stands until a
    /// restart.
    ///
    /// Under `edits`, so it orders with an admin's calls (#607's review): a
    /// worker revoked meanwhile isn't saved again, and one an admin resumed
    /// meanwhile is paused again with the save, so the saved and live states
    /// agree.
    ///
    /// Returns the code it logged (`workers.pause_not_saved`), if any:
    /// `unknown_worker` for a worker removed meanwhile (#615's review),
    /// `prefs_full` or `failed` for a save that didn't land.
    async fn save_floor_pause(&self, w: WorkerId) -> Option<&'static str> {
        let _edit = self.edits.lock().await;
        let code = if let Some(server) = self.follow()
            && self.check(&server, w).await.is_err()
        {
            Some("unknown_worker")
        } else {
            let saved = self.set_paused(w, true).await;
            if let Some(server) = self.follow() {
                server.pause(w);
            }
            saved.err().map(|e| match e {
                WorkersError::PrefsFull => "prefs_full",
                _ => "failed",
            })
        };
        if let Some(code) = code {
            lodestar_log::log_event!(
                events::WORKERS_PAUSE_NOT_SAVED,
                worker = lodestar_log::Count(u64::from(w.0)),
                code = code
            );
        }
        code
    }

    /// Saves a pause or its end. The caller holds `edits`.
    async fn set_paused(&self, w: WorkerId, paused: bool) -> Result<(), WorkersError> {
        let mut s = self.saved(w);
        if s.paused == paused {
            return Ok(());
        }
        s.paused = paused;
        // A row is kept only for a worker the planner keeps settings for.
        if !self.placement.can_store(w) {
            return Err(WorkersError::PrefsFull);
        }
        // The row first: a failed write leaves the state as it was.
        self.persist(w, Some(s)).await?;
        lock(&self.saved).insert(w, s);
        Ok(())
    }

    async fn revoke_now(&self, w: WorkerId) -> Result<WorkerRevoked, WorkersError> {
        let _edit = self.edits.lock().await;
        let server = self.server()?;
        let known = self.check(&server, w).await?;
        // Disconnect (and take the leases back) first, then forget: a sync
        // in between can't rejoin it with the defaults.
        let (certificates, leases) = server.revoke_worker(w).await.map_err(failed)?;
        // Its floor reports not written yet go too (#607's review), and so
        // do the ones written: an owner-approved choice
        // (`docs/decisions/2026-11-14-X-sec-S6-02-03-r3.md`). Each key is
        // judged under the floor again by whoever fetches it next. A
        // failed delete leaves the holds in place, and the revocation
        // stands either way.
        self.cluster.forget_floor_reports(w);
        let _ = self.purge_floor_holds(w).await;
        self.placement.forget(w);
        lock(&self.saved).remove(&w);
        // The revocation stands either way. A row left behind only gives
        // an id that can never reconnect its old settings at the next
        // start, so this failing doesn't fail the revoke.
        let _ = self.persist(w, None).await;
        lodestar_log::log_event!(
            events::WORKERS_REVOKED,
            worker = lodestar_log::Count(u64::from(w.0)),
            leases = lodestar_log::Count(leases as u64)
        );
        Ok(WorkerRevoked {
            certificates: certificates as u64,
            gone_within_secs: u32::try_from(server.heartbeat_interval().as_secs().max(1))
                .unwrap_or(u32::MAX),
            id: w.0,
            leases_returned: leases as u64,
            name: known.name,
        })
    }

    async fn update_now(&self, w: WorkerId, patch: WorkerPatch) -> Result<Worker, WorkersError> {
        let _edit = self.edits.lock().await;
        let server = self.server()?;
        self.check(&server, w).await?;
        let mut s = self.saved(w);
        s.apply(&patch);
        let full = |_: PrefsFull| WorkersError::PrefsFull;
        self.placement.set_settings(w, s.settings()).map_err(full)?;
        self.placement.pin(w, s.pin()).map_err(full)?;
        // The row first, as `set_paused` does. The planner was changed
        // above (it says whether there's room); a failed write is a 500
        // and the plan is corrected by the next PATCH or restart.
        self.persist(w, Some(s)).await?;
        lock(&self.saved).insert(w, s);
        self.one(&server, w).await
    }
}

/// What [`Workers::known`] found for one worker.
#[derive(Debug, Default)]
struct Known {
    name: Option<String>,
    status: Option<WorkerStatus>,
}

fn id(w: u32) -> WorkerId {
    WorkerId(w)
}

impl WorkersApi for Workers {
    fn list(&self) -> BoxFuture<'_, Result<WorkerList, WorkersError>> {
        Box::pin(self.list_now())
    }

    fn create_token(&self, name: String) -> BoxFuture<'_, Result<NewToken, WorkersError>> {
        Box::pin(self.token_now(name))
    }

    fn drain(&self, w: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(self.drain_now(id(w)))
    }

    fn resume(&self, w: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(self.resume_now(id(w)))
    }

    fn pause(&self, w: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(self.pause_now(id(w)))
    }

    fn revoke(&self, w: u32) -> BoxFuture<'_, Result<WorkerRevoked, WorkersError>> {
        Box::pin(self.revoke_now(id(w)))
    }

    fn update(&self, w: u32, patch: WorkerPatch) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(self.update_now(id(w), patch))
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Instant;

    use lodestar_cluster::proto::{
        Capabilities, ClaimItem, DenyReason, GrantItem, Hello, Outcome, OutcomeItem, StopReason,
    };
    use lodestar_cluster::{
        Credentials, JoinToken, Joining, LeaseAuthority, NoEvents, SystemClock, WorkerConfig,
        WorkerEvent, WorkerSession,
    };

    use super::*;
    use crate::cluster::{Cluster, Wiring};
    use crate::config;

    const DEADLINE: Duration = Duration::from_secs(20);

    /// Polls `what` until it holds; fails with what it last saw.
    async fn until<T: std::fmt::Debug, F: std::future::Future<Output = (bool, T)>>(
        name: &str,
        mut what: impl FnMut() -> F,
    ) -> T {
        let start = Instant::now();
        loop {
            let (done, seen) = what().await;
            if done {
                return seen;
            }
            assert!(start.elapsed() < DEADLINE, "{name}: still {seen:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    struct Coord {
        cluster: Cluster,
        workers: Arc<Workers>,
    }

    async fn coordinator(dir: &Path, enabled: bool) -> Coord {
        crate::test_log::install();
        let placement = Arc::new(Placement::new(true));
        let cluster = Cluster::start(Wiring {
            db: dir.join("lodestar.db"),
            store: StoreConfig::default(),
            data_dir: dir.to_path_buf(),
            authority: Arc::new(LeaseAuthority::new(1, 600, Arc::new(NoEvents))),
            remote: None,
            settings: config::Cluster {
                enabled,
                listen: SocketAddr::from(([127, 0, 0, 1], 0)),
                ..config::Cluster::default()
            },
            clock: Arc::new(SystemClock),
            placement: placement.clone(),
            fleet: None,
            coordinator_dht: None,
            coordinator_share: None,
            held: None,
        })
        .await
        .unwrap();
        let h = cluster.handle();
        if enabled {
            until("serving", || {
                let h = h.clone();
                async move { (h.server().is_some(), ()) }
            })
            .await;
        }
        let workers = Workers::open(
            h,
            placement,
            &dir.join("lodestar.db"),
            StoreConfig::default(),
        )
        .unwrap();
        Coord { cluster, workers }
    }

    impl Coord {
        fn addr(&self) -> String {
            self.cluster.handle().local_addr().unwrap().to_string()
        }

        async fn join(&self, name: &str) -> (Credentials, String) {
            let new = self.workers.create_token(name.into()).await.unwrap();
            let token = new.token.clone();
            let joining = Joining::new(JoinToken::parse(&token).unwrap()).unwrap();
            let creds =
                lodestar_cluster::join(&self.addr(), joining, Arc::new(SystemClock), DEADLINE)
                    .await
                    .unwrap();
            (creds, token)
        }

        async fn connect(&self, creds: &Credentials) -> WorkerSession {
            let link =
                lodestar_cluster::connect(&self.addr(), creds, Arc::new(SystemClock), DEADLINE)
                    .await
                    .unwrap();
            let caps = Capabilities {
                external_ips: vec!["81.2.69.7".parse().unwrap()],
                ..Capabilities::default()
            };
            let config = WorkerConfig {
                auto_heartbeat: false,
                ..WorkerConfig::default()
            };
            let hello = Hello::new(creds.worker().0, caps);
            WorkerSession::start(link, hello, Arc::new(SystemClock), config)
                .await
                .unwrap()
        }

        async fn status(&self, w: WorkerId) -> Option<Worker> {
            let list = self.workers.list().await.unwrap();
            list.workers.into_iter().find(|x| x.id == w.0)
        }

        async fn wait_status(&self, w: WorkerId, want: StatusDto) -> Worker {
            until("status", || async move {
                let s = self.status(w).await;
                (s.as_ref().is_some_and(|s| s.status == want), s)
            })
            .await
            .unwrap()
        }

        async fn stop(self) {
            drop(self.workers);
            self.cluster.shutdown().await;
        }
    }

    fn claim(key: u8) -> Vec<ClaimItem> {
        vec![ClaimItem {
            key: [key; 20],
            source: 0,
            strategy: 0,
            discovered_ms: unix_now() * 1000,
            hints: vec![],
        }]
    }

    fn granted(items: &[GrantItem]) -> bool {
        matches!(items, [GrantItem::Granted { .. }])
    }

    /// Events already queued, without waiting for more: a heartbeat's ack
    /// comes after everything the coordinator sent before it.
    async fn queued(session: &mut WorkerSession) -> Vec<WorkerEvent> {
        session.heartbeat().await.unwrap();
        let mut out = Vec::new();
        while let Ok(Some(e)) = tokio::time::timeout(Duration::ZERO, session.next_event()).await {
            out.push(e);
        }
        out
    }

    #[tokio::test]
    async fn a_single_machine_lists_nothing_and_acts_on_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), false).await;
        let list = c.workers.list().await.unwrap();
        assert_eq!(
            list,
            WorkerList {
                allowed: false,
                workers: vec![]
            }
        );
        assert_eq!(
            c.workers.create_token("w1".into()).await.unwrap_err(),
            WorkersError::Off
        );
        for e in [
            c.workers.drain(1).await.unwrap_err(),
            c.workers.pause(1).await.unwrap_err(),
            c.workers.resume(1).await.unwrap_err(),
            c.workers
                .update(1, WorkerPatch::default())
                .await
                .unwrap_err(),
        ] {
            assert_eq!(e, WorkersError::Off);
        }
        assert_eq!(c.workers.revoke(1).await.unwrap_err(), WorkersError::Off);
        // Nothing was created: no settings rows, no PKI.
        let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        assert!(store.settings_with_prefix(PREFS_PREFIX).unwrap().is_empty());
        assert!(store.settings_with_prefix("cluster.").unwrap().is_empty());
        c.stop().await;
    }

    #[tokio::test]
    async fn drain_pause_resume_on_a_real_worker() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, token) = c.join("w1").await;
        let w = creds.worker();
        let s = c.connect(&creds).await;
        let online = c.wait_status(w, StatusDto::Online).await;
        assert_eq!(online.name.as_deref(), Some("w1"));
        assert_eq!(online.external_ips, vec!["81.2.69.7".to_string()]);
        assert!(granted(&s.claim(claim(1)).await.unwrap()));

        // Paused: connected, no new leases, never told to drain.
        assert_eq!(
            c.workers.pause(w.0).await.unwrap().status,
            StatusDto::Paused
        );
        let denied = s.claim(claim(2)).await.unwrap();
        assert!(matches!(
            denied[..],
            [GrantItem::Denied {
                reason: DenyReason::Draining,
                ..
            }]
        ));
        assert_eq!(
            c.status(w).await.unwrap().leases,
            1,
            "it keeps what it holds"
        );
        // A pause survives a reconnect and is never turned into a drain.
        drop(s);
        let mut s = c.connect(&creds).await;
        let events = queued(&mut s).await;
        assert!(!events.contains(&WorkerEvent::Drain), "{events:?}");
        assert_eq!(
            c.wait_status(w, StatusDto::Paused).await.status,
            StatusDto::Paused
        );
        assert!(!granted(&s.claim(claim(3)).await.unwrap()));

        assert_eq!(
            c.workers.resume(w.0).await.unwrap().status,
            StatusDto::Online
        );
        assert!(granted(&s.claim(claim(4)).await.unwrap()));

        // Drain: the worker is told; draining shows over a pause.
        c.workers.pause(w.0).await.unwrap();
        assert_eq!(
            c.workers.drain(w.0).await.unwrap().status,
            StatusDto::Draining
        );
        let start = Instant::now();
        loop {
            let e = queued(&mut s).await;
            if e.contains(&WorkerEvent::Drain) {
                break;
            }
            assert!(start.elapsed() < DEADLINE, "no drain: {e:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            c.workers.resume(w.0).await.unwrap().status,
            StatusDto::Online
        );
        assert_eq!(
            c.workers.drain(99).await.unwrap_err(),
            WorkersError::UnknownWorker
        );
        // No response but the create carries the token: every action's,
        // from the real binary.
        let patch = WorkerPatch {
            pool_cap: Some(9),
            ..WorkerPatch::default()
        };
        let mut bodies = vec![
            serde_json::to_string(&c.workers.list().await.unwrap()).unwrap(),
            serde_json::to_string(&c.workers.pause(w.0).await.unwrap()).unwrap(),
            serde_json::to_string(&c.workers.resume(w.0).await.unwrap()).unwrap(),
            serde_json::to_string(&c.workers.drain(w.0).await.unwrap()).unwrap(),
            serde_json::to_string(&c.workers.update(w.0, patch).await.unwrap()).unwrap(),
        ];
        bodies.push(serde_json::to_string(&c.workers.revoke(w.0).await.unwrap()).unwrap());
        for b in &bodies {
            assert!(!b.contains(&token) && !b.contains(&token[4..60]), "{b}");
        }
        c.stop().await;
    }

    /// Claims `n` keys (first byte 200, then the index) and reports each
    /// one a floor drop (`Lost(Blocked)`) under its token.
    async fn report_floor_drops(s: &WorkerSession, n: usize) {
        let items: Vec<ClaimItem> = (0..n)
            .map(|i| {
                let mut key = [200u8; 20];
                key[1..9].copy_from_slice(&(i as u64).to_le_bytes());
                ClaimItem {
                    key,
                    source: 0,
                    strategy: 0,
                    discovered_ms: unix_now() * 1000,
                    hints: vec![],
                }
            })
            .collect();
        let grants = s.claim(items).await.unwrap();
        let reports: Vec<OutcomeItem> = grants
            .iter()
            .map(|g| match g {
                GrantItem::Granted { key, token, .. } => OutcomeItem {
                    key: *key,
                    token: *token,
                    outcome: Outcome::Lost {
                        reason: lodestar_sources::Loss::Blocked as u8,
                    },
                },
                GrantItem::Denied { .. } => panic!("denied: {g:?}"),
            })
            .collect();
        s.report(reports).await.unwrap();
    }

    /// #607's delta review: the first floor report over a worker's budget
    /// pauses it, takes its leases back, and saves the pause, so Workers
    /// shows it paused and a new server (or a restart) keeps it paused.
    #[tokio::test]
    async fn a_worker_over_its_floor_budget_stays_paused() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, _) = c.join("w1").await;
        let w = creds.worker();
        let s = c.connect(&creds).await;
        c.wait_status(w, StatusDto::Online).await;
        // Holding a lease it won't report: evicted with the rest.
        assert!(granted(&s.claim(claim(9)).await.unwrap()));
        report_floor_drops(&s, crate::cluster::serve::FLOOR_BUDGET + 1).await;
        let workers = &c.workers;
        until("saved as paused", || async move {
            (workers.saved(w).paused, ())
        })
        .await;
        c.wait_status(w, StatusDto::Paused).await;
        let h = c.cluster.handle();
        let leases = |server: &ClusterServer| {
            server
                .snapshot()
                .workers
                .iter()
                .find(|x| x.worker == w)
                .map(|x| x.leases)
        };
        assert_eq!(leases(&h.server().unwrap()), Some(0), "leases taken back");
        // The row is in the store, so it outlives this process.
        let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        let row = store.settings_with_prefix(PREFS_PREFIX).unwrap();
        assert!(
            row.iter()
                .any(|(_, t)| Saved::parse(t).is_some_and(|s| s.paused))
        );
        drop(store);
        // A new server (workers off and on) has it paused before serving.
        drop(s);
        h.set_allowed(false);
        until("off", || {
            let h = h.clone();
            async move { (h.server().is_none(), ()) }
        })
        .await;
        h.set_allowed(true);
        let new = until("on again", || {
            let h = h.clone();
            async move {
                let s = h.server();
                (s.is_some(), s)
            }
        })
        .await
        .unwrap();
        assert!(
            new.snapshot()
                .workers
                .iter()
                .any(|x| x.worker == w && x.paused)
        );
        let s = c.connect(&creds).await;
        assert!(!granted(&s.claim(claim(5)).await.unwrap()));
        drop(s);
        c.stop().await;
    }

    /// #607's review: the over-budget save orders with an admin's calls. A
    /// worker revoked before it runs isn't saved again; one an admin
    /// resumed before it runs is paused again, saved and live alike.
    #[tokio::test]
    async fn the_floor_pause_save_follows_a_resume_or_a_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let server = c.cluster.handle().server().unwrap();
        let paused = |w: WorkerId| {
            server
                .snapshot()
                .workers
                .iter()
                .any(|x| x.worker == w && x.paused)
        };

        // The sink paused it; the admin resumed it; then the save runs.
        let (one, _) = c.join("w1").await;
        let w1 = one.worker();
        assert!(server.pause(w1));
        c.workers.resume(w1.0).await.unwrap();
        assert!(!paused(w1));
        assert_eq!(c.workers.save_floor_pause(w1).await, None);
        assert!(
            c.workers.saved(w1).paused && paused(w1),
            "saved and live agree"
        );

        // The sink paused it; the admin revoked it; then the save runs.
        let (two, _) = c.join("w2").await;
        let w2 = two.worker();
        assert!(server.pause(w2));
        c.workers.revoke(w2.0).await.unwrap();
        // Removed meanwhile: not saved, and said so (#615's review).
        assert_eq!(c.workers.save_floor_pause(w2).await, Some("unknown_worker"));
        assert!(!c.workers.saved(w2).paused);
        let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        assert!(
            store
                .settings_with_prefix(PREFS_PREFIX)
                .unwrap()
                .iter()
                .all(|(k, _)| *k != prefs_key(w2))
        );
        drop(store);
        c.stop().await;
    }

    /// #607's delta review: revoking a worker forgets its floor reports not
    /// written yet and its budget window.
    #[tokio::test]
    async fn revoking_a_worker_forgets_its_floor_reports() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, _) = c.join("w1").await;
        let w = creds.worker();
        let h = c.cluster.handle();
        // A budget used up: its next report would be over it.
        for i in 0..crate::cluster::serve::FLOOR_BUDGET {
            assert!(h.floor_report_for_test(w, i as u8, i / 256));
        }
        assert!(!h.floor_report_for_test(w, 255, 999));
        c.workers.revoke(w.0).await.unwrap();
        assert_eq!(h.floor_pending_for_test(w), 0);
        assert!(h.floor_report_for_test(w, 1, 1000), "a fresh window");
        c.stop().await;
    }

    #[tokio::test]
    async fn a_revoked_worker_is_gone_within_one_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, token) = c.join("w1").await;
        let w = creds.worker();
        let mut s = c.connect(&creds).await;
        c.wait_status(w, StatusDto::Online).await;
        assert!(granted(&s.claim(claim(1)).await.unwrap()));
        assert!(granted(&s.claim(claim(2)).await.unwrap()));
        c.workers
            .update(
                w.0,
                WorkerPatch {
                    pool_cap: Some(1234),
                    ..WorkerPatch::default()
                },
            )
            .await
            .unwrap();

        let heartbeat = c.cluster.handle().server().unwrap().heartbeat_interval();
        // The production heartbeat (§19.2, 5 s): the gate means what it
        // says, and the bound below is relative to it.
        assert_eq!(
            heartbeat,
            lodestar_cluster::ServerConfig::default().heartbeat
        );
        let start = Instant::now();
        let r = c.workers.revoke(w.0).await.unwrap();
        assert_eq!(
            (r.id, r.leases_returned, r.name.as_deref()),
            (w.0, 2, Some("w1"))
        );
        assert!(r.certificates >= 1);
        assert_eq!(u64::from(r.gone_within_secs), heartbeat.as_secs());
        // Its session ends, revoked, within one heartbeat.
        let ended = tokio::time::timeout(DEADLINE, async {
            loop {
                match s.next_event().await {
                    Some(WorkerEvent::Ended(reason)) => return reason,
                    None => return None,
                    Some(_) => {}
                }
            }
        })
        .await
        .expect("the session ended");
        assert_eq!(ended, Some(StopReason::Revoked));
        assert!(start.elapsed() < heartbeat, "took {:?}", start.elapsed());
        assert_eq!(c.status(w).await, None, "not listed");
        assert_eq!(
            c.workers.pause(w.0).await.unwrap_err(),
            WorkersError::UnknownWorker
        );
        // Its saved row is gone, and it can't come back.
        let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        assert!(store.settings_with_prefix(PREFS_PREFIX).unwrap().is_empty());
        assert!(
            lodestar_cluster::connect(&c.addr(), &creds, Arc::new(SystemClock), DEADLINE)
                .await
                .is_err()
                || {
                    let link = lodestar_cluster::connect(
                        &c.addr(),
                        &creds,
                        Arc::new(SystemClock),
                        DEADLINE,
                    )
                    .await
                    .unwrap();
                    WorkerSession::start(
                        link,
                        Hello::new(w.0, Capabilities::default()),
                        Arc::new(SystemClock),
                        WorkerConfig::default(),
                    )
                    .await
                    .is_err()
                }
        );
        // The token was in no log line.
        let log = crate::test_log::text();
        assert!(log.contains("event=\"workers.token_created\""), "{log}");
        assert!(
            log.contains(&format!(
                "worker={} leases=2 event=\"workers.revoked\"",
                w.0
            )),
            "{log}"
        );
        assert!(
            !log.contains(&token) && !log.contains(&token[4..60]),
            "{log}"
        );
        c.stop().await;
    }

    #[tokio::test]
    async fn settings_and_pauses_survive_a_coordinator_restart() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, token) = c.join("w1").await;
        let w = creds.worker();
        let s = c.connect(&creds).await;
        c.wait_status(w, StatusDto::Online).await;
        let patch = WorkerPatch {
            budget: Some(WorkerBudget {
                cpu_permille: 250,
                down_kbps: 4000,
                ram_mb: 512,
                up_kbps: 1000,
            }),
            dht: Some(PinDto::Off),
            intensity: Some(WorkerIntensity::Low),
            pool_cap: Some(1234),
        };
        let after = c.workers.update(w.0, patch.clone()).await.unwrap();
        assert_eq!((after.pool_cap, after.dht), (1234, PinDto::Off));
        c.workers.pause(w.0).await.unwrap();
        // No response but the create carries the token.
        let listed = serde_json::to_string(&c.workers.list().await.unwrap()).unwrap();
        assert!(!listed.contains(&token), "{listed}");
        drop(s);
        c.stop().await;

        // Restarted: the row is read back and applied.
        let c = coordinator(dir.path(), true).await;
        let back = c.status(w).await.expect("known by its certificate");
        assert_eq!(back.status, StatusDto::Offline);
        assert_eq!(
            (back.pool_cap, back.dht, back.intensity, back.budget),
            (
                1234,
                PinDto::Off,
                WorkerIntensity::Low,
                patch.budget.unwrap()
            )
        );
        let mut s = c.connect(&creds).await;
        // Paused before it could claim anything.
        assert!(!granted(&s.claim(claim(9)).await.unwrap()));
        let events = queued(&mut s).await;
        assert!(!events.contains(&WorkerEvent::Drain), "{events:?}");
        assert_eq!(c.wait_status(w, StatusDto::Paused).await.leases, 0);
        // The planner has its settings for when it joins.
        let server = c.cluster.handle().server().unwrap();
        c.workers.placement().sync(&server.snapshot(), &server);
        let a = c.workers.placement().assignment(w).expect("planned");
        assert_eq!((a.pool_cap, a.dht), (1234, None));
        assert_eq!(a.budget.up_kbps, 1000);
        c.stop().await;
    }

    /// A revoke removes that worker's written floor reports and nothing
    /// else: another worker's report and a local or user row stay.
    #[tokio::test]
    async fn a_revoke_removes_only_that_workers_floor_reports() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, _) = c.join("w1").await;
        let w = creds.worker();
        let other = WorkerId(w.0 + 100);
        {
            let mut store =
                Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
            store.tombstone_from_worker(&[[1; 20]], w.0, 1).unwrap();
            store.tombstone_from_worker(&[[2; 20]], other.0, 1).unwrap();
        }
        c.workers.revoke(w.0).await.unwrap();
        let store = Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        assert_eq!(
            store.held_among(&[[1; 20], [2; 20]]).unwrap(),
            vec![false, true]
        );
        drop(store);
        c.stop().await;
    }

    #[tokio::test]
    async fn a_new_cluster_server_gets_saved_pauses_without_an_api_call() {
        let dir = tempfile::tempdir().unwrap();
        let c = coordinator(dir.path(), true).await;
        let (creds, _) = c.join("w1").await;
        let w = creds.worker();
        c.workers.pause(w.0).await.unwrap();
        // Workers off, then on: a new cluster server with an empty fleet.
        let h = c.cluster.handle();
        let old = h.server().unwrap();
        h.set_allowed(false);
        until("off", || {
            let h = h.clone();
            async move { (h.server().is_none(), ()) }
        })
        .await;
        h.set_allowed(true);
        let new = until("on again", || {
            let h = h.clone();
            async move {
                let s = h.server();
                (s.is_some(), s)
            }
        })
        .await
        .unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        // No `Workers` call from here: only the follower can pause it.
        until("pause re-applied", || {
            let new = new.clone();
            async move {
                let paused = new
                    .snapshot()
                    .workers
                    .iter()
                    .any(|s| s.worker == w && s.paused);
                (paused, ())
            }
        })
        .await;
        let s = c.connect(&creds).await;
        assert!(!granted(&s.claim(claim(5)).await.unwrap()));
        drop(s);
        c.stop().await;
    }

    #[tokio::test]
    async fn a_bad_row_falls_back_to_the_defaults_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store =
                Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
            let bad = Saved {
                pool_cap: 0,
                ..Saved::default()
            };
            store
                .update_settings_text(
                    &[
                        ("workers.prefs.7", &bad.text()),
                        ("workers.prefs.8", "not json"),
                        ("workers.prefs.x", "{}"),
                    ],
                    &[],
                )
                .unwrap();
        }
        let c = coordinator(dir.path(), false).await;
        assert!(lock(&c.workers.saved).is_empty());
        let log = crate::test_log::text();
        for w in [7, 8] {
            let line = format!("worker={w} event=\"workers.prefs_skipped\"");
            assert!(log.contains(&line), "{log}");
        }
        c.stop().await;
    }

    #[test]
    fn rows_round_trip() {
        let mut s = Saved::default();
        s.apply(&WorkerPatch {
            dht: Some(PinDto::Off),
            intensity: Some(WorkerIntensity::Aggressive),
            pool_cap: Some(7),
            ..WorkerPatch::default()
        });
        s.paused = true;
        assert_eq!(Saved::parse(&s.text()), Some(s));
        assert_eq!(s.pin(), DhtPin::Off);
        assert_eq!(s.settings().pool_cap, 7);
        assert_eq!(Saved::parse(r#"{"v":2}"#), None);
    }
}
