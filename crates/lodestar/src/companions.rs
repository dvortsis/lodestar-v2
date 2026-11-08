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
//! Starting installed companions at serve, behind their own route (Q18's
//! `companions.<id>.egress`), is CMP-5g's wiring; until then [`Unwired`]
//! refuses every start, so nothing runs on a route nobody chose.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
use lodestar_companions::configure::InstallReceipt;
use lodestar_companions::credentials::{self, CredentialKey, Field, SealedRow};
use lodestar_companions::fetch::{self, Guarded};
use lodestar_companions::inputs::{ContainerName, HostPath, NetworkName, Pgid, Puid};
use lodestar_companions::log_events::INSTALL_FAILED;
use lodestar_companions::manager::{
    ApiConfigure, Downloader, Health, Host, InstallToken, Launch, Manager, ManagerError, Parts,
    Refusal, RowError, Rows, Run, Snapshot,
};
use lodestar_companions::supervise::logs::Redactor;
use lodestar_companions::{Catalogue, CompanionId, Entry, Version, unraid};
use lodestar_integrations::net::Route;
use lodestar_integrations::secret::{SealedSecret, Secret, SecretStore};
use lodestar_store::Store;
use tokio::sync::watch;

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

/// Refuses every start until CMP-5g hands in the supervisor and each
/// companion's route. Stopping is a no-op: nothing was started.
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

/// The manager over this data folder and store. Downloads go direct, as
/// the updater's do (CMP-1's decision); nothing is started ([`Unwired`]).
/// Blocking (the store, the secrets file, the lock): async callers use
/// `spawn_blocking`.
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
    manager_with(data, store, catalogue, host).map(|(m, _)| m)
}

/// [`manager_on`], with the secrets file the manager seals with, for the
/// API's reveal.
fn manager_with(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    catalogue: &'static Catalogue,
    host: Host,
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
        run: Arc::new(Unwired),
        api: Arc::new(Unwired),
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

/// What serve keeps: the manager, and the snapshot channel.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "CMP-5g starts companions and hands the snapshot on"
)]
pub struct Companions {
    pub manager: Arc<Manager>,
    /// `/companions` (CMP-6), layered over the API.
    pub api: Arc<Service>,
    /// CMP-10's input. TODO(CMP-5g): hand it to the pipeline as
    /// `PipelineConfig::companions` once CMP-10 has merged.
    pub snapshot: watch::Receiver<Snapshot>,
}

/// At serve: recovers what a crash interrupted, publishes the installed
/// companions, and follows whether any download client is set up. A bad
/// catalogue turns companions off for the run (CMP-0); a failed recovery
/// is logged by code and leaves that companion as it is.
///
/// TODO(CMP-5g): the task following `integrations.clients` isn't stopped
/// at shutdown; its stop signal belongs in `Handles::shutdown` with the
/// rest of CMP-5g's wiring.
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
    let opened = tokio::task::spawn_blocking(move || manager_with(data, store, catalogue, host))
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
    for (id, r) in manager.recover().await {
        if let Err(e) = r {
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
    let m = manager.clone();
    m.set_download_clients(!integrations.borrow_and_update().clients.is_empty());
    tokio::spawn(async move {
        while integrations.changed().await.is_ok() {
            let any = !integrations.borrow_and_update().clients.is_empty();
            m.set_download_clients(any);
        }
    });
    let snapshot = manager.snapshot();
    let api = Arc::new(Service {
        manager: manager.clone(),
        catalogue,
        host,
        data: folder,
        secrets,
        rows,
        live: Arc::new(Unwired),
    });
    Some(Companions {
        manager,
        api,
        snapshot,
    })
}

/// What only a running supervisor can do (CMP-5g hands one in): restart,
/// recent output and regenerating a credential, which rewrites the
/// companion's own config and restarts it. It answers `not_installed` for
/// a companion it doesn't run. [`Unwired`] refuses each with `not_wired`.
pub trait Live: Send + Sync {
    fn restart<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, Result<(), &'static str>>;
    /// Its recent output, as the supervisor's ring holds it (redacted
    /// once there; [`Service`] redacts it again).
    fn logs(&self, id: &CompanionId) -> Result<Vec<String>, &'static str>;
    fn regenerate<'a>(
        &'a self,
        id: &'a CompanionId,
        field: Field,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
}

impl Live for Unwired {
    fn restart<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async { Err("not_wired") })
    }
    fn logs(&self, _: &CompanionId) -> Result<Vec<String>, &'static str> {
        Err("not_wired")
    }
    fn regenerate<'a>(
        &'a self,
        _: &'a CompanionId,
        _: Field,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async { Err("not_wired") })
    }
}

