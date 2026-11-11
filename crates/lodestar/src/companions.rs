//! The companion manager in the binary (CMP-5; plan
//! 2026-11-06-v06-companions §4.3; decisions
//! `docs/decisions/2026-11-08-CMP-5.md`):
//!
//! - [`StoreRows`]: companion credential rows in `settings`, through the
//!   store, each write one committed transaction;
//! - [`host`]: the platform, and whether Lodestar runs in a container (Q17);
//! - [`start`]: at serve, journal recovery for every companion before
//!   anything else touches them, and the snapshot channel CMP-10 reads,
//!   with whether any download client is set up.
//!
//! - [`Wiring`]: at serve, CMP-4a's supervisor in CMP-4b's sandbox
//!   ([`Supervised`]) and CMP-8b's configuration ([`Api`]), handed to the
//!   manager once it holds the companion lock; every installed companion
//!   is then started (CMP-5g-3). The CLI's manager and a container's stay
//!   [`Unwired`], which refuses every start.
//! - [`CompanionRoutes`]: each companion's route (Q18), from its
//!   `companions.<id>.egress` setting, set from the plugin that needs it
//!   when it's first started, so nothing runs on a route nobody chose.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lodestar_api::companions::{CompanionsApi, CompanionsError, Revealed};
use lodestar_api::dto::companions::{
    Companion, CompanionHealth, CompanionTemplate, InstallOffer, RemovalItem, RemovalItemKind,
    RemovalPlan, TemplateKind, TemplateRequest, TemplateRoute, UnraidField, UnraidFile,
};
use lodestar_companions::adapter::{AdapterError, BoxFuture, Credentials, Target};
use lodestar_companions::catalogue::{Sha256, TemplateId};
use lodestar_companions::compose::{
    self, DataFolder, DataFolders, MountRole, Paths, ServiceInput, TemplateError,
};
use lodestar_companions::configure::{self, InstallReceipt, Socks5Proxy};
use lodestar_companions::credentials::{self, CredentialKey, Field, SealedRow};
use lodestar_companions::detect::CompanionHttp;
use lodestar_companions::fetch::{self, Guarded};
use lodestar_companions::inputs::{ContainerName, HostPath, NetworkName, Pgid, Puid};
use lodestar_companions::log_events::INSTALL_FAILED;
use lodestar_companions::manager::{
    Api, ApiConfigure, Downloader, Health, Host, InstallToken, Launch, Manager, ManagerError,
    Parts, Refusal, Routes, RowError, Rows, Run, Snapshot, Supervised, field_of, monitored,
};
use lodestar_companions::supervise::egress::Route as GateRoute;
use lodestar_companions::supervise::logs::Redactor;
use lodestar_companions::supervise::{Launcher, Limits, Sandbox, Status, Supervisor, Timings};
use lodestar_companions::{Catalogue, CompanionId, Entry, Version, unraid};
use lodestar_integrations::net::Route;
use lodestar_integrations::secret::{SealedSecret, Secret, SecretStore};
use lodestar_pipeline::control::{EgressSetting, SavedEgress, Unreadable};
use lodestar_sources::egress::{
    DownReason, Egress, EgressFactory, Endpoints, NoCredentials, RouteKind,
};
use lodestar_store::Store;
use tokio::sync::watch;

/// A `settings` row as the store reads it: text, or the bytes of a blob.
type RawRow = (String, Result<String, Vec<u8>>);

/// Companion credential rows in the store's `settings` table.
#[derive(Debug, Clone)]
pub struct StoreRows(pub Arc<Mutex<Store>>);

impl Rows for StoreRows {
    fn put(&self, row: &SealedRow) -> Result<(), RowError> {
        let mut s = self.0.lock().map_err(|_| RowError)?;
        s.update_settings_raw(&[(&row.key, row.value.as_bytes())], &[])
            .map_err(|_| RowError)
    }

    fn get(&self, key: &str) -> Result<Option<SealedSecret>, RowError> {
        let s = self.0.lock().map_err(|_| RowError)?;
        let rows = s.settings_with_prefix_raw(key).map_err(|_| RowError)?;
        match rows.into_iter().find(|(k, _)| k == key) {
            None => Ok(None),
            Some((_, Err(bytes))) => SealedSecret::from_bytes(bytes)
                .map(Some)
                .map_err(|_| RowError),
            Some((_, Ok(_))) => Err(RowError),
        }
    }

    fn forget(&self, id: &CompanionId) -> Result<(), RowError> {
        let keys: Vec<String> = Field::ALL
            .iter()
            .map(|f| CredentialKey::new(id.clone(), *f).row_key())
            .collect();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let mut s = self.0.lock().map_err(|_| RowError)?;
        s.update_settings_raw(&[], &keys).map_err(|_| RowError)
    }
}

/// This platform, and whether it's a container. Stricter than the About
/// screen's detection, which lets `LODESTAR_DEPLOYMENT` say otherwise:
/// here any sign of one refuses native installs (Q17).
pub fn host() -> Host {
    let hinted =
        std::env::var("LODESTAR_DEPLOYMENT").is_ok_and(|d| matches!(d.trim(), "docker" | "unraid"));
    Host::this(hinted || in_container(|p| std::fs::read(p).ok()))
}

/// Docker's `/.dockerenv`, Podman's `/run/.containerenv`, a container
/// cgroup (v1), or `container=` in PID 1's environment (LXC,
/// systemd-nspawn, Podman). On cgroup v2 the cgroup alone says nothing.
fn in_container(read: impl Fn(&str) -> Option<Vec<u8>>) -> bool {
    let has = |p: &str| read(p).is_some();
    let cgroup = read("/proc/1/cgroup").is_some_and(|c| {
        let c = String::from_utf8_lossy(&c);
        c.contains("/docker") || c.contains("/kubepods") || c.contains("/containerd")
    });
    let env = read("/proc/1/environ")
        .is_some_and(|e| e.split(|b| *b == 0).any(|v| v.starts_with(b"container=")));
    has("/.dockerenv") || has("/run/.containerenv") || cgroup || env
}

/// Refuses every start: the CLI's manager, a container's, and serve's
/// when the supervisor couldn't be built. Stopping is a no-op: nothing
/// was started.
#[derive(Debug)]
pub struct Unwired;

impl Run for Unwired {
    fn ready(&self) -> Result<(), &'static str> {
        Err("not_wired")
    }
    fn start(&self, _: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        Box::pin(async { Err("not_wired") })
    }
    fn healthy<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn stop<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl ApiConfigure for Unwired {
    fn configure<'a>(
        &'a self,
        _: &'a CompanionId,
        _: &'a Target,
        _: InstallReceipt,
        _: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async { Err(AdapterError::Refused("not_wired")) })
    }
}

/// The manager over this data folder and store, for the CLI. Downloads go
/// direct, as the updater's do (CMP-1's decision); nothing is started
/// ([`Unwired`]). Blocking (the store, the secrets file, the lock): async
/// callers use `spawn_blocking`.
pub fn manager(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    catalogue: &'static Catalogue,
) -> Result<Manager, OpenError> {
    manager_on(data, store, catalogue, host())
}

fn manager_on(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    catalogue: &'static Catalogue,
    host: Host,
) -> Result<Manager, OpenError> {
    let unwired = Arc::new(Unwired);
    manager_with(data, store, catalogue, host, unwired.clone(), unwired).map(|(m, _)| m)
}

/// [`manager_on`], with the secrets file the manager seals with, for the
/// API's reveal.
fn manager_with(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    catalogue: &'static Catalogue,
    host: Host,
    run: Arc<dyn Run>,
    api: Arc<dyn ApiConfigure>,
) -> Result<(Manager, Arc<SecretStore>), OpenError> {
    let fail = |code: &'static str, message: String| OpenError { code, message };
    let secrets =
        SecretStore::open_or_create(&data).map_err(|e| fail("secrets", format!("secrets: {e}")))?;
    let client = fetch::client(&Route::Direct).map_err(|e| fail(e.code(), e.code().to_owned()))?;
    let downloader =
        Downloader::new(Guarded::new(client).map_err(|e| fail(e.code(), e.code().to_owned()))?);
    let secrets = Arc::new(secrets);
    Manager::open(Parts {
        data,
        host,
        catalogue,
        downloader,
        secrets: secrets.clone(),
        rows: Arc::new(StoreRows(store)),
        run,
        api,
    })
    .map_err(|e| match e {
        lodestar_companions::manager::ManagerError::Refused(_) => fail(
            "busy",
            "another Lodestar process is managing companions".to_owned(),
        ),
        e => fail(e.code(), e.code().to_owned()),
    })
    .map(|m| (m, secrets))
}

/// Why the manager couldn't be opened: a stable code for the log, and a
/// sentence for the CLI.
#[derive(Debug)]
pub struct OpenError {
    pub code: &'static str,
    pub message: String,
}

/// What serve keeps: the manager, the snapshot channel, and what runs
/// the companions.
#[derive(Debug)]
pub struct Companions {
    /// `/companions` (CMP-6), layered over the API.
    pub api: Arc<Service>,
    /// The installed companions, which `companion_wiring` folds into the
    /// pipeline's companion gate (CMP-5g-1).
    pub snapshot: watch::Receiver<Snapshot>,
    wiring: Arc<Wiring>,
    /// Following `integrations.clients`, and starting the installed
    /// companions: both stopped at shutdown.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Companions {
    /// Serve's shutdown (#619 N3): stops following the integrations and
    /// starting companions, then stops every companion, each bounded.
    pub async fn shutdown(self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(s) = self.wiring.supervised.get() {
            s.shutdown().await;
        }
    }
}

/// At serve: recovers what a crash interrupted, publishes the installed
/// companions, starts each one Lodestar installed, and follows whether any
/// download client is set up. A bad catalogue turns companions off for the
/// run (CMP-0); a failed recovery is logged by code and leaves that
/// companion as it is.
pub async fn start(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    integrations: watch::Receiver<crate::settings::integrations::Integrations>,
) -> Option<Companions> {
    let catalogue = lodestar_companions::builtin().ok()?;
    start_with(data, store, integrations, catalogue, host()).await
}

async fn start_with(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    mut integrations: watch::Receiver<crate::settings::integrations::Integrations>,
    catalogue: &'static Catalogue,
    host: Host,
) -> Option<Companions> {
    let rows = StoreRows(store.clone());
    let folder = data.clone();
    let wiring = Arc::new(Wiring::default());
    let routes = Arc::new(CompanionRoutes::new(store.clone()));
    let (run, api) = (wiring.clone(), wiring.clone());
    let opened =
        tokio::task::spawn_blocking(move || manager_with(data, store, catalogue, host, run, api))
            .await
            .unwrap_or_else(|_| {
                Err(OpenError {
                    code: "panicked",
                    message: String::new(),
                })
            });
    let (manager, secrets) = match opened {
        Ok((m, secrets)) => (Arc::new(m), secrets),
        Err(e) => {
            // Companions are off for this run.
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = "all",
                version = "",
                step = "open",
                reason = e.code
            );
            return None;
        }
    };
    // Only once this process holds the companion lock: the supervisor
    // kills companions an earlier run left behind. Nothing is native in a
    // container (Q17).
    if !host.in_container {
        match supervise(&folder, routes).await {
            Ok((supervised, api)) => wiring.wire(supervised, api),
            Err(code) => lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = "all",
                version = "",
                step = "supervise",
                reason = code
            ),
        }
    }
    let mut started = Vec::new();
    for (id, r) in manager.recover().await {
        match r {
            Ok(Some(_)) => started.push(id),
            Ok(None) => {}
            Err(e) => {
                let companion = catalogue
                    .get(id.as_str())
                    .map_or("unknown", |e| e.id.as_str());
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = companion,
                    version = "",
                    step = "recover",
                    reason = e.code()
                );
            }
        }
    }
    let mut tasks = Vec::new();
    if wiring.supervised.get().is_some() {
        // One at a time, in the background: each waits for its health.
        // A failure is logged by the manager (step `start`).
        let m = manager.clone();
        tasks.push(tokio::spawn(async move {
            for id in started {
                let _ = m.start(&id).await;
            }
        }));
    }
    let m = manager.clone();
    m.set_download_clients(!integrations.borrow_and_update().clients.is_empty());
    tasks.push(tokio::spawn(async move {
        while integrations.changed().await.is_ok() {
            let any = !integrations.borrow_and_update().clients.is_empty();
            m.set_download_clients(any);
        }
    }));
    let snapshot = manager.snapshot();
    let live: Arc<dyn Live> = match wiring.supervised.get() {
        Some(s) => Arc::new(Supervision {
            manager: manager.clone(),
            supervised: s.clone(),
        }),
        None => Arc::new(Unwired),
    };
    let api = Arc::new(Service {
        manager: manager.clone(),
        catalogue,
        host,
        data: folder,
        secrets,
        rows,
        live,
    });
    Some(Companions {
        api,
        snapshot,
        wiring,
        tasks,
    })
}

/// The supervisor in its sandbox, with the guarded client, and CMP-8b's
/// configuration on each companion's route. A failure's code otherwise.
async fn supervise(
    data: &Path,
    routes: Arc<CompanionRoutes>,
) -> Result<(Arc<Supervised>, Api), &'static str> {
    let launcher = Launcher::lodestar().map_err(|e| e.code())?;
    let sandbox = Sandbox::new(Limits::default(), launcher).map_err(|e| e.code())?;
    let supervisor = Supervisor::new(data.to_path_buf(), sandbox.hook(), Timings::default())
        .await
        .map_err(|e| e.code())?;
    let http: Arc<dyn lodestar_companions::adapter::GuardedHttp> =
        Arc::new(CompanionHttp::new().map_err(|_| "companion_http")?);
    let api = Api {
        http: http.clone(),
        route: Arc::new({
            let routes = routes.clone();
            move |id: &CompanionId| routes.configured(id)
        }),
    };
    let supervised = Supervised::new(data.to_path_buf(), supervisor, sandbox, http, routes, None);
    Ok((Arc::new(supervised), api))
}

/// What the manager starts and configures with at serve: [`Supervised`]
/// and [`Api`], once [`supervise`] has built them; until then, or if it
/// couldn't, it refuses as [`Unwired`] does.
#[derive(Default)]
struct Wiring {
    supervised: OnceLock<Arc<Supervised>>,
    api: OnceLock<Api>,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("wired", &self.supervised.get().is_some())
            .finish_non_exhaustive()
    }
}

impl Wiring {
    fn wire(&self, supervised: Arc<Supervised>, api: Api) {
        // Configuration first: nothing starts until both are set.
        let _ = self.api.set(api);
        let _ = self.supervised.set(supervised);
    }
}

impl Run for Wiring {
    fn ready(&self) -> Result<(), &'static str> {
        match (self.supervised.get(), self.api.get()) {
            (Some(s), Some(_)) => s.ready(),
            _ => Err("not_wired"),
        }
    }
    fn start(&self, l: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        match self.supervised.get() {
            Some(s) => s.start(l),
            None => Box::pin(async { Err("not_wired") }),
        }
    }
    fn healthy<'a>(&'a self, id: &'a CompanionId, t: &'a Target) -> BoxFuture<'a, bool> {
        match self.supervised.get() {
            Some(s) => s.healthy(id, t),
            None => Box::pin(async { false }),
        }
    }
    fn stop<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, ()> {
        match self.supervised.get() {
            Some(s) => s.stop(id),
            None => Box::pin(async {}),
        }
    }
    fn status(&self, id: &CompanionId) -> Option<watch::Receiver<Status>> {
        self.supervised.get().and_then(|s| s.status(id))
    }
}

impl ApiConfigure for Wiring {
    fn configure<'a>(
        &'a self,
        id: &'a CompanionId,
        target: &'a Target,
        receipt: InstallReceipt,
        credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        match self.api.get() {
            Some(a) => a.configure(id, target, receipt, credentials),
            None => Box::pin(async { Err(AdapterError::Refused("not_wired")) }),
        }
    }
}

/// The `settings` row of a companion's route (Q18): the same text as a
/// source's `sources.egress.<plugin>` ([`EgressSetting::encode`]):
/// `direct`, or `socks5 <host> <port> <auto|proxy|local>`. Not a
/// registry setting, so a settings import can't write it (only registry
/// sections are imported).
pub fn egress_key(id: &CompanionId) -> String {
    format!(
        "{}{id}.egress",
        lodestar_companions::credentials::ROW_PREFIX
    )
}

/// The source plugin a companion serves, whose egress it's set from at
/// its first start (Q18): the download-client source for qBittorrent,
/// the Prowlarr plugin for Prowlarr and FlareSolverr. The ids are
/// CMP-10's (`lodestar_companions::requires`).
fn plugin_for(id: &CompanionId) -> Option<&'static str> {
    match id.as_str() {
        "qbittorrent" => Some("download_clients"),
        "prowlarr" | "flaresolverr" => Some("prowlarr"),
        _ => None,
    }
}

/// `id`'s saved route. With no row yet, the route of the plugin it serves
/// is saved as its own and used (Q18: "set at install to the egress of
/// the plugin that needs it"); an unreadable one of the plugin's is used
/// but not saved, so it's read again next time. A row that doesn't decode
/// is [`Unreadable`]: the route stays down, never direct. `None` when the
/// store can't be read or written, or it serves no plugin: it isn't
/// started. Blocking.
fn egress_setting(store: &Mutex<Store>, id: &CompanionId) -> Option<SavedEgress> {
    let key = egress_key(id);
    let mut s = store.lock().ok()?;
    if let Some(text) = s.setting_text(&key).ok()? {
        return Some(EgressSetting::decode(&text).ok_or(Unreadable));
    }
    let plugin = plugin_for(id)?;
    let saved = lodestar_pipeline::control::saved_settings(&s, plugin, false)
        .ok()?
        .egress;
    if let Ok(e) = &saved {
        s.update_settings_text(&[(&key, &e.encode())], &[]).ok()?;
    }
    Some(saved)
}

/// Each companion's route (Q18), built for every start from its saved
/// setting ([`egress_setting`]) through its own [`EgressFactory`], and
/// kept fresh by [`monitored`] (CMP-5's #608 follow-up). The route a
/// companion is configured for (its bind or proxy setting, CMP-8b) is the
/// one its last start was built with.
pub struct CompanionRoutes {
    store: Arc<Mutex<Store>>,
    /// No SOCKS5 credentials: a companion's own proxy setting has none
    /// (CMP-8b's `Socks5Proxy` is an address), and the probe only connects.
    factory: EgressFactory,
    configured: Mutex<HashMap<CompanionId, configure::Route>>,
}