/// `/companions` over the manager (CMP-6). CMP-5g swaps `live` (and the
/// manager's `Run`) for the supervisor; nothing here changes.
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
        field,
        code: "bad_template_input",
    }
}

fn template_error(e: TemplateError) -> CompanionsError {
    match e {
        TemplateError::UnknownCompanion => CompanionsError::Unknown,
        TemplateError::NoTemplate | TemplateError::NotAvailable(_) => {
            CompanionsError::Refused(e.code())
        }
        TemplateError::MissingPath(r) | TemplateError::UnexpectedPath(r) => {
            CompanionsError::BadInput {
                field: match r {
                    MountRole::Config => "config_dir",
                    MountRole::Downloads => "downloads",
                },
                code: e.code(),
            }
        }
        TemplateError::DataFolder | TemplateError::OtherCompanion | TemplateError::DotFolder => {
            CompanionsError::BadInput {
                field: "config_dir",
                code: e.code(),
            }
        }
        TemplateError::Encode => CompanionsError::Internal(e.code()),
        _ => CompanionsError::BadInput {
            field: "kind",
            code: e.code(),
        },
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

    /// Whether Lodestar installed it (the manager's snapshot, which
    /// recovery fills from each journal at start).
    fn installed(&self, id: &CompanionId) -> bool {
        self.manager
            .snapshot()
            .borrow()
            .companions
            .iter()
            .any(|c| &c.id == id && c.version.is_some())
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
                field: "gluetun_container",
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
            self.live
                .restart(&e.id)
                .await
                .map_err(CompanionsError::Refused)
        })
    }

    fn logs(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Vec<String>, CompanionsError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            self.off(move |s| {
                let lines = s.live.logs(&e.id).map_err(CompanionsError::Refused)?;
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
                let (compose, unraid) = match kind {
                    TemplateKind::Compose => (
                        Some(compose::compose(s.catalogue, &e.id, &input).map_err(template_error)?),
                        Vec::new(),
                    ),
                    TemplateKind::Unraid => (
                        None,
                        unraid::unraid(s.catalogue, &e.id, &input)
                            .map_err(template_error)?
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
                };
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
            // Q5 is about generated credentials: only a companion Lodestar
            // installed has one. A pasted key or temporary password (a
            // guided or detected companion) is never shown again.
            if !self.installed(&e.id) {
                return Err(CompanionsError::NoCredential);
            }
            let secret = self
                .off(move |s| s.open(&e.id, f))
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
            if !self.installed(&e.id) || !self.off(move |s| s.stored(&e.id)).await?.contains(&f) {
                return Err(CompanionsError::NoCredential);
            }
            self.live
                .regenerate(&e.id, f)
                .await
                .map_err(CompanionsError::Refused)
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
        fn restart<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), &'static str>> {
            Box::pin(async { Ok(()) })
        }
        fn logs(&self, _: &CompanionId) -> Result<Vec<String>, &'static str> {
            Ok(vec![format!(
                "WebUI: signed in with {} from 127.0.0.1",
                self.0
            )])
        }
        fn regenerate<'a>(
            &'a self,
            _: &'a CompanionId,
            _: Field,
        ) -> BoxFuture<'a, Result<(), &'static str>> {
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
        let (m, secrets) = manager_with(dir.to_path_buf(), store.clone(), catalogue, host).unwrap();
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
        s.manager.recover().await;
        assert!(s.installed(&CompanionId::parse(id).unwrap()));
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
                Err(CompanionsError::NoCredential)
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

    #[tokio::test]
    async fn restart_and_logs_need_the_supervisor() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
        assert_eq!(
            s.restart("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_wired"))
        );
        assert_eq!(
            s.logs("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_wired"))
        );
        assert_eq!(
            s.removal("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
        assert_eq!(
            s.remove("qbittorrent".into()).await,
            Err(CompanionsError::Refused("not_installed"))
        );
    }

    /// The supervisor's lines are redacted again with the companion's own
    /// credentials on the way out.
    #[tokio::test]
    async fn logs_are_redacted_again() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path(), pinned(), NATIVE, Arc::new(Unwired));
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
                Err(CompanionsError::BadInput { field: f, .. }) => assert_eq!(f, field, "{body}"),
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
}