impl std::fmt::Debug for CompanionRoutes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanionRoutes").finish_non_exhaustive()
    }
}

impl CompanionRoutes {
    pub fn new(store: Arc<Mutex<Store>>) -> CompanionRoutes {
        CompanionRoutes {
            store,
            factory: EgressFactory::new(Arc::new(NoCredentials)),
            configured: Mutex::new(HashMap::new()),
        }
    }

    /// CMP-8b's form of the route `id` was last started on. `None` when it
    /// was built from an unreadable setting or a proxy that didn't
    /// resolve: that route is down for good, so the companion never runs.
    fn configured(&self, id: &CompanionId) -> Option<configure::Route> {
        self.configured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }
}

impl Routes for CompanionRoutes {
    fn route<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, Option<GateRoute>> {
        Box::pin(async move {
            let (store, cid) = (self.store.clone(), id.clone());
            let saved = tokio::task::spawn_blocking(move || egress_setting(&store, &cid))
                .await
                .ok()??;
            let label = format!("companion.{id}");
            // The probe is all that's used: the companion makes its own
            // connections, and this client may reach nothing.
            let none = || Endpoints::manifest(&[]);
            let (egress, config) = match &saved {
                Ok(EgressSetting::Direct) => (
                    self.factory.build(&label, &Egress::Direct, none()).await,
                    Some(configure::Route::Direct),
                ),
                Ok(EgressSetting::Socks5(spec)) => {
                    let (e, built) = self.factory.build_socks5(&label, spec, none()).await;
                    let config = match built {
                        Some(Egress::Socks5 { proxy, .. }) => {
                            Socks5Proxy::new(proxy).ok().map(configure::Route::Socks5)
                        }
                        _ => None,
                    };
                    (e, config)
                }
                Err(Unreadable) => (
                    self.factory.unavailable(
                        &label,
                        RouteKind::Socks5,
                        DownReason::SettingUnreadable,
                        none(),
                    ),
                    None,
                ),
            };
            {
                let mut c = self.configured.lock().unwrap_or_else(|e| e.into_inner());
                match config {
                    Some(r) => c.insert(id.clone(), r),
                    None => c.remove(id),
                };
            }
            // The relay stops once the gate and the supervisor's copy of
            // this route are dropped, at the companion's stop.
            let (route, _relay) =
                monitored(Arc::new(egress), lodestar_pipeline::EGRESS_PROBE_EVERY);
            Some(route)
        })
    }
}

/// [`Live`] over the manager and the supervisor.
struct Supervision {
    manager: Arc<Manager>,
    supervised: Arc<Supervised>,
}

impl Live for Supervision {
    fn restart<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, Result<(), CompanionsError>> {
        Box::pin(async move { self.manager.restart(id).await.map(drop).map_err(refused) })
    }
    fn logs(&self, id: &CompanionId) -> Result<Vec<String>, CompanionsError> {
        Ok(self.supervised.logs(id).unwrap_or_default())
    }
    fn regenerate<'a>(
        &'a self,
        id: &'a CompanionId,
        _: Field,
    ) -> BoxFuture<'a, Result<(), CompanionsError>> {
        Box::pin(async move { self.manager.regenerate(id).await.map(drop).map_err(refused) })
    }
}

/// What only a running supervisor can do ([`Supervision`] at serve):
/// restart, recent output and regenerating a credential, which rewrites
/// the companion's own config and restarts it. Restart and regenerate
/// answer once they've begun (the API's `202`); the manager refuses
/// `busy` while another operation on it runs. [`Service`] calls them only
/// for a companion Lodestar installed. [`Unwired`] refuses each with
/// `not_wired`.
pub trait Live: Send + Sync {
    fn restart<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, Result<(), CompanionsError>>;
    /// Its recent output, as the supervisor's ring holds it (redacted
    /// once there; [`Service`] redacts it again). Empty when it hasn't
    /// been started in this run of Lodestar.
    fn logs(&self, id: &CompanionId) -> Result<Vec<String>, CompanionsError>;
    fn regenerate<'a>(
        &'a self,
        id: &'a CompanionId,
        field: Field,
    ) -> BoxFuture<'a, Result<(), CompanionsError>>;
}

impl Live for Unwired {
    fn restart<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), CompanionsError>> {
        Box::pin(async { Err(CompanionsError::Refused("not_wired")) })
    }
    fn logs(&self, _: &CompanionId) -> Result<Vec<String>, CompanionsError> {
        Err(CompanionsError::Refused("not_wired"))
    }
    fn regenerate<'a>(
        &'a self,
        _: &'a CompanionId,
        _: Field,
    ) -> BoxFuture<'a, Result<(), CompanionsError>> {
        Box::pin(async { Err(CompanionsError::Refused("not_wired")) })
    }
}

/// `/companions` over the manager (CMP-6), with `live` the supervisor at
/// serve.
pub struct Service {
    manager: Arc<Manager>,
    catalogue: &'static Catalogue,
    host: Host,
    /// Compared against in templates, never returned.
    data: PathBuf,
    secrets: Arc<SecretStore>,
    rows: StoreRows,
    live: Arc<dyn Live>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

/// The manager's answer as the API's stable code.
fn refused(e: ManagerError) -> CompanionsError {
    match e {
        ManagerError::Refused(Refusal::Unknown) => CompanionsError::Unknown,
        ManagerError::Refused(Refusal::Confirmation) => CompanionsError::Mismatch,
        ManagerError::Refused(r) => CompanionsError::Refused(r.code()),
        ManagerError::Step {
            code: "not_wired", ..
        } => CompanionsError::Refused("not_wired"),
        e => CompanionsError::Internal(e.code()),
    }
}

fn health(h: Health) -> CompanionHealth {
    match h {
        Health::Unknown => CompanionHealth::Unknown,
        Health::Healthy => CompanionHealth::Healthy,
        Health::Unhealthy => CompanionHealth::Unhealthy,
    }
}

fn field(name: &str) -> Result<Field, CompanionsError> {
    Field::ALL
        .into_iter()
        .find(|f| f.as_str() == name)
        .ok_or(CompanionsError::NoCredential)
}

fn bad(field: &'static str) -> impl Fn(lodestar_companions::inputs::InputError) -> CompanionsError {
    move |_| CompanionsError::BadInput {
        field: Some(field),
        code: "bad_template_input",
    }
}

/// A template refusal as the API's code. `path` names the folder a
/// folder rule refused, once [`Service::template`] has found which.
fn template_error(e: TemplateError, path: Option<&'static str>) -> CompanionsError {
    match e {
        TemplateError::UnknownCompanion => CompanionsError::Unknown,
        TemplateError::NoTemplate | TemplateError::NotAvailable(_) => {
            CompanionsError::Refused(e.code())
        }
        TemplateError::MissingPath(r) | TemplateError::UnexpectedPath(r) => {
            CompanionsError::BadInput {
                field: Some(role_field(r)),
                code: e.code(),
            }
        }
        TemplateError::DataFolder | TemplateError::OtherCompanion | TemplateError::DotFolder => {
            CompanionsError::BadInput {
                field: path,
                code: e.code(),
            }
        }
        TemplateError::Encode | TemplateError::DataFolderUnreadable => {
            CompanionsError::Internal(e.code())
        }
        // No one field is to blame (a character rule, a port rule).
        _ => CompanionsError::BadInput {
            field: None,
            code: e.code(),
        },
    }
}

fn role_field(r: MountRole) -> &'static str {
    match r {
        MountRole::Config => "config_dir",
        MountRole::Downloads => "downloads",
    }
}

/// Whether a folder rule refused it: it names a folder, not which.
fn folder_rule(e: TemplateError) -> bool {
    matches!(
        e,
        TemplateError::DataFolder | TemplateError::OtherCompanion | TemplateError::DotFolder
    )
}

/// Which folder a folder rule refused. CMP-7 checks the settings folder
/// first, so with the downloads folder set to it, the same refusal blames
/// the settings folder, and none (or another) blames downloads.
fn blame<T>(
    input: &ServiceInput,
    err: TemplateError,
    generate: impl Fn(&ServiceInput) -> Result<T, TemplateError>,
) -> &'static str {
    if input.paths.downloads.is_none() {
        return "config_dir";
    }
    let mut probe = input.clone();
    probe.paths.downloads = probe.paths.config.clone();
    match generate(&probe) {
        Err(again) if again == err => "config_dir",
        _ => "downloads",
    }
}

impl Service {
    fn entry(&self, id: &str) -> Result<&'static Entry, CompanionsError> {
        CompanionId::parse(id).map_err(|_| CompanionsError::Unknown)?;
        self.catalogue.get(id).ok_or(CompanionsError::Unknown)
    }

    /// Runs the manager's or the store's blocking work off the runtime.
    async fn off<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Service) -> Result<T, CompanionsError> + Send + 'static,
    ) -> Result<T, CompanionsError> {
        let me = self.clone_parts();
        tokio::task::spawn_blocking(move || f(&me))
            .await
            .map_err(|_| CompanionsError::Internal("panicked"))?
    }

    fn clone_parts(&self) -> Service {
        Service {
            manager: self.manager.clone(),
            catalogue: self.catalogue,
            host: self.host,
            data: self.data.clone(),
            secrets: self.secrets.clone(),
            rows: self.rows.clone(),
            live: self.live.clone(),
        }
    }

    /// The fields with a stored row. Blocking.
    fn stored(&self, id: &CompanionId) -> Result<Vec<Field>, CompanionsError> {
        let mut out = Vec::new();
        for f in Field::ALL {
            let key = CredentialKey::new(id.clone(), f).row_key();
            if self
                .rows
                .get(&key)
                .map_err(|_| CompanionsError::Internal("store"))?
                .is_some()
            {
                out.push(f);
            }
        }
        Ok(out)
    }

    /// Opens a stored credential. Blocking.
    fn open(&self, id: &CompanionId, f: Field) -> Result<Option<Secret>, CompanionsError> {
        let key = CredentialKey::new(id.clone(), f);
        let Some(sealed) = self
            .rows
            .get(&key.row_key())
            .map_err(|_| CompanionsError::Internal("store"))?
        else {
            return Ok(None);
        };
        credentials::open(&self.secrets, &key, &sealed)
            .map(Some)
            .map_err(|_| CompanionsError::Internal("sealed"))
    }

    /// One row of the list. Blocking (the journal, the store).
    fn row(&self, e: &'static Entry) -> Result<Companion, CompanionsError> {
        let present = self
            .manager
            .snapshot()
            .borrow()
            .companions
            .iter()
            .find(|c| c.id == e.id)
            .cloned();
        let (available, refusal) = match self.manager.offer(&e.id) {
            Ok(o) => (Some(o.version.as_str().to_owned()), None),
            Err(ManagerError::Refused(Refusal::Current)) => (None, None),
            Err(ManagerError::Refused(r)) => (None, Some(r.code().to_owned())),
            // A failed read isn't a reason it can't be installed.
            Err(err) => return Err(CompanionsError::Internal(err.code())),
        };
        let installed = present.as_ref().and_then(|p| p.version.clone());
        Ok(Companion {
            available,
            credentials: self
                .stored(&e.id)?
                .into_iter()
                .map(|f| f.as_str().to_owned())
                .collect(),
            health: installed
                .as_ref()
                .and(present.as_ref())
                .map(|p| health(p.health)),
            id: e.id.as_str().to_owned(),
            installed: installed.map(|v| v.as_str().to_owned()),
            licence: e.licence.spdx.clone(),
            name: e.name.clone(),
            opt_in: e.opt_in,
            refusal,
            templates: e
                .templates
                .iter()
                .filter_map(|t| match t {
                    TemplateId::ComposeService => Some(TemplateKind::Compose),
                    TemplateId::UnraidTemplate => Some(TemplateKind::Unraid),
                    _ => None,
                })
                .collect(),
        })
    }

    /// The secrets file credentials are sealed with (CMP-5g-2's paste).
    pub(crate) fn secrets(&self) -> Arc<SecretStore> {
        self.secrets.clone()
    }

    /// Whether Lodestar owns `id` here, for CMP-5g-2's guided endpoints:
    /// `Some("installed")` when its journal records an installed version,
    /// `Some("busy")` while an install is under way (so a guided step
    /// can't race CMP-5's first install), `None` otherwise. Inside a
    /// container nothing is native. Blocking.
    pub(crate) fn lodestar_owned(
        &self,
        id: &CompanionId,
    ) -> Result<Option<&'static str>, CompanionsError> {
        if self.host.in_container {
            return Ok(None);
        }
        let home = self.data.join("companions").join(id.as_str());
        match lodestar_companions::state::load(&home) {
            Ok(Some(r)) if r.installed.is_some() => Ok(Some("installed")),
            Ok(Some(r)) if r.op.is_some() => Ok(Some("busy")),
            Ok(_) => Ok(None),
            Err(_) => Err(CompanionsError::Internal("journal")),
        }
    }

    /// The host's container detection, for detection's runtime.
    pub(crate) fn in_container(&self) -> bool {
        self.host.in_container
    }

    /// The catalogue it serves.
    pub(crate) fn catalogue(&self) -> &'static Catalogue {
        self.catalogue
    }

    /// Whether Lodestar installed it: its own journal (CMP-5's
    /// `state.json`) records an installed version. Never the snapshot,
    /// which detected companions join with a version too (CMP-6's
    /// decisions). Inside a container nothing is native. Blocking.
    fn installed(&self, id: &CompanionId) -> Result<bool, CompanionsError> {
        if self.host.in_container {
            return Ok(false);
        }
        let home = self.data.join("companions").join(id.as_str());
        match lodestar_companions::state::load(&home) {
            Ok(rec) => Ok(rec.is_some_and(|r| r.installed.is_some())),
            Err(_) => Err(CompanionsError::Internal("journal")),
        }
    }

    /// `not_installed` unless Lodestar installed it: only then does its
    /// supervisor run it, and only then is its credential Lodestar's alone.
    async fn ours(&self, e: &'static Entry) -> Result<(), CompanionsError> {
        if self.off(move |s| s.installed(&e.id)).await? {
            Ok(())
        } else {
            Err(CompanionsError::Refused("not_installed"))
        }
    }

    /// Every companion row, read in one query: a provenance and the
    /// credential it describes are always seen together.
    fn companion_rows(&self) -> Result<Vec<RawRow>, CompanionsError> {
        self.rows
            .0
            .lock()
            .map_err(|_| CompanionsError::Internal("store"))?
            .settings_with_prefix_raw(lodestar_companions::credentials::ROW_PREFIX)
            .map_err(|_| CompanionsError::Internal("store"))
    }

    fn generated_in(rows: &[RawRow], id: &CompanionId, field: Field) -> bool {
        let text = |key: &str| {
            rows.iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.as_ref().ok().cloned())
        };
        lodestar_companions::guided::provenance(id, field, text)
            == Some(lodestar_companions::guided::Provenance::Generated)
    }

    /// The credential, when it's one Lodestar generated (Q5: only that is
    /// ever revealed): the companion is one Lodestar installed (its
    /// journal), or one you added whose credential's provenance row says
    /// `generated` (a guided qBittorrent's replaced password, CMP-5g-2). A
    /// pasted one never is. For a
    /// companion you added, the provenance and the sealed row come from
    /// the same read, so a paste landing in between can't be shown.
    /// Blocking.
    fn revealable(&self, id: &CompanionId, f: Field) -> Result<Option<Secret>, CompanionsError> {
        if self.installed(id)? {
            return self.open(id, f);
        }
        let rows = self.companion_rows()?;
        if !Self::generated_in(&rows, id, f) {
            return Ok(None);
        }
        let key = CredentialKey::new(id.clone(), f);
        let Some((_, Err(bytes))) = rows.into_iter().find(|(k, _)| *k == key.row_key()) else {
            return Ok(None);
        };
        let sealed =
            SealedSecret::from_bytes(bytes).map_err(|_| CompanionsError::Internal("sealed"))?;
        credentials::open(&self.secrets, &key, &sealed)
            .map(Some)
            .map_err(|_| CompanionsError::Internal("sealed"))
    }

    fn input(&self, r: &TemplateRequest) -> Result<ServiceInput, CompanionsError> {
        let path = |p: &Option<String>, f: &'static str| {
            p.as_deref()
                .map(HostPath::parse)
                .transpose()
                .map_err(bad(f))
        };
        let route = match r.route {
            TemplateRoute::Direct => compose::Route::Direct,
            TemplateRoute::Gluetun => compose::Route::Gluetun,
            TemplateRoute::ExistingGluetun => compose::Route::ExistingGluetun(
                ContainerName::parse(r.gluetun_container.as_deref().unwrap_or(""))
                    .map_err(bad("gluetun_container"))?,
            ),
        };
        if r.route != TemplateRoute::ExistingGluetun && r.gluetun_container.is_some() {
            return Err(CompanionsError::BadInput {
                field: Some("gluetun_container"),
                code: "bad_template_input",
            });
        }
        // A native install knows its own data folder, which a template
        // must never mount; in a container it's the container's path.
        // It fails closed on Unix. On Windows the folder is a drive path,
        // which no template's host folder (a `/` path) can name.
        let data = if self.host.in_container || cfg!(windows) {
            DataFolders::none()
        } else {
            DataFolders::none().with(
                DataFolder::from_running(&self.data)
                    .map_err(|_| CompanionsError::Internal("data_folder"))?,
            )
        };
        Ok(ServiceInput {
            puid: Puid::parse(&r.puid).map_err(bad("puid"))?,
            pgid: Pgid::parse(&r.pgid).map_err(bad("pgid"))?,
            network: NetworkName::parse(&r.network).map_err(bad("network"))?,
            route,
            paths: Paths {
                config: path(&r.config_dir, "config_dir")?,
                downloads: path(&r.downloads, "downloads")?,
            },
            publish: Vec::new(),
            data,
            taken: Vec::new(),
        })
    }
}

impl CompanionsApi for Service {
    fn list(&self) -> lodestar_api::ops::BoxFuture<'_, Result<Vec<Companion>, CompanionsError>> {
        Box::pin(self.off(|s| s.catalogue.entries().iter().map(|e| s.row(e)).collect()))
    }

    fn get(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Companion, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            self.off(move |s| s.row(e)).await
        })
    }

    fn offer(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<InstallOffer, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let o = self
                .off(move |s| s.manager.offer(&e.id).map_err(refused))
                .await?;
            Ok(InstallOffer {
                id: o.id.as_str().to_owned(),
                licence: o.licence,
                name: o.name,
                replaces: o.replaces.map(|v| v.as_str().to_owned()),
                sha256: o.sha256.to_hex(),
                size_bytes: o.size,
                version: o.version.as_str().to_owned(),
            })
        })
    }

    fn install(
        &self,
        id: String,
        version: String,
        sha256: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Companion, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let token = InstallToken {
                id: e.id.clone(),
                version: Version::parse(&version).map_err(|_| CompanionsError::Mismatch)?,
                sha256: Sha256::parse_hex(&sha256).map_err(|_| CompanionsError::Mismatch)?,
            };
            self.manager.install(&token).await.map_err(refused)?;
            self.off(move |s| s.row(e)).await
        })
    }

    fn removal(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let r = self
                .off(move |s| s.manager.removal(&e.id).map_err(refused))
                .await?;
            Ok(plan(e, &r.rows, false))
        })
    }

    fn remove(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let r = self.manager.remove(&e.id).await.map_err(refused)?;
            Ok(plan(e, &r.rows, true))
        })
    }

    fn restart(&self, id: String) -> lodestar_api::ops::BoxFuture<'_, Result<(), CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            self.ours(e).await?;
            self.live.restart(&e.id).await
        })
    }

    fn logs(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Vec<String>, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            self.off(move |s| {
                if !s.installed(&e.id)? {
                    return Err(CompanionsError::Refused("not_installed"));
                }
                let lines = s.live.logs(&e.id)?;
                // Redacted again with its own credentials, whatever the
                // ring did (plan CMP-6).
                let mut secrets = Vec::new();
                for f in s.stored(&e.id)? {
                    secrets.extend(s.open(&e.id, f)?);
                }
                let r = Redactor::new(&secrets);
                Ok(lines.iter().map(|l| r.redact(l)).collect())
            })
            .await
        })
    }

    fn template(
        &self,
        id: String,
        request: TemplateRequest,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<CompanionTemplate, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let kind = request.kind;
            self.off(move |s| {
                let input = s.input(&request)?;
                let generate = |input: &ServiceInput| -> Result<(Option<String>, Vec<UnraidFile>), TemplateError> {
                    Ok(match kind {
                        TemplateKind::Compose => {
                            (Some(compose::compose(s.catalogue, &e.id, input)?), Vec::new())
                        }
                        TemplateKind::Unraid => (
                            None,
                            unraid::unraid(s.catalogue, &e.id, input)?
                                .into_iter()
                                .map(|t| UnraidFile {
                                    add_container_path: t.add_container_path(),
                                    fields: t
                                        .fields
                                        .into_iter()
                                        .map(|(label, value)| UnraidField { label, value })
                                        .collect(),
                                    file_name: t.file_name,
                                    xml: t.xml,
                                })
                                .collect(),
                        ),
                    })
                };
                let (compose, unraid) = generate(&input).map_err(|err| {
                    let path = folder_rule(err).then(|| blame(&input, err, generate));
                    template_error(err, path)
                })?;
                Ok(CompanionTemplate {
                    compose,
                    id: e.id.as_str().to_owned(),
                    kind,
                    unraid,
                })
            })
            .await
        })
    }

    fn reveal(
        &self,
        id: String,
        name: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Revealed, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let f = field(&name)?;
            if field_of(&e.id) != Some(f) {
                return Err(CompanionsError::NoCredential);
            }
            // Q5 is about generated credentials: only a companion Lodestar
            // installed has one. A pasted key or temporary password (a
            // guided or detected companion) is never shown again.
            let secret = self
                .off(move |s| s.revealable(&e.id, f))
                .await?
                .ok_or(CompanionsError::NoCredential)?;
            Ok(Revealed {
                id: e.id.as_str(),
                field: f.as_str(),
                value: secret.expose().to_owned(),
            })
        })
    }

    fn regenerate(
        &self,
        id: String,
        name: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<(), CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let f = field(&name)?;
            if field_of(&e.id) != Some(f) {
                return Err(CompanionsError::NoCredential);
            }
            // #653's L4: a credential of a companion you added (even one
            // Lodestar generated, a guided qBittorrent's) is never rotated
            // here. Rotating it without setting it on the companion and
            // saving it in Integrations in the same step would leave the
            // stored password matching neither.
            self.ours(e).await?;
            if !self.off(move |s| s.stored(&e.id)).await?.contains(&f) {
                return Err(CompanionsError::NoCredential);
            }
            self.live.regenerate(&e.id, f).await
        })
    }
}

/// What a removal deletes: the folder, relative to the data folder, and
/// each credential row.
fn plan(e: &Entry, rows: &[String], removed: bool) -> RemovalPlan {
    let mut deletes = vec![RemovalItem {
        kind: RemovalItemKind::Folder,
        name: format!("companions/{}", e.id),
    }];
    deletes.extend(rows.iter().map(|r| RemovalItem {
        kind: RemovalItemKind::Setting,
        name: r.clone(),
    }));
    RemovalPlan {
        deletes,
        id: e.id.as_str().to_owned(),
        removed,
    }
}

/// A [`Service`] over `store` and the compiled-in catalogue, on Linux
/// x86-64, with nothing started: for the guided endpoints' tests.
#[cfg(test)]
pub(crate) fn test_service(
    dir: &std::path::Path,
    store: Arc<Mutex<Store>>,
    in_container: bool,
) -> Arc<Service> {
    let catalogue = lodestar_companions::builtin().unwrap();
    let host = Host {
        platform: Some((
            lodestar_companions::catalogue::Platform::Linux,
            lodestar_companions::catalogue::Arch::X86_64,
        )),
        in_container,
    };
    let unwired = Arc::new(Unwired);
    let (m, secrets) = manager_with(
        dir.to_path_buf(),
        store.clone(),
        catalogue,
        host,
        unwired.clone(),
        unwired,
    )
    .unwrap();
    Arc::new(Service {
        manager: Arc::new(m),
        catalogue,
        host,
        data: dir.to_path_buf(),
        secrets,
        rows: StoreRows(store),
        live: Arc::new(Unwired),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_companions::credentials::generate;

    #[test]
    fn containers_are_recognised() {
        let only = |path: &'static str, body: &'static [u8]| {
            move |p: &str| (p == path).then(|| body.to_vec())
        };
        assert!(in_container(only("/.dockerenv", b"")));
        assert!(in_container(only("/run/.containerenv", b"")));
        assert!(in_container(only(
            "/proc/1/cgroup",
            b"12:pids:/docker/abc\n"
        )));
        assert!(in_container(only(
            "/proc/1/environ",
            b"PATH=/bin\0container=lxc\0"
        )));
        assert!(!in_container(only("/proc/1/cgroup", b"0::/\n")));
        assert!(!in_container(only("/proc/1/environ", b"PATH=/bin\0")));
    }

    fn store(dir: &std::path::Path) -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(
            Store::open(dir.join("db"), Default::default()).unwrap(),
        ))
    }

    const NATIVE: Host = Host {
        platform: Some((
            lodestar_companions::catalogue::Platform::Linux,
            lodestar_companions::catalogue::Arch::X86_64,
        )),
        in_container: false,
    };

    fn integrations() -> watch::Sender<crate::settings::integrations::Integrations> {
        watch::channel(Default::default()).0
    }

    /// #619's review (B1): a held companion lock turns companions off for
    /// the run instead of stopping serve.
    #[tokio::test]
    async fn a_held_lock_turns_companions_off() {
        let dir = tempfile::tempdir().unwrap();
        let cat = lodestar_companions::builtin().unwrap();
        let held = manager_on(dir.path().to_path_buf(), store(dir.path()), cat, NATIVE).unwrap();
        let tx = integrations();
        let got = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            NATIVE,
        )
        .await;
        assert!(got.is_none());
        drop(held);
        let got = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            NATIVE,
        )
        .await;
        assert!(got.is_some());
    }

    /// Recovery runs at serve start: an empty folder (a crash before
    /// `begun`) is tidied, and a folder holding files Lodestar has no
    /// record of is left as it is.
    #[tokio::test]
    async fn recovery_runs_at_start_and_spares_foreign_files() {
        let dir = tempfile::tempdir().unwrap();
        let cat = lodestar_companions::builtin().unwrap();
        let companions = dir.path().join("companions");
        std::fs::create_dir_all(companions.join("qbittorrent")).unwrap();
        let mine = companions.join("prowlarr").join("config.xml");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, b"<Config/>").unwrap();
        let tx = integrations();
        let got = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            NATIVE,
        )
        .await;
        assert!(got.is_some());
        assert!(!companions.join("qbittorrent").exists());
        assert_eq!(std::fs::read(&mine).unwrap(), b"<Config/>");
    }

    /// The snapshot follows whether any download client is set up.
    #[tokio::test]
    async fn the_snapshot_follows_the_download_clients() {
        let dir = tempfile::tempdir().unwrap();
        let cat = lodestar_companions::builtin().unwrap();
        let tx = integrations();
        let c = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            NATIVE,
        )
        .await
        .unwrap();
        let mut rx = c.snapshot.clone();
        assert!(!rx.borrow_and_update().download_clients);
        let client = serde_json::from_value(serde_json::json!({
            "id": "home",
            "name": "Home",
            "kind": "qbittorrent",
            "url": "http://127.0.0.1:8080/"
        }))
        .unwrap();
        tx.send_modify(|i| i.clients.push(client));
        let deadline = std::time::Duration::from_secs(10);
        tokio::time::timeout(deadline, rx.wait_for(|s| s.download_clients))
            .await
            .expect("the snapshot didn't follow the new client")
            .unwrap();
        tx.send_modify(|i| i.clients.clear());
        tokio::time::timeout(deadline, rx.wait_for(|s| !s.download_clients))
            .await
            .expect("the snapshot didn't follow the removed client")
            .unwrap();
    }

    #[test]
    fn rows_round_trip_and_forget_only_their_companion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db"), Default::default()).unwrap();
        let rows = StoreRows(Arc::new(Mutex::new(store)));
        let secrets = SecretStore::from_key(&[3u8; 32]);
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        let prowlarr = CompanionId::parse("prowlarr").unwrap();
        let a = generate(
            &secrets,
            &CredentialKey::new(qbt.clone(), Field::WebUiPassword),
        )
        .unwrap();
        let b = generate(
            &secrets,
            &CredentialKey::new(prowlarr.clone(), Field::ApiKey),
        )
        .unwrap();
        rows.put(&a.row).unwrap();
        rows.put(&b.row).unwrap();
        assert_eq!(rows.get(&a.row.key).unwrap(), Some(a.row.value.clone()));
        rows.forget(&qbt).unwrap();
        assert_eq!(rows.get(&a.row.key).unwrap(), None);
        assert_eq!(rows.get(&b.row.key).unwrap(), Some(b.row.value));
    }

    /// The compiled-in catalogue. Nothing in it is pinned yet, and the
    /// binary can't load another (the `testing` feature stays out of it),
    /// so the manager's refusal before any download is tested where the
    /// manager is, with a pinned catalogue.
    fn pinned() -> &'static Catalogue {
        lodestar_companions::builtin().unwrap()
    }

    /// The supervisor CMP-5g hands in, as far as these tests need: its
    /// ring holds a line with the companion's own password in it.
    struct Ring(String);

    impl Live for Ring {
        fn restart<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), CompanionsError>> {
            Box::pin(async { Ok(()) })
        }
        fn logs(&self, _: &CompanionId) -> Result<Vec<String>, CompanionsError> {
            Ok(vec![format!(
                "WebUI: signed in with {} from 127.0.0.1",
                self.0
            )])
        }
        fn regenerate<'a>(
            &'a self,
            _: &'a CompanionId,
            _: Field,
        ) -> BoxFuture<'a, Result<(), CompanionsError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn service(
        dir: &std::path::Path,
        catalogue: &'static Catalogue,
        host: Host,
        live: Arc<dyn Live>,
    ) -> Service {
        let store = store(dir);
        let unwired = Arc::new(Unwired);
        let (m, secrets) = manager_with(
            dir.to_path_buf(),
            store.clone(),
            catalogue,
            host,
            unwired.clone(),
            unwired,
        )
        .unwrap();
        Service {
            manager: Arc::new(m),
            catalogue,
            host,
            data: dir.to_path_buf(),
            secrets,
            rows: StoreRows(store),
            live,
        }
    }

    /// Stores a generated credential for `id`, as an install would.
    fn stored(s: &Service, id: &str, f: Field) -> String {
        let key = CredentialKey::new(CompanionId::parse(id).unwrap(), f);
        let issued = generate(&s.secrets, &key).unwrap();
        s.rows.put(&issued.row).unwrap();
        issued.secret.expose().to_owned()
    }

    /// An install reaches the manager only with a well-formed token, and
    /// comes back with the manager's refusal; nothing is written.
    #[tokio::test]
    async fn an_install_passes_the_managers_refusal_through() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        assert_eq!(
            s.offer("qbittorrent".into()).await,
            Err(CompanionsError::Refused("awaiting_pin"))
        );
        let got = s
            .install("qbittorrent".into(), "5.1.2".into(), "ab".repeat(32))
            .await;
        assert_eq!(got, Err(CompanionsError::Refused("awaiting_pin")));
        assert!(!dir.path().join("companions/qbittorrent").exists());
        for (v, sha) in [
            ("5.1.2", "nope".to_owned()),
            ("not a version!", "ab".repeat(32)),
        ] {
            assert_eq!(
                s.install("qbittorrent".into(), v.into(), sha).await,
                Err(CompanionsError::Mismatch)
            );
        }
        assert_eq!(
            s.install("nope".into(), "1".into(), "ab".repeat(32)).await,
            Err(CompanionsError::Unknown)
        );
    }

    #[test]
    fn manager_refusals_map_to_api_codes() {
        assert_eq!(
            refused(Refusal::Confirmation.into()),
            CompanionsError::Mismatch
        );
        assert_eq!(refused(Refusal::Unknown.into()), CompanionsError::Unknown);
        assert_eq!(
            refused(Refusal::Busy.into()),
            CompanionsError::Refused("busy")
        );
        assert_eq!(
            refused(ManagerError::Step {
                step: "start",
                code: "not_wired"
            }),
            CompanionsError::Refused("not_wired")
        );
        assert_eq!(
            refused(ManagerError::Step {
                step: "fetch",
                code: "io"
            }),
            CompanionsError::Internal("io")
        );
    }

    #[tokio::test]
    async fn the_list_says_what_can_be_installed_here_and_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        stored(&s, "prowlarr", Field::ApiKey);
        let all = s.list().await.unwrap();
        let qbt = &all[0];
        assert_eq!(
            (
                qbt.id.as_str(),
                qbt.available.as_deref(),
                qbt.refusal.as_deref()
            ),
            ("qbittorrent", None, Some("awaiting_pin"))
        );
        assert_eq!((qbt.installed.as_ref(), qbt.health), (None, None));
        assert_eq!(qbt.licence, "GPL-2.0-or-later");
        assert_eq!(
            qbt.templates,
            vec![TemplateKind::Compose, TemplateKind::Unraid]
        );
        assert!(qbt.credentials.is_empty());
        let p = &all[1];
        assert_eq!(
            (p.available.as_deref(), p.refusal.as_deref()),
            (None, Some("awaiting_pin"))
        );
        assert_eq!(p.credentials, vec!["api_key".to_owned()]);
        assert_eq!(s.get("prowlarr".into()).await.unwrap(), *p);
        assert_eq!(s.get("Nope".into()).await, Err(CompanionsError::Unknown));

        let dir = tempfile::tempdir().unwrap();
        let docker = Host {
            in_container: true,
            ..NATIVE
        };
        let s = service(dir.path(), pinned(), docker, Arc::new(Unwired));
        let qbt = s.get("qbittorrent".into()).await.unwrap();
        assert_eq!(qbt.refusal.as_deref(), Some("native_in_container"));
        assert_eq!(
            s.offer("qbittorrent".into()).await,
            Err(CompanionsError::Refused("native_in_container"))
        );
    }

    /// A journal saying Lodestar installed `id` at 5.1.2, read back by
    /// recovery as at serve start.
    async fn installed(s: &Service, dir: &std::path::Path, id: &str) {
        let home = dir.join("companions").join(id);
        std::fs::create_dir_all(&home).unwrap();
        lodestar_companions::state::save(
            &home,
            &lodestar_companions::state::Record {
                installed: Some(Version::parse("5.1.2").unwrap()),
                op: None,
            },
        )
        .unwrap();
        assert!(s.installed(&CompanionId::parse(id).unwrap()).unwrap());
    }

    /// The installed check reads the journal, not the snapshot: a journal
    /// recovery hasn't published yet counts, and a snapshot whose journal
    /// is gone doesn't (CMP-6's decisions: detected companions join the
    /// snapshot with a version).
    #[tokio::test]
    async fn installed_means_the_journal_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        assert!(!s.installed(&qbt).unwrap());
        installed(&s, dir.path(), "qbittorrent").await;
        let password = stored(&s, "qbittorrent", Field::WebUiPassword);
        assert!(s.manager.snapshot().borrow().companions.is_empty());
        let r = s
            .reveal("qbittorrent".into(), "webui_password".into())
            .await
            .unwrap();
        assert_eq!(r.value, password);
        // Published, then the journal goes: the snapshot still lists it.
        s.manager.recover().await;
        std::fs::remove_file(dir.path().join("companions/qbittorrent/state.json")).unwrap();
        assert!(
            s.manager
                .snapshot()
                .borrow()
                .companions
                .iter()
                .any(|c| c.id == qbt && c.version.is_some())
        );
        assert_eq!(
            s.reveal("qbittorrent".into(), "webui_password".into())
                .await
                .err(),
            Some(CompanionsError::NoCredential)
        );
        // Inside a container nothing is native, whatever the journal says.
        installed(&s, dir.path(), "qbittorrent").await;
        let docker = Service {
            host: Host {
                in_container: true,
                ..NATIVE
            },
            ..s.clone_parts()
        };
        assert!(!docker.installed(&qbt).unwrap());
    }

    /// Carry-over 2: a companion you added reveals its credential only
    /// when its provenance says Lodestar generated it.
    #[tokio::test]
    async fn an_added_companions_credential_reveals_only_when_generated() {
        use lodestar_companions::guided::{Provenance, provenance_key};
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        let password = stored(&s, "qbittorrent", Field::WebUiPassword);
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        let key = provenance_key(&qbt, Field::WebUiPassword).unwrap();
        let mark = |p: Provenance| {
            s.rows
                .0
                .lock()
                .unwrap()
                .set_settings_text(&[(&key, p.as_str())])
                .unwrap();
        };
        let reveal = || s.reveal("qbittorrent".into(), "webui_password".into());
        assert_eq!(reveal().await.err(), Some(CompanionsError::NoCredential));
        mark(Provenance::Pasted);
        assert_eq!(reveal().await.err(), Some(CompanionsError::NoCredential));
        mark(Provenance::Generated);
        assert_eq!(reveal().await.unwrap().value, password);
    }

    /// The template guard's positive path (CMP-6's delta review): on a
    /// native install the resolved data folder goes into the generator's
    /// input, so a guard that resolved it and dropped it would fail here.
    #[cfg(unix)]
    #[test]
    fn the_data_folder_reaches_the_template_input() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        let req: TemplateRequest =
            serde_json::from_value(serde_json::json!({"kind": "compose"})).unwrap();
        let input = s.input(&req).unwrap();
        let want = format!(
            "{:?}",
            DataFolders::none().with(DataFolder::from_running(dir.path()).unwrap())
        );
        assert_eq!(format!("{:?}", input.data), want);
        assert_ne!(want, format!("{:?}", DataFolders::none()));
        let docker = Service {
            host: Host {
                in_container: true,
                ..NATIVE
            },
            ..s.clone_parts()
        };
        assert_eq!(
            format!("{:?}", docker.input(&req).unwrap().data),
            format!("{:?}", DataFolders::none())
        );
    }

    /// Q5: a credential of a companion Lodestar didn't install is a pasted
    /// one, and is never shown again.
    #[tokio::test]
    async fn a_pasted_credential_is_never_revealed() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        stored(&s, "qbittorrent", Field::WebUiPassword);
        stored(&s, "prowlarr", Field::ApiKey);
        for (id, f) in [("qbittorrent", "webui_password"), ("prowlarr", "api_key")] {
            assert_eq!(
                s.reveal(id.into(), f.into()).await.err(),
                Some(CompanionsError::NoCredential),
                "{id}"
            );
            assert_eq!(
                s.regenerate(id.into(), f.into()).await,
                Err(CompanionsError::Refused("not_installed"))
            );
        }
    }

    /// Q5: the stored value of a companion Lodestar installed, opened from
    /// its sealed row, and nothing for a field or companion with no row.
    #[tokio::test]
    async fn reveal_opens_the_stored_credential_only() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        installed(&s, dir.path(), "qbittorrent").await;
        let password = stored(&s, "qbittorrent", Field::WebUiPassword);
        let r = s
            .reveal("qbittorrent".into(), "webui_password".into())
            .await
            .unwrap();
        assert_eq!((r.id, r.field), ("qbittorrent", "webui_password"));
        assert_eq!(r.value, password);
        assert!(!format!("{r:?}").contains(&password));
        for (id, field, want) in [
            ("qbittorrent", "api_key", CompanionsError::NoCredential),
            ("prowlarr", "api_key", CompanionsError::NoCredential),
            ("qbittorrent", "password", CompanionsError::NoCredential),
            ("nope", "api_key", CompanionsError::Unknown),
        ] {
            assert_eq!(
                s.reveal(id.into(), field.into()).await.err(),
                Some(want),
                "{id} {field}"
            );
        }
        // Regenerate needs the supervisor: refused, and the row is kept.
        assert_eq!(
            s.regenerate("qbittorrent".into(), "webui_password".into())
                .await,
            Err(CompanionsError::Refused("not_wired"))
        );
        assert_eq!(
            s.regenerate("qbittorrent".into(), "api_key".into()).await,
            Err(CompanionsError::NoCredential)
        );
        let again = s
            .reveal("qbittorrent".into(), "webui_password".into())
            .await
            .unwrap();
        assert_eq!(again.value, password);
    }

    /// Restart and logs are for a companion Lodestar installed, and need
    /// the supervisor.
    #[tokio::test]
    async fn restart_and_logs_need_the_supervisor() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        assert_eq!(
            s.restart("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
        assert_eq!(
            s.logs("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
        assert_eq!(
            s.removal("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
        assert_eq!(
            s.remove("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
        installed(&s, dir.path(), "qbittorrent").await;
        assert_eq!(
            s.restart("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_wired"))
        );
        assert_eq!(
            s.logs("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_wired"))
        );
    }

    /// The supervisor's lines are redacted again with the companion's own
    /// credentials on the way out.
    #[tokio::test]
    async fn logs_are_redacted_again() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        installed(&s, dir.path(), "qbittorrent").await;
        let password = stored(&s, "qbittorrent", Field::WebUiPassword);
        let s = Service {
            live: Arc::new(Ring(password.clone())),
            ..s.clone_parts()
        };
        let lines = s.logs("qbittorrent".into()).await.unwrap();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains(&password), "{}", lines[0]);
        assert!(lines[0].contains("127.0.0.1"), "{}", lines[0]);
    }

    #[test]
    fn a_removal_names_the_folder_relative_to_the_data_folder() {
        let e = pinned().get("qbittorrent").unwrap();
        let p = plan(
            e,
            &["companions.qbittorrent.secret.webui_password".into()],
            false,
        );
        assert_eq!(
            p.deletes,
            vec![
                RemovalItem {
                    kind: RemovalItemKind::Folder,
                    name: "companions/qbittorrent".into()
                },
                RemovalItem {
                    kind: RemovalItemKind::Setting,
                    name: "companions.qbittorrent.secret.webui_password".into()
                },
            ]
        );
        assert!(!p.removed);
    }

    #[tokio::test]
    async fn templates_check_their_inputs_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        let req = |v: serde_json::Value| -> TemplateRequest { serde_json::from_value(v).unwrap() };
        for (body, field) in [
            (serde_json::json!({"kind": "compose", "puid": "x"}), "puid"),
            (
                serde_json::json!({"kind": "compose", "config_dir": "relative"}),
                "config_dir",
            ),
            (
                serde_json::json!({"kind": "compose", "network": "bad net"}),
                "network",
            ),
            (
                serde_json::json!({"kind": "compose", "route": "existing_gluetun"}),
                "gluetun_container",
            ),
            (
                serde_json::json!({"kind": "compose", "gluetun_container": "vpn"}),
                "gluetun_container",
            ),
        ] {
            match s.template("qbittorrent".into(), req(body.clone())).await {
                Err(CompanionsError::BadInput { field: f, .. }) => {
                    assert_eq!(f, Some(field), "{body}")
                }
                other => panic!("{body}: {other:?}"),
            }
        }
        let ok =
            serde_json::json!({"kind": "compose", "config_dir": "/srv/q", "downloads": "/srv/d"});
        // A data folder that can't be resolved refuses rather than
        // dropping the guard against mounting it.
        #[cfg(unix)]
        {
            let gone = Service {
                data: dir.path().join("missing"),
                ..s.clone_parts()
            };
            let got = gone.template("qbittorrent".into(), req(ok.clone())).await;
            assert_eq!(got.err(), Some(CompanionsError::Internal("data_folder")));
        }
        // Valid inputs reach CMP-7's generator, which refuses an image
        // that isn't pinned yet.
        for kind in ["compose", "unraid"] {
            let mut body = ok.clone();
            body["kind"] = kind.into();
            assert_eq!(
                s.template("qbittorrent".into(), req(body)).await.err(),
                Some(CompanionsError::Refused("awaiting_pin")),
                "{kind}"
            );
        }
        assert_eq!(
            s.template("nope".into(), req(ok)).await.err(),
            Some(CompanionsError::Unknown)
        );
    }

    /// The folder a folder rule refused is named, not always the settings
    /// one. The generator here checks folders as CMP-7 does: settings
    /// first, then downloads, refusing a dot component.
    #[test]
    fn a_folder_refusal_names_the_folder_at_fault() {
        let generate = |i: &ServiceInput| -> Result<(), TemplateError> {
            for p in [&i.paths.config, &i.paths.downloads].into_iter().flatten() {
                if p.as_str().contains("/.") {
                    return Err(TemplateError::DotFolder);
                }
            }
            Ok(())
        };
        let s_req = |config: &str, downloads: Option<&str>| ServiceInput {
            puid: Puid::parse("1000").unwrap(),
            pgid: Pgid::parse("1000").unwrap(),
            network: NetworkName::parse("lodestar").unwrap(),
            route: compose::Route::Direct,
            paths: Paths {
                config: Some(HostPath::parse(config).unwrap()),
                downloads: downloads.map(|d| HostPath::parse(d).unwrap()),
            },
            publish: Vec::new(),
            data: DataFolders::none(),
            taken: Vec::new(),
        };
        let bad_downloads = s_req("/srv/qbittorrent", Some("/srv/.hidden"));
        let e = generate(&bad_downloads).unwrap_err();
        assert_eq!(blame(&bad_downloads, e, generate), "downloads");
        let bad_config = s_req("/srv/.hidden", Some("/srv/downloads"));
        let e = generate(&bad_config).unwrap_err();
        assert_eq!(blame(&bad_config, e, generate), "config_dir");
        let only_config = s_req("/srv/.hidden", None);
        assert_eq!(
            blame(&only_config, TemplateError::DotFolder, generate),
            "config_dir"
        );
        // A rule no one field breaks names none.
        assert_eq!(
            template_error(TemplateError::Interpolation, None),
            CompanionsError::BadInput {
                field: None,
                code: "interpolation"
            }
        );
    }

    /// Each companion's reveal and Regenerate reach only its own field.
    #[tokio::test]
    async fn only_the_companions_own_credential_is_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        installed(&s, dir.path(), "qbittorrent").await;
        stored(&s, "qbittorrent", Field::WebUiPassword);
        // A row under the other field, which qBittorrent never has.
        stored(&s, "qbittorrent", Field::ApiKey);
        assert_eq!(
            s.reveal("qbittorrent".into(), "api_key".into()).await.err(),
            Some(CompanionsError::NoCredential)
        );
        assert_eq!(
            s.regenerate("qbittorrent".into(), "api_key".into()).await,
            Err(CompanionsError::NoCredential)
        );
        assert!(
            s.reveal("qbittorrent".into(), "webui_password".into())
                .await
                .is_ok()
        );
    }

    /// Counts the supervisor's calls.
    #[derive(Default)]
    struct Calls(std::sync::atomic::AtomicUsize);

    impl Calls {
        fn n(&self) -> usize {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn hit(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Live for Calls {
        fn restart<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), CompanionsError>> {
            self.hit();
            Box::pin(async { Ok(()) })
        }
        fn logs(&self, _: &CompanionId) -> Result<Vec<String>, CompanionsError> {
            self.hit();
            Ok(Vec::new())
        }
        fn regenerate<'a>(
            &'a self,
            _: &'a CompanionId,
            _: Field,
        ) -> BoxFuture<'a, Result<(), CompanionsError>> {
            self.hit();
            Box::pin(async { Ok(()) })
        }
    }

    /// #653's L4: a guided qBittorrent's generated password, the one it
    /// uses now, is never rotated (nor restarted or read from a ring) by
    /// the supervisor: it's refused before the supervisor is asked, and the
    /// stored password stays the one you can reveal. A companion Lodestar
    /// installed reaches it.
    #[tokio::test]
    async fn a_guided_companions_credential_is_never_rotated() {
        use lodestar_companions::guided::{Provenance, provenance_key};
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(Calls::default());
        let s = service(dir.path(), pinned(), NATIVE, calls.clone());
        let password = stored(&s, "qbittorrent", Field::WebUiPassword);
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        let key = provenance_key(&qbt, Field::WebUiPassword).unwrap();
        s.rows
            .0
            .lock()
            .unwrap()
            .set_settings_text(&[(&key, Provenance::Generated.as_str())])
            .unwrap();
        let row = CredentialKey::new(qbt.clone(), Field::WebUiPassword).row_key();
        let before = s.rows.get(&row).unwrap();
        let no = CompanionsError::Refused("not_installed");
        let not = Err(no.clone());
        assert_eq!(
            s.regenerate("qbittorrent".into(), "webui_password".into())
                .await,
            not
        );
        assert_eq!(s.restart("qbittorrent".into()).await, not);
        assert_eq!(s.logs("qbittorrent".into()).await, Err(no));
        assert_eq!(calls.n(), 0);
        assert_eq!(s.rows.get(&row).unwrap(), before);
        let r = s
            .reveal("qbittorrent".into(), "webui_password".into())
            .await
            .unwrap();
        assert_eq!(r.value, password);
        // Installed by Lodestar: each reaches the supervisor.
        installed(&s, dir.path(), "qbittorrent").await;
        s.regenerate("qbittorrent".into(), "webui_password".into())
            .await
            .unwrap();
        s.restart("qbittorrent".into()).await.unwrap();
        s.logs("qbittorrent".into()).await.unwrap();
        assert_eq!(calls.n(), 3);
    }

    fn egress_row(st: &Mutex<Store>, key: &str) -> Option<String> {
        st.lock().unwrap().setting_text(key).unwrap()
    }

    fn set_row(st: &Mutex<Store>, key: &str, value: &str) {
        st.lock()
            .unwrap()
            .set_settings_text(&[(key, value)])
            .unwrap();
    }

    /// Q18: a companion's route is set from the plugin it serves the first
    /// time it's needed, then kept as its own: the plugin's later changes
    /// don't move it, and its own row wins.
    #[test]
    fn a_companions_route_is_set_once_from_its_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        let prowlarr = CompanionId::parse("prowlarr").unwrap();
        // The download-client source has none saved: direct.
        assert_eq!(egress_setting(&st, &qbt), Some(Ok(EgressSetting::Direct)));
        assert_eq!(
            egress_row(&st, "companions.qbittorrent.egress").as_deref(),
            Some("direct")
        );
        // Prowlarr's plugin has a proxy: Prowlarr gets it.
        set_row(&st, "sources.egress.prowlarr", "socks5 10.8.0.1 1080 auto");
        set_row(
            &st,
            "sources.egress.download_clients",
            "socks5 10.8.0.1 1080 auto",
        );
        let want = EgressSetting::decode("socks5 10.8.0.1 1080 auto").unwrap();
        assert_eq!(egress_setting(&st, &prowlarr), Some(Ok(want.clone())));
        assert_eq!(
            egress_row(&st, "companions.prowlarr.egress").as_deref(),
            Some("socks5 10.8.0.1 1080 auto")
        );
        // qBittorrent's was saved already: the plugin's change doesn't
        // reach it.
        assert_eq!(egress_setting(&st, &qbt), Some(Ok(EgressSetting::Direct)));
        // Its own row, set on its screen, wins.
        set_row(
            &st,
            "companions.qbittorrent.egress",
            "socks5 10.8.0.2 1080 local",
        );
        assert!(matches!(
            egress_setting(&st, &qbt),
            Some(Ok(EgressSetting::Socks5(_)))
        ));
    }

    /// An unreadable route is never read as direct, and an unreadable
    /// plugin route isn't saved as the companion's. A companion that
    /// serves no plugin has none.
    #[test]
    fn an_unreadable_route_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        set_row(&st, "companions.qbittorrent.egress", "interface tun0");
        assert_eq!(egress_setting(&st, &qbt), Some(Err(Unreadable)));
        let prowlarr = CompanionId::parse("prowlarr").unwrap();
        set_row(&st, "sources.egress.prowlarr", "socks5 nope");
        assert_eq!(egress_setting(&st, &prowlarr), Some(Err(Unreadable)));
        assert_eq!(egress_row(&st, "companions.prowlarr.egress"), None);
        let gluetun = CompanionId::parse("gluetun").unwrap();
        assert_eq!(egress_setting(&st, &gluetun), None);
        assert_eq!(egress_row(&st, "companions.gluetun.egress"), None);
    }

    /// Each start's route follows the saved setting: its kind, its live
    /// state, and the form CMP-8b configures the companion for.
    #[tokio::test]
    async fn routes_are_built_from_the_saved_setting() {
        use lodestar_sources::egress::RouteState;
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let routes = CompanionRoutes::new(st.clone());
        let prowlarr = CompanionId::parse("prowlarr").unwrap();
        let direct = routes.route(&prowlarr).await.unwrap();
        assert_eq!(direct.kind, RouteKind::Direct);
        assert_eq!(routes.configured(&prowlarr), Some(configure::Route::Direct));
        set_row(&st, "companions.prowlarr.egress", "socks5 127.0.0.1 9 auto");
        let socks = routes.route(&prowlarr).await.unwrap();
        assert_eq!(socks.kind, RouteKind::Socks5);
        let proxy = Socks5Proxy::new("127.0.0.1:9".parse().unwrap()).unwrap();
        assert_eq!(
            routes.configured(&prowlarr),
            Some(configure::Route::Socks5(proxy))
        );
        set_row(&st, "companions.prowlarr.egress", "socks5");
        let mut down = routes.route(&prowlarr).await.unwrap();
        assert_eq!(down.kind, RouteKind::Socks5);
        let deadline = std::time::Duration::from_secs(10);
        let state = tokio::time::timeout(
            deadline,
            down.state
                .wait_for(|s| *s == RouteState::Down(DownReason::SettingUnreadable)),
        )
        .await
        .expect("the unreadable route never went down");
        assert!(state.is_ok());
        assert_eq!(routes.configured(&prowlarr), None);
        let gluetun = CompanionId::parse("gluetun").unwrap();
        assert!(routes.route(&gluetun).await.is_none());
    }

    /// At serve, a native run hands the manager a supervisor, and shutdown
    /// stops it: nothing can start after.
    #[tokio::test]
    async fn serve_wires_the_supervisor_and_shuts_it_down() {
        let dir = tempfile::tempdir().unwrap();
        let cat = lodestar_companions::builtin().unwrap();
        let tx = integrations();
        let c = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            NATIVE,
        )
        .await
        .unwrap();
        assert!(c.wiring.supervised.get().is_some());
        assert!(c.wiring.ready().is_ok());
        let wiring = c.wiring.clone();
        c.shutdown().await;
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        // A program in an installed version's folder, so the start gets as
        // far as the supervisor's lock.
        let install = dir.path().join("companions/qbittorrent/5.1.2");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join("qbittorrent");
        std::fs::write(&executable, b"never run").unwrap();
        let launch = Launch {
            id: qbt.clone(),
            executable,
            install,
            args: Vec::new(),
            target: Target::Loopback(lodestar_companions::inputs::PortNumber::new(8080).unwrap()),
            secrets: Vec::new(),
        };
        assert_eq!(wiring.start(launch).await, Err("shutting_down"));
        // Inside a container nothing is native: unwired.
        let dir = tempfile::tempdir().unwrap();
        let docker = Host {
            in_container: true,
            ..NATIVE
        };
        let c = start_with(
            dir.path().to_path_buf(),
            store(dir.path()),
            tx.subscribe(),
            cat,
            docker,
        )
        .await
        .unwrap();
        assert_eq!(c.wiring.ready(), Err("not_wired"));
    }
}
