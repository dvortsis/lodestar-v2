//! The companion wiring in the binary (CMP-5g-1; plan
//! 2026-11-06-v06-companions CMP-5g, CMP-10's contract in
//! `docs/decisions/2026-11-08-CMP-5.md`):
//!
//! - [`present`] and [`start`]: what the pipeline's companion gate
//!   (`lodestar_pipeline::requires`) reads. Every companion Lodestar
//!   installed, with its health; every one you added and confirmed
//!   (the guided records, sent through [`Wired::added`]), watched every 30 s
//!   (`lodestar_companions::watch`); whether a download client you set up
//!   yourself exists; and whether the Prowlarr source runs against an
//!   indexer you set up yourself.
//! - [`Sink`]: CMP-8b's `ConfigSink`, writing a configured companion into
//!   Lodestar's own settings. qBittorrent becomes the `integrations.clients`
//!   entry [`MANAGED_CLIENT`]; Prowlarr becomes the Prowlarr source's
//!   address and sealed `api_key`.
//!
//! **Nothing a companion sent reaches these settings:** the sink writes
//! the target Lodestar chose and the credential it holds. Source settings
//! apply at Lodestar's next start, as any saved source setting does (the
//! source shows `restart_required`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_api::dto::ops::SourcePatch;
use lodestar_api::dto::settings::{SettingsBatch, SettingsPatch};
use lodestar_api::ops::OpsApi;
use lodestar_api::settings::SettingsApi;
use lodestar_companions::CompanionId;
use lodestar_companions::adapter::{BoxFuture, GuardedHttp, Target};
use lodestar_companions::configure::{
    ConfigSink, DownloadClientEntry, IndexerManagerEntry, SinkError,
};
use lodestar_companions::detect::{self, CompanionHttp};
use lodestar_companions::manager::{self, Snapshot};
use lodestar_companions::watch::{self, Seen, Watched};
use lodestar_integrations::secret::Secret;
use lodestar_pipeline::requires;
use lodestar_store::Store;
use tokio::sync::watch as channel;
use tokio::task::JoinHandle;

use crate::settings::integrations::{DownloadClient, Integrations};

/// The `integrations.clients` id of the qBittorrent Lodestar configured.
/// Any other entry is a client you set up yourself.
pub const MANAGED_CLIENT: &str = "lodestar-qbittorrent";
/// The source plugin a configured Prowlarr is handed to.
const PROWLARR_SOURCE: &str = "prowlarr";

/// The address Lodestar reaches a target at, as the sink writes it and
/// the gate compares it: `http://127.0.0.1:<port>`, `http://<name>:<port>`
/// or the URL you entered, without a trailing `/`.
pub fn target_url(t: &Target) -> String {
    let url = match t {
        Target::Loopback(p) => format!("http://127.0.0.1:{p}"),
        Target::Container(name, p) => format!("http://{name}:{p}"),
        Target::Entered(u) => u.as_str().to_owned(),
    };
    canonical(&url)
}

/// An address as parsed: scheme, host and port (the scheme's default
/// filled in, so `http://h` and `http://h:80` are one), then the path
/// without a trailing `/`. One that doesn't parse is kept as text, trimmed,
/// so it equals only itself.
fn canonical(url: &str) -> String {
    let text = url.trim();
    match reqwest::Url::parse(text) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("");
            let port = u
                .port_or_known_default()
                .map_or(String::new(), |p| format!(":{p}"));
            format!(
                "{}://{host}{port}{}",
                u.scheme(),
                u.path().trim_end_matches('/')
            )
        }
        Err(_) => text.trim_end_matches('/').to_owned(),
    }
}

/// What the gate reads, from what's known:
/// - each companion Lodestar installed, healthy only when its last check
///   passed (not yet checked counts as unhealthy: it fails closed);
/// - each one you added, as the watch last saw it; a missing one isn't
///   listed, so its dependents need it;
/// - `external_client`: an `integrations.clients` entry that isn't
///   [`MANAGED_CLIENT`], or is when no qBittorrent is Lodestar's (none
///   installed or added);
/// - `external_indexer`: the Prowlarr source is on and its saved address
///   isn't a companion's.
pub fn present(
    installed: &Snapshot,
    watched: &Watched,
    clients: &[DownloadClient],
    prowlarr: Option<&[String]>,
    added: &[Added],
) -> requires::Present {
    let mut companions = std::collections::BTreeMap::new();
    for c in &installed.companions {
        if c.version.is_some() {
            let h = match c.health {
                manager::Health::Healthy => requires::Health::Healthy,
                _ => requires::Health::Unhealthy,
            };
            companions.insert(c.id.as_str().to_owned(), h);
        }
    }
    for (id, (target, seen)) in watched {
        // Only for the target it's added at now: a removed or moved
        // companion's old result never counts.
        if !added.iter().any(|(a, t)| a == id && t == target) {
            continue;
        }
        let h = match seen {
            Seen::Healthy => requires::Health::Healthy,
            Seen::Unhealthy => requires::Health::Unhealthy,
            Seen::Missing => continue,
        };
        // A companion Lodestar installed is judged by its own health.
        companions.entry(id.as_str().to_owned()).or_insert(h);
    }
    let ours: Vec<String> = added
        .iter()
        .filter(|(id, _)| id.as_str() == PROWLARR_SOURCE)
        .map(|(_, t)| target_url(t))
        .chain(
            installed
                .companions
                .iter()
                .filter(|c| c.id.as_str() == PROWLARR_SOURCE && c.version.is_some())
                .filter_map(|c| {
                    let entry = lodestar_companions::builtin().ok()?.get(c.id.as_str())?;
                    let port = entry.port("webui")?.number;
                    Some(canonical(&format!("http://127.0.0.1:{port}")))
                }),
        )
        .collect();
    // The managed entry stands for a qBittorrent Lodestar installed or you
    // added; with neither (you forgot it, say), it's a client like yours,
    // and it still works (#653's review, L3).
    let qbittorrent_ours = added.iter().any(|(id, _)| id.as_str() == HANDED_OVER)
        || installed
            .companions
            .iter()
            .any(|c| c.id.as_str() == HANDED_OVER && c.version.is_some());
    requires::Present {
        companions,
        external_client: clients
            .iter()
            .any(|c| c.id != MANAGED_CLIENT || !qbittorrent_ours),
        external_indexer: prowlarr
            .is_some_and(|endpoints| endpoints.iter().any(|e| !ours.contains(&canonical(e)))),
    }
}

/// Whether the Prowlarr source is on. `Ok(false)` when it's off or not
/// registered; `Err` when the sources couldn't be read.
async fn prowlarr_on(ops: &dyn OpsApi) -> Result<bool, ()> {
    let sources = ops.sources().await.map_err(|_| ())?;
    Ok(sources
        .iter()
        .any(|s| s.plugin == PROWLARR_SOURCE && s.enabled))
}

/// The Prowlarr source's saved address (its config's `url`), when the
/// source is on: `Ok(None)` when it's off or has no address saved, `Err`
/// when either couldn't be read (the gate then keeps what it last knew).
/// The manifest lists no endpoints for it: the address is the one you
/// entered, in its saved config.
async fn prowlarr_endpoints(
    on: Result<bool, ()>,
    read: impl FnOnce() -> Result<Option<String>, ()> + Send + 'static,
) -> Result<Option<Vec<String>>, ()> {
    if !on? {
        return Ok(None);
    }
    let config = tokio::task::spawn_blocking(read).await.map_err(|_| ())??;
    Ok(config.and_then(|c| config_url(&c)).map(|u| vec![u]))
}

/// The Prowlarr source's saved config, as its form saved it: `Ok(None)`
/// when none is saved, `Err` when the store couldn't be read. Blocking.
fn saved_config(store: &Mutex<Store>) -> Result<Option<String>, ()> {
    store
        .lock()
        .map_err(|_| ())?
        .source_setting(PROWLARR_SOURCE, lodestar_store::SourceSetting::Config)
        .map_err(|_| ())
}

/// The `url` in a saved Prowlarr config; `None` when it doesn't parse.
fn config_url(config: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(config).ok()?;
    Some(v.get("url")?.as_str()?.to_owned())
}

/// The Prowlarr source's addresses when it's on ([`prowlarr_endpoints`];
/// a fake in tests).
type Endpoints = Arc<dyn Fn() -> BoxFuture<'static, Result<Option<Vec<String>>, ()>> + Send + Sync>;

/// A companion you added and confirmed: its id and where it is.
pub type Added = (CompanionId, Target);

/// The companion gate for the pipeline: its sender, for [`serve`], and
/// the receiver `PipelineConfig::companions` takes. With companions off
/// for the run (a fixture, a held lock, a bad catalogue) there's no
/// receiver, which turns the gate off: the sources run as before
/// (`lodestar_pipeline::requires`).
pub fn gate<T>(
    companions: Option<&T>,
) -> (
    channel::Sender<requires::Snapshot>,
    Option<requires::Companions>,
) {
    let (tx, rx) = channel::channel(requires::Snapshot::Unknown);
    (tx, companions.map(|_| rx))
}

/// What [`start`] runs, stopped when dropped.
#[derive(Debug)]
pub struct Wired {
    /// The companions you added, which the guided endpoints send after a
    /// change to their records.
    pub added: channel::Sender<Vec<Added>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Wired {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// How often the gate looks at the Prowlarr source's addresses again (they
/// change only at a start, so this is for a source turned on or off).
const REFRESH: Duration = watch::EVERY;

/// Starts the watch over the companions you added and the task that sends
/// the gate its [`requires::Snapshot`]s. The gate holds `Unknown` until
/// the first is sent, so nothing gated starts before then.
pub fn start(
    added: Vec<Added>,
    installed: channel::Receiver<Snapshot>,
    integrations: channel::Receiver<Integrations>,
    ops: Arc<dyn OpsApi>,
    store: Arc<Mutex<Store>>,
    gate: channel::Sender<requires::Snapshot>,
) -> Result<Wired, &'static str> {
    let http: Arc<dyn GuardedHttp> = Arc::new(CompanionHttp::new().map_err(|_| "companion_http")?);
    let endpoints: Endpoints = Arc::new(move || {
        let (ops, store) = (ops.clone(), store.clone());
        Box::pin(async move {
            let on = prowlarr_on(ops.as_ref()).await;
            prowlarr_endpoints(on, move || saved_config(&store)).await
        })
    });
    start_with(
        added,
        installed,
        integrations,
        endpoints,
        gate,
        http,
        Arc::new(|id: &CompanionId| detect::health(id)),
        watch::EVERY,
    )
}

/// At serve: reads the companions you added from their rows, [`start`]s
/// the watch over them and builds the guided endpoints (CMP-5g-2) over the
/// same list. A failure is logged by code and leaves the gate `Unknown`:
/// gated plugins wait (fail closed), and the guided routes answer 503.
pub async fn serve(
    companions: &crate::companions::Companions,
    settings: Arc<crate::settings::Registry>,
    ops: Arc<dyn OpsApi>,
    store: Arc<Mutex<Store>>,
    gate: channel::Sender<requires::Snapshot>,
) -> Option<(Wired, Arc<Guided>)> {
    // A companion Lodestar installs is handed over through the guided
    // path's own sink (X-cmp-native-handover). Wired first, before
    // anything here can fail, so a removal is never refused for the run
    // because the watch or the guarded client couldn't start.
    let sink = Arc::new(Sink {
        settings: settings.clone(),
        ops: ops.clone(),
        store: store.clone(),
    });
    companions.wire_sink(sink.clone());
    let service = companions.api.clone();
    let (catalogue, rt) = (service.catalogue(), runtime(service.in_container()));
    let read = store.clone();
    let stored = tokio::task::spawn_blocking(move || records(&read, catalogue, rt))
        .await
        .unwrap_or_default();
    let failed = |code: &'static str| {
        lodestar_log::log_event!(
            lodestar_companions::log_events::INSTALL_FAILED,
            companion = "all",
            version = "",
            step = "wire",
            reason = code
        );
    };
    let http: Arc<dyn GuardedHttp> = match CompanionHttp::new() {
        Ok(h) => Arc::new(h),
        Err(_) => {
            failed("companion_http");
            return None;
        }
    };
    let wired = match start(
        added_of(&stored),
        companions.snapshot.clone(),
        settings.subscribe(),
        ops.clone(),
        store.clone(),
        gate,
    ) {
        Ok(w) => w,
        Err(code) => {
            failed(code);
            return None;
        }
    };
    let guided = Arc::new(Guided::new(
        service.clone(),
        store,
        rt,
        http,
        sink,
        wired.added.clone(),
    ));
    Some((wired, guided))
}

#[allow(clippy::too_many_arguments, reason = "test seams")]
fn start_with(
    added: Vec<Added>,
    mut installed: channel::Receiver<Snapshot>,
    mut integrations: channel::Receiver<Integrations>,
    endpoints: Endpoints,
    gate: channel::Sender<requires::Snapshot>,
    http: Arc<dyn GuardedHttp>,
    checker: watch::Checker,
    every: Duration,
) -> Result<Wired, &'static str> {
    let (added_tx, mut added_rx) = channel::channel(added);
    let (list_tx, list_rx) = channel::channel(added_rx.borrow().clone());
    let (mut watched, watching) = watch::spawn(http, checker, list_rx, every);
    let composing = tokio::spawn(async move {
        let period = REFRESH.min(every * 3);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // What the Prowlarr source last read as: a read that fails keeps
        // it, so a busy store never stops a working source.
        let mut known: Option<Vec<String>> = None;
        let mut failing = false;
        loop {
            match endpoints().await {
                Ok(e) => {
                    known = e;
                    failing = false;
                }
                Err(()) => {
                    if !failing {
                        lodestar_log::log_event!(
                            lodestar_companions::log_events::CONFIG_FAILED,
                            companion = PROWLARR_SOURCE,
                            setting = "gate.prowlarr_address",
                            reason = "read_failed"
                        );
                    }
                    failing = true;
                }
            }
            let endpoints = known.clone();
            let snapshot = {
                let added = added_rx.borrow_and_update().clone();
                list_tx.send_if_modified(|l| {
                    let changed = *l != added;
                    l.clone_from(&added);
                    changed
                });
                requires::Snapshot::Known(present(
                    &installed.borrow_and_update(),
                    &watched.borrow_and_update(),
                    &integrations.borrow_and_update().clients,
                    endpoints.as_deref(),
                    &added,
                ))
            };
            gate.send_if_modified(|g| {
                let changed = *g != snapshot;
                *g = snapshot;
                changed
            });
            tokio::select! {
                _ = tick.tick() => {}
                r = installed.changed() => if r.is_err() { return },
                r = watched.changed() => if r.is_err() {
                    // The watch stopped (it ended or panicked): the gate's
                    // sender goes with this task, and the gate reads as
                    // unknown, which holds gated sources.
                    lodestar_log::log_event!(
                        lodestar_companions::log_events::INSTALL_FAILED,
                        companion = "all",
                        version = "",
                        step = "watch",
                        reason = "stopped"
                    );
                    return;
                },
                r = integrations.changed() => if r.is_err() { return },
                r = added_rx.changed() => if r.is_err() { return },
            }
        }
    });
    Ok(Wired {
        added: added_tx,
        tasks: vec![watching, composing],
    })
}

/// CMP-8b's `ConfigSink` over the settings registry and the sources.
pub struct Sink {
    pub settings: Arc<crate::settings::Registry>,
    pub ops: Arc<dyn OpsApi>,
    /// A read-write connection, for the Prowlarr source's saved config.
    pub store: Arc<Mutex<Store>>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink").finish_non_exhaustive()
    }
}

/// What the managed client entry holds, from CMP-8b's entry.
struct ManagedClient {
    target: Target,
    username: String,
    password: Secret,
    category: String,
}

/// Every client but [`MANAGED_CLIENT`], each without its password, which
/// keeps the saved one (the registry keeps a saved secret for an item of
/// the same id, kind and address).
fn other_clients(current: &Integrations) -> Result<Vec<serde_json::Value>, SinkError> {
    current
        .clients
        .iter()
        .filter(|c| c.id != MANAGED_CLIENT)
        .map(|c| {
            let mut c = c.clone();
            c.password = None;
            serde_json::to_value(c).map_err(|_| SinkError::Rejected)
        })
        .collect()
}

/// The `clients` list that makes `entry` the managed client: the list as it
/// is, with [`MANAGED_CLIENT`] replaced or added. A name or category you
/// gave the managed entry is kept unless the hand-over sets its own
/// category, so a restart's hand-over doesn't wipe them.
fn with_managed(
    current: &Integrations,
    entry: &ManagedClient,
) -> Result<Vec<serde_json::Value>, SinkError> {
    let mut clients = other_clients(current)?;
    let kept = current.client(MANAGED_CLIENT);
    let name = kept.map_or("qBittorrent (Lodestar)", |c| c.name.as_str());
    let category = match (entry.category.as_str(), kept) {
        ("", Some(c)) => c.category.clone(),
        (own, _) => own.to_owned(),
    };
    clients.push(serde_json::json!({
        "id": MANAGED_CLIENT,
        "name": name,
        "kind": "qbittorrent",
        "url": format!("{}/", target_url(&entry.target)),
        "username": entry.username,
        "password": entry.password.expose(),
        "category": category,
    }));
    Ok(clients)
}

/// How many times a save that lost to another one is tried again.
const SAVE_TRIES: usize = 3;

/// The `integrations` batch saving `clients`, refused over any save after
/// `revision`.
fn clients_batch(revision: u64, clients: Vec<serde_json::Value>) -> SettingsBatch {
    let mut patch = serde_json::Map::new();
    patch.insert("if_revision".into(), revision.into());
    patch.insert("clients".into(), serde_json::Value::Array(clients));
    SettingsBatch(std::iter::once(("integrations".to_owned(), SettingsPatch(patch))).collect())
}

/// Saves the `clients` list `edit` makes from the one saved now (`None`:
/// nothing to save), never over a save made in between: the list and its
/// revision come from one read of the section (the registry makes both
/// under one lock), the revision is sent as `if_revision`, and a stale one
/// is read and made again, at most [`SAVE_TRIES`] times (#679's review).
/// The read has no passwords, which `edit` never sends for your clients
/// anyway. Blocking.
fn save_clients(
    settings: &crate::settings::Registry,
    edit: impl Fn(&Integrations) -> Result<Option<Vec<serde_json::Value>>, SinkError>,
) -> Result<(), SinkError> {
    for _ in 0..SAVE_TRIES {
        let values = settings
            .section("integrations")
            .ok_or(SinkError::Unavailable)?;
        let mut current = settings.subscribe::<Integrations>().borrow().clone();
        current.clients = values
            .fields
            .get("clients")
            .map(|v| serde_json::from_value(v.value.clone()))
            .ok_or(SinkError::Unavailable)?
            .map_err(|_| SinkError::Unavailable)?;
        let revision = values.revision;
        let Some(clients) = edit(&current)? else {
            return Ok(());
        };
        match settings.save(clients_batch(revision, clients)) {
            Ok(_) => return Ok(()),
            Err(f) if matches!(f.error, lodestar_api::settings::SettingsError::Stale(_)) => {}
            Err(_) => return Err(SinkError::Rejected),
        }
    }
    Err(SinkError::Unavailable)
}

/// Saves `managed` as the managed client. Blocking.
fn save_client(
    settings: &crate::settings::Registry,
    managed: &ManagedClient,
) -> Result<(), SinkError> {
    save_clients(settings, |current| with_managed(current, managed).map(Some))
}

/// Takes [`MANAGED_CLIENT`] out of `integrations.clients`, keeping every
/// other one. Nothing to save when it isn't there. Blocking.
fn forget_client(settings: &crate::settings::Registry) -> Result<(), SinkError> {
    save_clients(settings, |current| {
        if current.client(MANAGED_CLIENT).is_none() {
            return Ok(None);
        }
        other_clients(current).map(Some)
    })
}

/// Whether a [`MANAGED_CLIENT`] entry is saved.
fn has_managed_client(settings: &crate::settings::Registry) -> bool {
    settings
        .subscribe::<Integrations>()
        .borrow()
        .client(MANAGED_CLIENT)
        .is_some()
}

impl crate::companions::ManagedSink for Sink {
    fn has_download_client(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let settings = self.settings.clone();
            // An unanswered read counts as present: it fails closed.
            tokio::task::spawn_blocking(move || has_managed_client(&settings))
                .await
                .unwrap_or(true)
        })
    }

    fn forget_download_client(&self) -> BoxFuture<'_, Result<(), SinkError>> {
        Box::pin(async move {
            let settings = self.settings.clone();
            tokio::task::spawn_blocking(move || forget_client(&settings))
                .await
                .map_err(|_| SinkError::Unavailable)?
        })
    }
}

/// The Prowlarr source's new config: the companion's address, keeping any
/// other setting you saved (its `agents`).
fn prowlarr_config(previous: Option<&str>, url: &str) -> serde_json::Value {
    let mut config = previous
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    config["url"] = url.into();
    config["manager"] = "prowlarr".into();
    config
}

impl ConfigSink for Sink {
    fn download_client<'a>(
        &'a self,
        entry: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            let managed = ManagedClient {
                target: entry.target,
                username: entry.username.as_str().to_owned(),
                password: entry.password,
                category: entry
                    .category
                    .as_ref()
                    .map_or(String::new(), |c| c.as_str().to_owned()),
            };
            let settings = self.settings.clone();
            tokio::task::spawn_blocking(move || save_client(&settings, &managed))
                .await
                .map_err(|_| SinkError::Unavailable)?
        })
    }

    fn indexer_manager<'a>(
        &'a self,
        entry: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            let store = self.store.clone();
            let previous = tokio::task::spawn_blocking(move || saved_config(&store))
                .await
                .map_err(|_| SinkError::Unavailable)?
                .map_err(|_| SinkError::Unavailable)?;
            let writes = OpsWrites(self.ops.as_ref());
            hand_over_prowlarr(
                &writes,
                previous.as_deref(),
                &target_url(&entry.target),
                entry.api_key,
            )
            .await
        })
    }
}

/// What the Prowlarr hand-over writes to the source: the ops in shipped
/// builds, a recorder in tests.
trait SourceWrites: Send + Sync {
    /// The Prowlarr source's id; `None` when it isn't registered.
    fn source<'a>(&'a self) -> BoxFuture<'a, Option<u8>>;
    fn config<'a>(&'a self, id: u8, config: serde_json::Value) -> BoxFuture<'a, bool>;
    /// Stores (`Some`) or removes (`None`) the source's `api_key`.
    fn key<'a>(&'a self, id: u8, key: Option<Secret>) -> BoxFuture<'a, bool>;
}

struct OpsWrites<'o>(&'o dyn OpsApi);

impl SourceWrites for OpsWrites<'_> {
    fn source<'a>(&'a self) -> BoxFuture<'a, Option<u8>> {
        Box::pin(async move {
            let sources = self.0.sources().await.ok()?;
            sources
                .iter()
                .find(|s| s.plugin == PROWLARR_SOURCE)
                .map(|s| s.id)
        })
    }
    fn config<'a>(&'a self, id: u8, config: serde_json::Value) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let patch = SourcePatch {
                config: Some(config),
                ..SourcePatch::default()
            };
            self.0.patch_source(id, patch, false).await.is_ok()
        })
    }
    fn key<'a>(&'a self, id: u8, key: Option<Secret>) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.0
                .patch_source_credentials(id, vec![("api_key".into(), key)])
                .await
                .is_ok()
        })
    }
}

/// Hands a configured Prowlarr to the source (runner's call on #645's
/// delta review, option a, "replace"):
/// 1. the companion's key replaces the one you saved, so the source never
///    loses its credential (removing an Account source's last credential
///    turns it off for good, `Control::update_credentials`);
/// 2. then the address (keeping your other settings, such as `agents`).
///
/// If step 2 fails, the companion's key is removed, which turns the source
/// off: it fails closed, and your own key is gone (you enter it again). A
/// failed removal is logged by name, never with a value. Every write
/// applies at the next start (`restart_required`).
///
/// **Accepted:** between the two steps the companion's key sits beside
/// your own address. It can reach that server only after a restart in that
/// window, or if both step 2 and the removal fail.
async fn hand_over_prowlarr(
    w: &dyn SourceWrites,
    previous: Option<&str>,
    url: &str,
    key: Secret,
) -> Result<(), SinkError> {
    let id = w.source().await.ok_or(SinkError::Unavailable)?;
    if !w.key(id, Some(key)).await {
        // It may have been saved before the read-back failed, so it's
        // left as it is (removing it could take your own key) and logged.
        lodestar_log::log_event!(
            lodestar_companions::log_events::CONFIG_FAILED,
            companion = PROWLARR_SOURCE,
            setting = "lodestar.settings",
            reason = "key_not_confirmed"
        );
        return Err(SinkError::Rejected);
    }
    if w.config(id, prowlarr_config(previous, url)).await {
        return Ok(());
    }
    if !w.key(id, None).await {
        lodestar_log::log_event!(
            lodestar_companions::log_events::CONFIG_FAILED,
            companion = PROWLARR_SOURCE,
            setting = "lodestar.settings",
            reason = "rollback_failed"
        );
    }
    Err(SinkError::Rejected)
}

// ─── CMP-5g-2: guided and detected companions ─────────────────────────

use lodestar_api::companions::CompanionsError;
use lodestar_api::companions_guided::{CompanionsGuidedApi, GuidedError};
use lodestar_api::dto::companions::Companion;
use lodestar_api::dto::companions_guided::{
    CompanionForgotten, CompanionTarget, DetectedCompanion,
};
use lodestar_companions::adapter::{AdapterError, Credentials, EnteredUrl};
use lodestar_companions::credentials::{CredentialKey, Field};
use lodestar_companions::detect::Runtime;
use lodestar_companions::guided::{self, Origin as RecordOrigin, Provenance, Record, StoredTarget};
use lodestar_companions::{Catalogue, Entry};
use lodestar_integrations::secret::SecretStore;

/// How long detection may take in all (runner: a request behind a reverse
/// proxy must stay short). Each probe also keeps its own `Limits::PROBE`.
pub const DETECT_CAP: Duration = Duration::from_secs(15);
/// The least time between two plans: each costs qBittorrent a failed
/// sign-in (CMP-8b), and it bans after five by default.
pub const PLAN_EVERY: Duration = Duration::from_secs(60);
/// The one companion whose hand-over this version makes (runner: Prowlarr
/// waits for `X-prowlarr-settings`).
const HANDED_OVER: &str = "qbittorrent";
/// The WebUI account qBittorrent's temporary password is for: its own
/// built-in one (Q19). The hand-over keeps it; only the password changes.
const GUIDED_USERNAME: &str = "admin";

/// A guided run's input, as [`GUIDED_USERNAME`], setting the credential
/// `holding` has. With `temporary` it's the guided install (Q19), signing
/// in with that; without, it's a companion you confirmed, signing in with
/// the stored generated password: a retry after a failed apply or
/// hand-over (#653's review, B1).
fn input_for<'a>(
    holding: &'a Holding,
    target: &Target,
    temporary: Option<Secret>,
) -> Result<lodestar_companions::adapter::ConfigureInput<'a>, CompanionsError> {
    let username = lodestar_companions::config_files::Username::parse(GUIDED_USERNAME)
        .map_err(|_| CompanionsError::Internal("username"))?;
    let confirmation = lodestar_companions::configure::Confirmation::of(target.clone());
    let input = lodestar_companions::adapter::ConfigureInput::new(holding).username(username);
    Ok(match temporary {
        Some(t) => input.guided(confirmation, t),
        None => input.confirmed(confirmation),
    })
}

/// Refused sign-ins in a row before plans and confirmations stop asking:
/// qBittorrent bans an address after five (#653's review, L2).
pub const SIGN_IN_CAP: u32 = 3;

/// The credential a guided step works with, read with its provenance.
enum Held {
    /// The temporary password you pasted, and its sealed row.
    Pasted(lodestar_integrations::secret::SealedSecret, Secret),
    /// The generated password, set or being set on qBittorrent, before
    /// the hand-over completed: a retry signs in with it.
    Pending(Secret),
}

/// Detection's runtime here (Q12: container names only under Docker's
/// DNS).
pub fn runtime(in_container: bool) -> Runtime {
    if in_container {
        Runtime::Container
    } else {
        Runtime::Native
    }
}

/// Every companion row's text, read once. Blocking.
fn texts(store: &Mutex<Store>) -> Result<Vec<(String, String)>, ()> {
    let rows = store
        .lock()
        .map_err(|_| ())?
        .settings_with_prefix_raw(lodestar_companions::credentials::ROW_PREFIX)
        .map_err(|_| ())?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, v)| v.ok().map(|v| (k, v)))
        .collect())
}

/// The companions you added and confirmed, from their rows. A row that
/// doesn't read is no record (`guided`'s rule). Blocking.
pub fn records(store: &Mutex<Store>, catalogue: &Catalogue, runtime: Runtime) -> Vec<Record> {
    let rows = texts(store).unwrap_or_default();
    let text = |key: &str| rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    catalogue
        .entries()
        .iter()
        .filter_map(|e| Record::read(e, runtime, text))
        .collect()
}

/// The records as the watch takes them.
fn added_of(records: &[Record]) -> Vec<Added> {
    records
        .iter()
        .map(|r| (r.id.clone(), r.target.clone()))
        .collect()
}

/// One credential, held for a configuration run.
struct Holding(Field, Secret);

impl Credentials for Holding {
    fn get(
        &self,
        field: &str,
    ) -> Result<Option<Secret>, lodestar_companions::adapter::CredentialError> {
        Ok((field == self.0.as_str()).then(|| self.1.clone()))
    }
}

/// An adapter's failure as a stable code; never a value.
fn adapter_code(e: AdapterError) -> &'static str {
    match e {
        AdapterError::Refused(code) => code,
        AdapterError::Unauthorized => "companion_unauthorized",
        AdapterError::Timeout | AdapterError::Unreachable => "companion_unreachable",
        AdapterError::Blocked => "address_refused",
        AdapterError::NotThisCompanion => "not_detected",
        _ => "companion_protocol",
    }
}

/// A guided step on the one companion handed over that failed after it
/// changed something, by code only.
fn failed(reason: &'static str) {
    lodestar_log::log_event!(
        lodestar_companions::log_events::CONFIG_FAILED,
        companion = HANDED_OVER,
        setting = "guided",
        reason = reason
    );
}

fn target_dto(t: &Target) -> CompanionTarget {
    match StoredTarget::of(t) {
        StoredTarget::Loopback { port } => CompanionTarget::Loopback { port },
        StoredTarget::Container { port } => CompanionTarget::Container { port },
        StoredTarget::Url { url } => CompanionTarget::Url { url },
    }
}

/// A requested target as one detection may use for `entry` here: a
/// catalogue port on loopback (or, in a container, its container name),
/// or an address you typed that the egress rules let Lodestar reach
/// (`http`/`https`, no user name, query or fragment; link-local and cloud
/// metadata addresses refused; a name's answer is checked at connect).
fn checked_target(
    entry: &Entry,
    runtime: Runtime,
    t: CompanionTarget,
) -> Result<Target, CompanionsError> {
    let stored = match t {
        CompanionTarget::Loopback { port } => StoredTarget::Loopback { port },
        CompanionTarget::Container { port } => StoredTarget::Container { port },
        CompanionTarget::Url { url } => {
            let entered = EnteredUrl::parse(&url).map_err(|_| CompanionsError::BadInput {
                field: Some("url"),
                code: "bad_template_input",
            })?;
            let parsed =
                reqwest::Url::parse(entered.as_str()).map_err(|_| CompanionsError::BadInput {
                    field: Some("url"),
                    code: "bad_template_input",
                })?;
            lodestar_integrations::net::check_url(
                &parsed,
                lodestar_integrations::net::NetPolicy::download_client(),
            )
            .map_err(|_| CompanionsError::Refused("address_refused"))?;
            return Ok(Target::Entered(entered));
        }
    };
    stored
        .target(entry, runtime)
        .ok_or(CompanionsError::BadInput {
            field: Some("port"),
            code: "bad_template_input",
        })
}

/// `/companions/{id}/…`'s guided half over the store, the secrets file,
/// CMP-8a's detection, CMP-8b's configuration and the [`Sink`].
pub struct Guided {
    service: Arc<crate::companions::Service>,
    store: Arc<Mutex<Store>>,
    secrets: Arc<SecretStore>,
    catalogue: &'static Catalogue,
    runtime: Runtime,
    http: Arc<dyn GuardedHttp>,
    sink: Arc<dyn ConfigSink>,
    /// The watch's list (`Wired::added`), sent after every change.
    added: channel::Sender<Vec<Added>>,
    /// Detection's overall cap: [`DETECT_CAP`], shorter in tests.
    detect_cap: Duration,
    /// One detection at a time.
    detecting: tokio::sync::Mutex<()>,
    /// One guided change at a time (target, paste, plan, confirm,
    /// forget): each reads rows another writes, and a provenance must
    /// never describe another step's credential. Shared with a native
    /// install (`Service::changes`), so neither writes the managed entry
    /// while the other runs (#679's review, Important 1).
    changing: Arc<tokio::sync::Mutex<()>>,
    last_plan: Mutex<Option<tokio::time::Instant>>,
    /// The last plan's settings, taken by the next confirmation: one
    /// confirmation per plan, and a mismatch is refused before it signs
    /// in again.
    planned: Mutex<Option<Vec<String>>>,
    /// Refused sign-ins in a row, across plans and confirmations; reset by
    /// one that signs in, or by forget.
    failures: Mutex<u32>,
}

impl std::fmt::Debug for Guided {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guided").finish_non_exhaustive()
    }
}

impl Guided {
    pub fn new(
        service: Arc<crate::companions::Service>,
        store: Arc<Mutex<Store>>,
        runtime: Runtime,
        http: Arc<dyn GuardedHttp>,
        sink: Arc<dyn ConfigSink>,
        added: channel::Sender<Vec<Added>>,
    ) -> Guided {
        Guided {
            changing: service.changes(),
            secrets: service.secrets(),
            catalogue: service.catalogue(),
            service,
            store,
            runtime,
            http,
            sink,
            added,
            detect_cap: DETECT_CAP,
            detecting: tokio::sync::Mutex::new(()),
            last_plan: Mutex::new(None),
            planned: Mutex::new(None),
            failures: Mutex::new(0),
        }
    }

    fn entry(&self, id: &str) -> Result<&'static Entry, CompanionsError> {
        CompanionId::parse(id).map_err(|_| CompanionsError::Unknown)?;
        self.catalogue.get(id).ok_or(CompanionsError::Unknown)
    }

    /// The guided lock, or `busy`.
    fn exclusive(&self) -> Result<tokio::sync::OwnedMutexGuard<()>, CompanionsError> {
        self.changing
            .clone()
            .try_lock_owned()
            .map_err(|_| CompanionsError::Refused("busy"))
    }

    /// Forgets the last plan: the rows it was made from changed.
    fn unplan(&self) {
        if let Ok(mut p) = self.planned.lock() {
            *p = None;
        }
    }

    /// Refuses a companion already added and confirmed: its record and
    /// generated credential stay until you forget it.
    async fn not_added(&self, e: &'static Entry) -> Result<(), CompanionsError> {
        let key = guided::origin_key(&e.id);
        let origin = self
            .blocking(move |s| Ok(texts(s)?.into_iter().any(|(k, _)| k == key)))
            .await?;
        if origin {
            return Err(CompanionsError::Refused("already_added"));
        }
        Ok(())
    }

    /// The companion this version hands over, and not one Lodestar
    /// installed (that one is managed on its own) or is installing.
    async fn handed_over(&self, id: &str) -> Result<&'static Entry, CompanionsError> {
        let e = self.entry(id)?;
        if e.id.as_str() != HANDED_OVER {
            return Err(CompanionsError::Refused("handover_later"));
        }
        let service = self.service.clone();
        let cid = e.id.clone();
        if let Some(code) = tokio::task::spawn_blocking(move || service.lodestar_owned(&cid))
            .await
            .map_err(|_| CompanionsError::Internal("panicked"))??
        {
            return Err(CompanionsError::Refused(code));
        }
        Ok(e)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Mutex<Store>) -> Result<T, ()> + Send + 'static,
    ) -> Result<T, CompanionsError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|_| CompanionsError::Internal("panicked"))?
            .map_err(|()| CompanionsError::Internal("store"))
    }

    /// The saved target, valid here.
    async fn target(&self, e: &'static Entry) -> Result<Target, CompanionsError> {
        let key = guided::target_key(&e.id);
        let rt = self.runtime;
        self.blocking(move |s| {
            Ok(texts(s)?
                .into_iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v))
        })
        .await?
        .and_then(|t| StoredTarget::from_row(&t))
        .and_then(|t| t.target(e, rt))
        .ok_or(CompanionsError::Refused("no_target"))
    }

    /// The provenance of `e`'s credential and its sealed row, read in one
    /// query.
    async fn credential(
        &self,
        e: &'static Entry,
    ) -> Result<
        (
            Option<Provenance>,
            Option<lodestar_integrations::secret::SealedSecret>,
        ),
        CompanionsError,
    > {
        let key = CredentialKey::new(e.id.clone(), Field::WebUiPassword).row_key();
        let id = e.id.clone();
        self.blocking(move |s| {
            let rows = s
                .lock()
                .map_err(|_| ())?
                .settings_with_prefix_raw(lodestar_companions::credentials::ROW_PREFIX)
                .map_err(|_| ())?;
            let text = |k: &str| {
                rows.iter()
                    .find(|(x, _)| x == k)
                    .and_then(|(_, v)| v.as_ref().ok().cloned())
            };
            let provenance = guided::provenance(&id, Field::WebUiPassword, text);
            let sealed = match rows.iter().find(|(k, _)| *k == key) {
                Some((_, Err(bytes))) => Some(
                    lodestar_integrations::secret::SealedSecret::from_bytes(bytes.clone())
                        .map_err(|_| ())?,
                ),
                _ => None,
            };
            Ok((provenance, sealed))
        })
        .await
    }

    /// The credential to work with: the pasted temporary password, or the
    /// generated one a failed apply or hand-over left.
    async fn held(&self, e: &'static Entry) -> Result<Held, CompanionsError> {
        let key = CredentialKey::new(e.id.clone(), Field::WebUiPassword);
        let (provenance, sealed) = self.credential(e).await?;
        let sealed = sealed.ok_or(CompanionsError::Refused("no_temporary_password"))?;
        let open = |s: &lodestar_integrations::secret::SealedSecret| {
            lodestar_companions::credentials::open(&self.secrets, &key, s)
                .map_err(|_| CompanionsError::Internal("sealed"))
        };
        match provenance {
            Some(Provenance::Pasted) => {
                let secret = open(&sealed)?;
                Ok(Held::Pasted(sealed, secret))
            }
            Some(Provenance::Generated) => Ok(Held::Pending(open(&sealed)?)),
            None => Err(CompanionsError::Refused("no_temporary_password")),
        }
    }

    /// Refuses a retry anywhere but the target the generated password was
    /// set on: the live password never goes to another address (#653's
    /// review, B2).
    async fn applied_here(
        &self,
        e: &'static Entry,
        target: &Target,
    ) -> Result<(), CompanionsError> {
        let key = guided::applied_key(&e.id);
        let applied = self
            .blocking(move |s| {
                Ok(texts(s)?
                    .into_iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v))
            })
            .await?;
        if applied.as_deref().and_then(StoredTarget::from_row) != Some(StoredTarget::of(target)) {
            return Err(CompanionsError::Refused("target_changed"));
        }
        Ok(())
    }

    /// Refuses once qBittorrent has refused [`SIGN_IN_CAP`] sign-ins in a
    /// row, before it would ban Lodestar's address.
    fn sign_ins_left(&self) -> Result<(), CompanionsError> {
        let n = *self
            .failures
            .lock()
            .map_err(|_| CompanionsError::Internal("lock"))?;
        if n >= SIGN_IN_CAP {
            return Err(CompanionsError::Refused("sign_in_refused"));
        }
        Ok(())
    }

    /// Counts a refused sign-in, or resets the count on one that worked.
    fn counted<T>(&self, r: Result<T, CompanionsError>) -> Result<T, CompanionsError> {
        if let Ok(mut n) = self.failures.lock() {
            match &r {
                Ok(_) => *n = 0,
                Err(CompanionsError::Refused("companion_unauthorized")) => {
                    *n = n.saturating_add(1);
                }
                Err(_) => {}
            }
        }
        r
    }

    /// Sends the watch the records as stored now.
    async fn refresh(&self) {
        let (store, cat, rt) = (self.store.clone(), self.catalogue, self.runtime);
        if let Ok(r) = tokio::task::spawn_blocking(move || records(&store, cat, rt)).await {
            self.added.send_replace(added_of(&r));
        }
    }

    /// The plan for `target`: signing in with `temporary` when given (the
    /// credential held is then the temporary password too, so a wrong one
    /// costs one refused sign-in), else with `stored`. Plans name settings,
    /// never values, so the generated password isn't needed to make one.
    async fn plan_with(
        &self,
        e: &'static Entry,
        target: &Target,
        stored: Secret,
        temporary: Option<Secret>,
    ) -> Result<lodestar_companions::adapter::ConfigPlan, CompanionsError> {
        let configure = lodestar_companions::configure::configurator(&e.id)
            .ok_or(CompanionsError::Refused("handover_later"))?;
        let holding = Holding(Field::WebUiPassword, stored);
        let input = input_for(&holding, target, temporary)?;
        self.counted(
            configure
                .plan(self.http.as_ref(), target, &input)
                .await
                .map_err(|e| CompanionsError::Refused(adapter_code(e))),
        )
    }

    /// Applies `plan` setting `stored`, hands the companion over, and
    /// writes its record: the confirmation's end, first try or retry.
    async fn finish(
        &self,
        e: &'static Entry,
        target: &Target,
        plan: &lodestar_companions::adapter::ConfigPlan,
        stored: Secret,
        temporary: Option<Secret>,
    ) -> Result<Companion, GuidedError> {
        let configure = lodestar_companions::configure::configurator(&e.id)
            .ok_or(CompanionsError::Refused("handover_later"))?;
        let holding = Holding(Field::WebUiPassword, stored);
        let input = input_for(&holding, target, temporary)?;
        // CMP-8b logs a failed step itself.
        configure
            .apply(self.http.as_ref(), target, &input, plan)
            .await
            .map_err(|err| CompanionsError::Refused(adapter_code(err)))?;
        // Under the guided lock: anything a crashed native install left
        // pending for it is no longer Lodestar's to release, now that this
        // entry is yours (#679 follow-up's self-review B2).
        let service = self.service.clone();
        let cid = e.id.clone();
        tokio::task::spawn_blocking(move || service.clear_handover(&cid))
            .await
            .map_err(|_| CompanionsError::Internal("panicked"))?;
        lodestar_companions::configure::hand_over(self.sink.as_ref(), &e.id, target, &input)
            .await
            .map_err(|err| CompanionsError::Refused(adapter_code(err)))?;
        let origin = guided::origin_key(&e.id);
        self.blocking(move |s| {
            s.lock()
                .map_err(|_| ())?
                .set_settings_text(&[(&origin, RecordOrigin::Confirmed.as_str())])
                .map_err(|_| ())
        })
        .await?;
        self.refresh().await;
        Ok(lodestar_api::companions::CompanionsApi::get(
            self.service.as_ref(),
            e.id.as_str().to_owned(),
        )
        .await?)
    }
}

impl CompanionsGuidedApi for Guided {
    fn detect(
        &self,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Vec<DetectedCompanion>, GuidedError>> {
        Box::pin(async move {
            let _one = self
                .detecting
                .try_lock()
                .map_err(|_| CompanionsError::Refused("busy"))?;
            let probes: Vec<(usize, CompanionId, Target)> =
                detect::auto_probes(self.catalogue, self.runtime)
                    .into_iter()
                    .enumerate()
                    .map(|(i, (id, t))| (i, id.clone(), t))
                    .collect();
            let mut set = tokio::task::JoinSet::new();
            for (i, id, target) in probes {
                let http = self.http.clone();
                set.spawn(async move {
                    let d = detect::detector(&id)?;
                    let found = d.detect(http.as_ref(), &target).await.ok().flatten()?;
                    Some((i, id, found))
                });
            }
            let mut found = Vec::new();
            let _ = tokio::time::timeout(self.detect_cap, async {
                while let Some(r) = set.join_next().await {
                    if let Ok(Some(hit)) = r {
                        found.push(hit);
                    }
                }
            })
            .await;
            set.abort_all();
            // Each companion once, at the first target that answered, in
            // the fixed probe order.
            found.sort_by_key(|(i, _, _)| *i);
            let mut out: Vec<DetectedCompanion> = Vec::new();
            for (_, id, d) in found {
                if out.iter().any(|o| o.id == id.as_str()) {
                    continue;
                }
                out.push(DetectedCompanion {
                    id: id.as_str().to_owned(),
                    target: target_dto(&d.target),
                    version: d.version.map(|v| v.as_str().to_owned()),
                });
            }
            Ok(out)
        })
    }

    fn set_target(
        &self,
        id: String,
        target: CompanionTarget,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<DetectedCompanion, GuidedError>> {
        Box::pin(async move {
            // The lock first, then the check: an install that finished
            // just before is seen (#679's review, Important 1).
            let _one = self.exclusive()?;
            let e = self.handed_over(&id).await?;
            self.not_added(e).await?;
            // The generated password may be qBittorrent's: a retry signs in
            // with it, so its address never moves (#653's review, B2).
            if self.credential(e).await?.0 == Some(Provenance::Generated) {
                return Err(CompanionsError::Refused("generated_pending").into());
            }
            let target = checked_target(e, self.runtime, target)?;
            let detector =
                detect::detector(&e.id).ok_or(CompanionsError::Refused("not_detected"))?;
            let found = detector
                .detect(self.http.as_ref(), &target)
                .await
                .map_err(|err| CompanionsError::Refused(adapter_code(err)))?
                .ok_or(CompanionsError::Refused("not_detected"))?;
            let key = guided::target_key(&e.id);
            let row = StoredTarget::of(&target).to_row();
            self.blocking(move |s| {
                s.lock()
                    .map_err(|_| ())?
                    .set_settings_text(&[(&key, &row)])
                    .map_err(|_| ())
            })
            .await?;
            self.unplan();
            Ok(DetectedCompanion {
                id: e.id.as_str().to_owned(),
                target: target_dto(&target),
                version: found.version.map(|v| v.as_str().to_owned()),
            })
        })
    }

    fn paste(
        &self,
        id: String,
        value: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<(), GuidedError>> {
        Box::pin(async move {
            // The lock first, then the check: an install that finished
            // just before is seen (#679's review, Important 1).
            let _one = self.exclusive()?;
            let e = self.handed_over(&id).await?;
            self.not_added(e).await?;
            // The generated password may already be qBittorrent's: never
            // drop it for a paste (#653's review, B1).
            if self.credential(e).await?.0 == Some(Provenance::Generated) {
                return Err(CompanionsError::Refused("generated_pending").into());
            }
            self.unplan();
            self.target(e).await?;
            let key = CredentialKey::new(e.id.clone(), Field::WebUiPassword);
            let sealed = lodestar_companions::credentials::accept_pasted(
                &self.secrets,
                &key,
                Secret::new(value),
            )
            .map_err(|_| CompanionsError::BadInput {
                field: Some("value"),
                code: "bad_value",
            })?;
            let prov = guided::provenance_key(&e.id, Field::WebUiPassword)
                .ok_or(CompanionsError::Internal("provenance"))?;
            let rows = crate::companions::StoreRows(self.store.clone());
            // The provenance first: a crash between the two leaves the old
            // credential marked pasted, which is never revealed.
            self.blocking(move |s| {
                s.lock()
                    .map_err(|_| ())?
                    .set_settings_text(&[(&prov, Provenance::Pasted.as_str())])
                    .map_err(|_| ())?;
                use lodestar_companions::manager::Rows as _;
                rows.put(&sealed).map_err(|_| ())
            })
            .await?;
            Ok(())
        })
    }

    fn plan(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Vec<String>, GuidedError>> {
        Box::pin(async move {
            // The lock first, then the check: an install that finished
            // just before is seen (#679's review, Important 1).
            let _one = self.exclusive()?;
            let e = self.handed_over(&id).await?;
            self.not_added(e).await?;
            let target = self.target(e).await?;
            let held = self.held(e).await?;
            self.sign_ins_left()?;
            {
                let mut last = self
                    .last_plan
                    .lock()
                    .map_err(|_| CompanionsError::Internal("lock"))?;
                let now = tokio::time::Instant::now();
                if let Some(at) = *last {
                    let since = now.duration_since(at);
                    if since < PLAN_EVERY {
                        return Err(GuidedError::RateLimited {
                            retry_after: (PLAN_EVERY - since).as_secs_f64().ceil().max(1.0) as u64,
                        });
                    }
                }
                *last = Some(now);
            }
            let plan = match held {
                Held::Pasted(_, temporary) => {
                    self.plan_with(e, &target, temporary.clone(), Some(temporary))
                        .await?
                }
                Held::Pending(generated) => {
                    self.applied_here(e, &target).await?;
                    self.plan_with(e, &target, generated, None).await?
                }
            };
            let settings: Vec<String> = plan
                .changes()
                .iter()
                .map(|k| k.as_str().to_owned())
                .collect();
            *self
                .planned
                .lock()
                .map_err(|_| CompanionsError::Internal("lock"))? = Some(settings.clone());
            Ok(settings)
        })
    }

    fn confirm(
        &self,
        id: String,
        settings: Vec<String>,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<Companion, GuidedError>> {
        Box::pin(async move {
            // The lock first, then the check: an install that finished
            // just before is seen (#679's review, Important 1).
            let _one = self.exclusive()?;
            let e = self.handed_over(&id).await?;
            self.not_added(e).await?;
            // One confirmation per plan, and one that doesn't echo it is
            // refused before qBittorrent is signed in to again.
            let planned = self
                .planned
                .lock()
                .map_err(|_| CompanionsError::Internal("lock"))?
                .take();
            if planned.as_ref() != Some(&settings) {
                return Err(CompanionsError::Mismatch.into());
            }
            let target = self.target(e).await?;
            let held = self.held(e).await?;
            self.sign_ins_left()?;
            let echoes = |p: &lodestar_companions::adapter::ConfigPlan| {
                p.changes()
                    .iter()
                    .map(|k| k.as_str())
                    .eq(settings.iter().map(String::as_str))
            };
            let (sealed, temporary) = match held {
                // A retry after a failed apply or hand-over: the generated
                // password signs in, and nothing is replaced.
                Held::Pending(generated) => {
                    self.applied_here(e, &target).await?;
                    let plan = self.plan_with(e, &target, generated.clone(), None).await?;
                    if !echoes(&plan) {
                        return Err(CompanionsError::Mismatch.into());
                    }
                    return self.finish(e, &target, &plan, generated, None).await;
                }
                Held::Pasted(sealed, temporary) => (sealed, temporary),
            };
            let key = CredentialKey::new(e.id.clone(), Field::WebUiPassword);
            let change = lodestar_companions::credentials::replace_temporary_password(
                &self.secrets,
                &key,
                &sealed,
            )
            .map_err(|_| CompanionsError::Internal("sealed"))?;
            let prov = guided::provenance_key(&e.id, Field::WebUiPassword)
                .ok_or(CompanionsError::Internal("provenance"))?;
            // The generated password is stored durably first (CMP-8b), then
            // marked generated: a crash between leaves it marked pasted,
            // which is never revealed.
            // The target it's set on goes with the marking: a retry signs in
            // only there.
            let rows = crate::companions::StoreRows(self.store.clone());
            let next_row = change.next.row.clone();
            let prov_set = prov.clone();
            let applied = (
                guided::applied_key(&e.id),
                StoredTarget::of(&target).to_row(),
            );
            self.blocking(move |s| {
                use lodestar_companions::manager::Rows as _;
                rows.put(&next_row).map_err(|_| ())?;
                s.lock()
                    .map_err(|_| ())?
                    .update_settings_text(
                        &[
                            (&prov_set, Provenance::Generated.as_str()),
                            (&applied.0, &applied.1),
                        ],
                        &[],
                    )
                    .map_err(|_| ())
            })
            .await?;
            let plan = self
                .plan_with(e, &target, temporary.clone(), Some(temporary.clone()))
                .await;
            let plan = match plan {
                Ok(p) if echoes(&p) => p,
                other => {
                    // Nothing applied yet: the temporary password goes back,
                    // so you can look again and confirm.
                    let restore = lodestar_companions::credentials::accept_pasted(
                        &self.secrets,
                        &key,
                        temporary,
                    );
                    let rows = crate::companions::StoreRows(self.store.clone());
                    let applied_key = guided::applied_key(&e.id);
                    let restored = self
                        .blocking(move |s| {
                            s.lock()
                                .map_err(|_| ())?
                                .update_settings_text(
                                    &[(&prov, Provenance::Pasted.as_str())],
                                    &[&applied_key],
                                )
                                .map_err(|_| ())?;
                            use lodestar_companions::manager::Rows as _;
                            rows.put(&restore.map_err(|_| ())?).map_err(|_| ())
                        })
                        .await;
                    if restored.is_err() {
                        failed("restore_failed");
                    }
                    return Err(match other {
                        Ok(_) => CompanionsError::Mismatch,
                        Err(e) => e,
                    }
                    .into());
                }
            };
            self.finish(e, &target, &plan, change.next.secret, Some(temporary))
                .await
        })
    }

    fn forget(
        &self,
        id: String,
    ) -> lodestar_api::ops::BoxFuture<'_, Result<CompanionForgotten, GuidedError>> {
        Box::pin(async move {
            let e = self.entry(&id)?;
            let _one = self.exclusive()?;
            self.unplan();
            let service = self.service.clone();
            let cid = e.id.clone();
            if let Some(code) = tokio::task::spawn_blocking(move || service.lodestar_owned(&cid))
                .await
                .map_err(|_| CompanionsError::Internal("panicked"))??
            {
                return Err(CompanionsError::Refused(code).into());
            }
            // A generated credential is, or may be, the password qBittorrent
            // uses now: the answer says so (#653's review, B1).
            let in_use = self.credential(e).await?.0 == Some(Provenance::Generated);
            let mut keys = guided::row_keys(&e.id);
            keys.extend(
                Field::ALL
                    .iter()
                    .map(|f| CredentialKey::new(e.id.clone(), *f).row_key()),
            );
            let gone = keys.clone();
            self.blocking(move |s| {
                let keys: Vec<&str> = gone.iter().map(String::as_str).collect();
                s.lock()
                    .map_err(|_| ())?
                    .update_settings_text(&[], &keys)
                    .map_err(|_| ())
            })
            .await?;
            if let Ok(mut n) = self.failures.lock() {
                *n = 0;
            }
            self.refresh().await;
            Ok(CompanionForgotten {
                credential_in_use: in_use,
                deleted: keys,
                id: e.id.as_str().to_owned(),
            })
        })
    }
}

#[cfg(test)]
#[path = "companion_wiring_guided_tests.rs"]
mod guided_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_companions::adapter::EnteredUrl;
    use lodestar_companions::inputs::PortNumber;
    use lodestar_companions::manager::Present;
    use lodestar_companions::{CompanionId, Version};

    fn id(s: &str) -> CompanionId {
        CompanionId::parse(s).unwrap()
    }

    fn client(id: &str) -> DownloadClient {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "Home", "kind": "qbittorrent", "url": "http://10.0.0.2:8080/"
        }))
        .unwrap()
    }

    fn installed(pairs: &[(&str, manager::Health)]) -> Snapshot {
        Snapshot {
            companions: pairs
                .iter()
                .map(|(i, h)| Present {
                    id: id(i),
                    version: Some(Version::parse("5.1.2").unwrap()),
                    health: *h,
                })
                .collect(),
            download_clients: false,
        }
    }

    /// Answers every check healthy, counting none: the gate test needs
    /// only the watch's report.
    struct AlwaysHealthy(CompanionId);

    impl lodestar_companions::adapter::Health for AlwaysHealthy {
        fn companion(&self) -> &CompanionId {
            &self.0
        }
        fn check<'a>(
            &'a self,
            _: &'a dyn GuardedHttp,
            _: &'a Target,
        ) -> BoxFuture<'a, lodestar_companions::adapter::HealthStatus> {
            Box::pin(async { lodestar_companions::adapter::HealthStatus::Healthy })
        }
    }

    struct NoHttp;

    impl GuardedHttp for NoHttp {
        fn send<'a>(
            &'a self,
            _: lodestar_companions::adapter::ProbeRequest<'a>,
        ) -> BoxFuture<
            'a,
            Result<
                lodestar_companions::adapter::ProbeResponse,
                lodestar_companions::adapter::AdapterError,
            >,
        > {
            Box::pin(async { Err(lodestar_companions::adapter::AdapterError::Refused("test")) })
        }
    }

    /// The gate leaves `Unknown` once the feed runs, and follows a
    /// companion you added, a client you set up and the records changing.
    /// Waits are bounded at 10 s.
    #[tokio::test]
    async fn the_gate_follows_what_is_there() {
        let deadline = Duration::from_secs(10);
        let (_inst_tx, inst) = channel::channel(Snapshot::default());
        let (int_tx, ints) = channel::channel(Integrations::default());
        let (gate_tx, mut gate) = channel::channel(requires::Snapshot::Unknown);
        let endpoints: Endpoints = Arc::new(|| Box::pin(async { Ok(None) }));
        let w = start_with(
            Vec::new(),
            inst,
            ints,
            endpoints,
            gate_tx,
            Arc::new(NoHttp),
            Arc::new(|id: &CompanionId| {
                Some(Box::new(AlwaysHealthy(id.clone()))
                    as Box<dyn lodestar_companions::adapter::Health>)
            }),
            // Long: every change below arrives through `changed()`, never
            // through a tick.
            Duration::from_secs(3600),
        )
        .unwrap();
        let known = |g: &requires::Snapshot, f: &dyn Fn(&requires::Present) -> bool| match g {
            requires::Snapshot::Known(p) => f(p),
            requires::Snapshot::Unknown => false,
        };
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| known(g, &|p| p.companions.is_empty())),
        )
        .await
        .expect("the gate stayed unknown")
        .unwrap();
        int_tx.send_modify(|i| i.clients.push(client("home")));
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| known(g, &|p| p.external_client)),
        )
        .await
        .expect("the gate didn't see your client")
        .unwrap();
        w.added.send_replace(vec![(
            id("prowlarr"),
            Target::Loopback(PortNumber::new(9696).unwrap()),
        )]);
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| {
                known(g, &|p| {
                    p.companions.get("prowlarr") == Some(&requires::Health::Healthy)
                })
            }),
        )
        .await
        .expect("the gate didn't see the companion you added")
        .unwrap();
        w.added.send_replace(Vec::new());
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| known(g, &|p| p.companions.is_empty())),
        )
        .await
        .expect("the gate kept a companion you removed")
        .unwrap();
    }

    /// Runner's condition (2026-11-09): with companions off, the gate is
    /// off and no source waits on it; on, it starts `Unknown`.
    #[test]
    fn the_gate_is_off_when_companions_are() {
        let (_tx, rx) = gate::<()>(None);
        assert!(rx.is_none());
        let (_tx, rx) = gate(Some(&()));
        assert_eq!(*rx.unwrap().borrow(), requires::Snapshot::Unknown);
    }

    /// The address comes from the saved config: the manifest lists none.
    #[test]
    fn the_prowlarr_address_is_its_saved_url_and_agents_are_kept() {
        assert_eq!(
            config_url(r#"{"url":"http://indexer.lan:9696","manager":"prowlarr"}"#).as_deref(),
            Some("http://indexer.lan:9696")
        );
        assert_eq!(config_url("not json"), None);
        assert_eq!(config_url(r#"{"manager":"jackett"}"#), None);
        let c = prowlarr_config(
            Some(r#"{"url":"http://indexer.lan:9696","manager":"jackett","agents":3}"#),
            "http://127.0.0.1:9696",
        );
        assert_eq!(
            c,
            serde_json::json!({"url":"http://127.0.0.1:9696","manager":"prowlarr","agents":3})
        );
        assert_eq!(
            prowlarr_config(None, "http://h:1"),
            serde_json::json!({"url":"http://h:1","manager":"prowlarr"})
        );
    }

    /// From a real saved config in the store, through the same read the
    /// gate uses.
    #[test]
    fn the_saved_prowlarr_config_is_read_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db"), Default::default()).unwrap();
        let store = Mutex::new(store);
        assert_eq!(saved_config(&store), Ok(None));
        store
            .lock()
            .unwrap()
            .set_settings_text(&[(
                // `SourceSetting::Config`'s key for the plugin.
                "sources.config.prowlarr",
                r#"{"url":"http://indexer.lan:9696","manager":"prowlarr"}"#,
            )])
            .unwrap();
        let url = saved_config(&store)
            .unwrap()
            .and_then(|c| config_url(&c))
            .unwrap();
        let ours: Vec<Added> = vec![(
            id("prowlarr"),
            Target::Entered(EnteredUrl::parse("http://indexer.lan:9696/").unwrap()),
        )];
        let none = Snapshot::default();
        assert!(
            !present(
                &none,
                &Watched::new(),
                &[],
                Some(std::slice::from_ref(&url)),
                &ours
            )
            .external_indexer
        );
        assert!(
            present(
                &none,
                &Watched::new(),
                &[],
                Some(std::slice::from_ref(&url)),
                &[]
            )
            .external_indexer
        );
    }

    /// The sink through a real registry (self-review I5): your own client
    /// keeps its saved password, and the managed entry is added, then
    /// replaced rather than added twice.
    #[test]
    fn the_sink_adds_the_managed_client_and_keeps_yours() {
        let dir = tempfile::tempdir().unwrap();
        // Everything in the temporary folder: no config is read from the
        // working directory, and the sealing key is created here, never in
        // the source tree (#645's security review).
        let data = dir.path().to_string_lossy().into_owned();
        let mut config =
            crate::config::Config::load(None, |k| (k == "LODESTAR_DATA_DIR").then(|| data.clone()))
                .unwrap();
        config.data_dir = dir.path().to_path_buf();
        let store = Arc::new(Mutex::new(
            Store::open(dir.path().join("db"), Default::default()).unwrap(),
        ));
        let registry = Arc::new(crate::settings::Registry::load(&config, store.clone()).unwrap());
        let mut home = serde_json::to_value(client("home")).unwrap();
        home["password"] = "hunter2hunter2".into();
        registry
            .save(
                serde_json::from_value(serde_json::json!({"integrations": {"clients": [home]}}))
                    .unwrap(),
            )
            .unwrap();
        drop(store);
        for password in ["first-generated", "second-generated"] {
            let managed = ManagedClient {
                target: Target::Loopback(PortNumber::new(8080).unwrap()),
                username: "lodestar".into(),
                password: Secret::new(String::from(password)),
                category: String::new(),
            };
            save_client(&registry, &managed).unwrap();
        }
        let now = registry.subscribe::<Integrations>().borrow().clone();
        assert_eq!(now.clients.len(), 2);
        let mine = now.client("home").unwrap();
        assert_eq!(
            mine.password.as_ref().map(|p| p.0.as_str()),
            Some("hunter2hunter2")
        );
        let managed = now.client(MANAGED_CLIENT).unwrap();
        assert_eq!(managed.url, "http://127.0.0.1:8080/");
        assert_eq!(
            managed.password.as_ref().map(|p| p.0.as_str()),
            Some("second-generated")
        );
    }

    /// X-cmp-native-handover: a removal takes the managed client out of
    /// `integrations.clients` through a real registry, and only that one:
    /// yours stays, with its saved password. With none there, nothing is
    /// saved.
    #[test]
    fn forgetting_the_managed_client_keeps_yours() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_string_lossy().into_owned();
        let mut config =
            crate::config::Config::load(None, |k| (k == "LODESTAR_DATA_DIR").then(|| data.clone()))
                .unwrap();
        config.data_dir = dir.path().to_path_buf();
        let store = Arc::new(Mutex::new(
            Store::open(dir.path().join("db"), Default::default()).unwrap(),
        ));
        let registry = Arc::new(crate::settings::Registry::load(&config, store.clone()).unwrap());
        let mut home = serde_json::to_value(client("home")).unwrap();
        home["password"] = "hunter2hunter2".into();
        registry
            .save(
                serde_json::from_value(serde_json::json!({"integrations": {"clients": [home]}}))
                    .unwrap(),
            )
            .unwrap();
        // None managed yet: nothing changes.
        forget_client(&registry).unwrap();
        assert_eq!(
            registry.subscribe::<Integrations>().borrow().clients.len(),
            1
        );
        let managed = ManagedClient {
            target: Target::Loopback(PortNumber::new(8080).unwrap()),
            username: "admin".into(),
            password: Secret::new(String::from("generated-by-lodestar")),
            category: String::new(),
        };
        save_client(&registry, &managed).unwrap();
        assert_eq!(
            registry.subscribe::<Integrations>().borrow().clients.len(),
            2
        );
        drop(store);
        forget_client(&registry).unwrap();
        let now = registry.subscribe::<Integrations>().borrow().clone();
        assert_eq!(now.clients.len(), 1);
        assert!(now.client(MANAGED_CLIENT).is_none());
        let mine = now.client("home").unwrap();
        assert_eq!(
            mine.password.as_ref().map(|p| p.0.as_str()),
            Some("hunter2hunter2")
        );
    }

    /// A save made between the list's read and the hand-over's own save
    /// isn't overwritten: the hand-over's is refused as stale and made
    /// again from the list as it is then, keeping both (#674's self-review
    /// I1). A name or category you gave the managed entry is kept.
    #[test]
    fn a_save_in_between_is_kept_and_your_names_survive() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_string_lossy().into_owned();
        let mut config =
            crate::config::Config::load(None, |k| (k == "LODESTAR_DATA_DIR").then(|| data.clone()))
                .unwrap();
        config.data_dir = dir.path().to_path_buf();
        let store = Arc::new(Mutex::new(
            Store::open(dir.path().join("db"), Default::default()).unwrap(),
        ));
        let registry = Arc::new(crate::settings::Registry::load(&config, store.clone()).unwrap());
        drop(store);
        let managed = ManagedClient {
            target: Target::Loopback(PortNumber::new(8080).unwrap()),
            username: "admin".into(),
            password: Secret::new(String::from("generated-by-lodestar")),
            category: String::new(),
        };
        save_client(&registry, &managed).unwrap();
        // You rename it and give it a category.
        let mut mine = serde_json::to_value(
            registry
                .subscribe::<Integrations>()
                .borrow()
                .client(MANAGED_CLIENT)
                .unwrap()
                .clone(),
        )
        .unwrap();
        mine["name"] = "My qBittorrent".into();
        mine["category"] = "tv".into();
        mine.as_object_mut().unwrap().remove("password");
        registry
            .save(
                serde_json::from_value(serde_json::json!({"integrations": {"clients": [mine]}}))
                    .unwrap(),
            )
            .unwrap();
        // A hand-over whose first try loses to a save of yours.
        let tries = std::sync::atomic::AtomicUsize::new(0);
        save_clients(&registry, |current| {
            if tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                let mut home = serde_json::to_value(client("home")).unwrap();
                home["password"] = "hunter2hunter2".into();
                let mut both = other_clients(current).unwrap();
                both.push(serde_json::to_value(current.client(MANAGED_CLIENT).unwrap()).unwrap());
                both.last_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("password");
                both.push(home);
                registry
                    .save(
                        serde_json::from_value(
                            serde_json::json!({"integrations": {"clients": both}}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
            }
            with_managed(current, &managed).map(Some)
        })
        .unwrap();
        assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 2);
        let now = registry.subscribe::<Integrations>().borrow().clone();
        assert!(now.client("home").is_some(), "the save in between was lost");
        let kept = now.client(MANAGED_CLIENT).unwrap();
        assert_eq!(
            (kept.name.as_str(), kept.category.as_str()),
            ("My qBittorrent", "tv")
        );
        assert_eq!(
            kept.password.as_ref().map(|p| p.0.as_str()),
            Some("generated-by-lodestar")
        );
    }

    /// The source on with an address, off, or with none saved; a read that
    /// fails is an error, never "not external".
    #[tokio::test]
    async fn the_prowlarr_address_is_read_only_when_the_source_is_on() {
        let saved = || {
            Ok(Some(
                r#"{"url":"http://indexer.lan:9696","manager":"prowlarr"}"#.to_owned(),
            ))
        };
        assert_eq!(
            prowlarr_endpoints(Ok(true), saved).await,
            Ok(Some(vec!["http://indexer.lan:9696".to_owned()]))
        );
        assert_eq!(prowlarr_endpoints(Ok(false), saved).await, Ok(None));
        assert_eq!(prowlarr_endpoints(Ok(true), || Ok(None)).await, Ok(None));
        assert_eq!(prowlarr_endpoints(Err(()), saved).await, Err(()));
        assert_eq!(prowlarr_endpoints(Ok(true), || Err(())).await, Err(()));
    }

    /// Runner's condition (2026-11-09): with companions on and nothing
    /// installed, your own Prowlarr or download client meets the need as
    /// soon as the first snapshot arrives, and a read that then fails
    /// keeps it.
    #[tokio::test]
    async fn your_own_setup_keeps_working_with_companions_on() {
        let deadline = Duration::from_secs(10);
        let catalogue = lodestar_companions::builtin().unwrap();
        let reads = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let r = reads.clone();
        let endpoints: Endpoints = Arc::new(move || {
            let n = r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(Some(vec!["http://indexer.lan:9696".to_owned()]))
                } else {
                    Err(())
                }
            })
        });
        let (_inst_tx, inst) = channel::channel(Snapshot::default());
        let mut ints = Integrations::default();
        ints.clients.push(client("home"));
        let (int_tx, ints) = channel::channel(ints);
        let (gate_tx, mut gate) = channel::channel(requires::Snapshot::Unknown);
        let _w = start_with(
            Vec::new(),
            inst,
            ints,
            endpoints,
            gate_tx,
            Arc::new(NoHttp),
            Arc::new(|_: &CompanionId| None),
            Duration::from_secs(3600),
        )
        .unwrap();
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| matches!(g, requires::Snapshot::Known(_))),
        )
        .await
        .expect("no first snapshot")
        .unwrap();
        let state = |plugin: &str, g: &requires::Snapshot| {
            requires::evaluate(
                &lodestar_companions::requires::requirement(plugin).unwrap(),
                Some(catalogue),
                g,
            )
        };
        assert_eq!(state("prowlarr", &gate.borrow()), requires::State::Ok);
        assert_eq!(
            state("download_clients", &gate.borrow()),
            requires::State::Ok
        );
        // A later change sends again with a failing read: the address read
        // first is kept.
        int_tx.send_modify(|i| i.clients.push(client("office")));
        tokio::time::timeout(deadline, async {
            while reads.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the gate didn't read again");
        assert_eq!(state("prowlarr", &gate.borrow()), requires::State::Ok);
    }

    /// Records each write; fails the step named in `fail`.
    #[derive(Default)]
    struct Writes {
        log: Mutex<Vec<String>>,
        fail: Vec<&'static str>,
    }

    impl Writes {
        fn failing(steps: &[&'static str]) -> Writes {
            Writes {
                fail: steps.to_vec(),
                ..Writes::default()
            }
        }
        fn step(&self, s: String, name: &'static str) -> bool {
            self.log.lock().unwrap().push(s);
            !self.fail.contains(&name)
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl SourceWrites for Writes {
        fn source<'a>(&'a self) -> BoxFuture<'a, Option<u8>> {
            Box::pin(async { Some(7) })
        }
        fn config<'a>(&'a self, _: u8, c: serde_json::Value) -> BoxFuture<'a, bool> {
            let ok = self.step(format!("config {}", c["url"]), "config");
            Box::pin(async move { ok })
        }
        fn key<'a>(&'a self, _: u8, key: Option<Secret>) -> BoxFuture<'a, bool> {
            let (s, name) = match key {
                Some(_) => ("key set".to_owned(), "key set"),
                None => ("key removed".to_owned(), "key removed"),
            };
            let ok = self.step(s, name);
            Box::pin(async move { ok })
        }
    }

    const MINE: &str = r#"{"url":"http://indexer.lan:9696","manager":"jackett","agents":3}"#;

    fn key() -> Secret {
        Secret::new(String::from("0123456789abcdef0123456789abcdef"))
    }

    /// The companion's key replaces yours, then the address.
    #[tokio::test]
    async fn the_prowlarr_hand_over_replaces_the_key_then_the_address() {
        let w = Writes::default();
        hand_over_prowlarr(&w, Some(MINE), "http://127.0.0.1:9696", key())
            .await
            .unwrap();
        assert_eq!(w.log(), ["key set", "config \"http://127.0.0.1:9696\""]);
    }

    /// The key not stored: nothing else is written, and yours stays.
    #[tokio::test]
    async fn a_key_that_cant_be_stored_stops_the_hand_over() {
        let w = Writes::failing(&["key set"]);
        let got = hand_over_prowlarr(&w, Some(MINE), "http://127.0.0.1:9696", key()).await;
        assert_eq!(got, Err(SinkError::Rejected));
        assert_eq!(w.log(), ["key set"]);
    }

    /// The address not written: the companion's key is removed (the
    /// source then turns off).
    #[tokio::test]
    async fn a_failed_address_removes_the_companions_key() {
        let w = Writes::failing(&["config"]);
        let got = hand_over_prowlarr(&w, None, "http://127.0.0.1:9696", key()).await;
        assert_eq!(got, Err(SinkError::Rejected));
        assert_eq!(
            w.log(),
            ["key set", "config \"http://127.0.0.1:9696\"", "key removed"]
        );
    }

    /// The address not written and the key not removable: logged, by name.
    #[tokio::test]
    async fn a_failed_rollback_is_logged() {
        let before = lodestar_companions::log_events::CONFIG_FAILED::EVENT.count();
        let w = Writes::failing(&["config", "key removed"]);
        let got = hand_over_prowlarr(&w, Some(MINE), "http://127.0.0.1:9696", key()).await;
        assert_eq!(got, Err(SinkError::Rejected));
        // The path itself: key, address, the removal tried.
        assert_eq!(
            w.log(),
            ["key set", "config \"http://127.0.0.1:9696\"", "key removed"]
        );
        // A process-wide count: other tests can only raise it, so this
        // proves the line was logged, and the order above proves where.
        assert!(lodestar_companions::log_events::CONFIG_FAILED::EVENT.count() > before);
    }

    /// The source writes over a real `Control`, as the ops make them.
    struct ControlWrites {
        control: lodestar_pipeline::Control,
        creds: lodestar_sources::consent::SourceCredentials,
        id: lodestar_store::SourceId,
        /// The stand-in's settings can be saved while this is set.
        editable: Arc<std::sync::atomic::AtomicBool>,
    }

    impl SourceWrites for ControlWrites {
        fn source<'a>(&'a self) -> BoxFuture<'a, Option<u8>> {
            Box::pin(async move { Some(self.id.get()) })
        }
        fn config<'a>(&'a self, _: u8, c: serde_json::Value) -> BoxFuture<'a, bool> {
            Box::pin(async move {
                let change = lodestar_pipeline::control::SourceChange {
                    config: Some(c.to_string()),
                    ..Default::default()
                };
                self.control.update_source(self.id, change).await.is_ok()
            })
        }
        fn key<'a>(&'a self, _: u8, key: Option<Secret>) -> BoxFuture<'a, bool> {
            Box::pin(async move {
                self.control
                    .update_credentials(self.id, &self.creds, vec![("api_key".into(), key)])
                    .await
                    .is_ok()
            })
        }
    }

    /// The Prowlarr source as the hand-over needs it: an Account source
    /// whose settings can be saved. (This build's Prowlarr plugin keeps its
    /// settings fixed, so the decisions file names the gap.)
    struct StandIn(
        lodestar_sources::Manifest,
        Arc<std::sync::atomic::AtomicBool>,
    );

    const MANIFEST: lodestar_sources::Manifest = lodestar_sources::Manifest {
        id: PROWLARR_SOURCE,
        name: "Prowlarr",
        version: "1",
        kinds: &[lodestar_sources::Kind::Discovery],
        networks: &[lodestar_sources::Network::Bt],
        endpoints: &[],
        risk: lodestar_sources::Risk::Medium,
        consent: lodestar_sources::Consent::Account,
        default_priority: lodestar_sources::Priority::Low,
        enabled_by_default: false,
        always_adult: false,
        finds_private: false,
    };

    impl lodestar_sources::SourcePlugin for StandIn {
        fn manifest(&self) -> &lodestar_sources::Manifest {
            &self.0
        }
        fn config_editable(&self) -> bool {
            self.1.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn config_json(&self) -> Option<String> {
            Some("{}".into())
        }
        fn check_config(&self, json: &str) -> Result<String, lodestar_sources::ConfigRefusal> {
            if self.config_editable() {
                Ok(json.to_owned())
            } else {
                Err(lodestar_sources::ConfigRefusal("refused by the test"))
            }
        }
        fn run(
            &self,
            mut ctx: lodestar_sources::SourceCtx,
        ) -> lodestar_sources::BoxFuture<'_, Result<(), lodestar_sources::PluginError>> {
            Box::pin(async move {
                ctx.shutdown.requested().await;
                Ok(())
            })
        }
    }

    /// A running Prowlarr source with your own address and key, on a real
    /// pipeline and `Control`.
    async fn enabled_prowlarr(
        dir: &std::path::Path,
    ) -> (lodestar_pipeline::Pipeline, ControlWrites) {
        use lodestar_store::StoreConfig;
        let db = dir.join("t.db");
        let editable = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let plugin: Arc<dyn lodestar_sources::SourcePlugin> =
            Arc::new(StandIn(MANIFEST, editable.clone()));
        let pipeline = lodestar_pipeline::Pipeline::start(
            lodestar_pipeline::PipelineConfig::default(),
            Store::open(&db, StoreConfig::default()).unwrap(),
            vec![plugin],
        )
        .await
        .unwrap();
        let status = pipeline
            .sources()
            .into_iter()
            .find(|s| s.plugin == PROWLARR_SOURCE)
            .unwrap();
        assert!(status.config_editable);
        let id = status.id;
        let store = Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        ));
        let control = lodestar_pipeline::Control::start(
            pipeline.monitor(),
            None,
            store.clone(),
            false,
            lodestar_dht::node::Intensity::Balanced,
        )
        .await
        .unwrap();
        let creds = lodestar_sources::consent::SourceCredentials::with_keys(
            store.clone(),
            Arc::new(lodestar_sources::consent::SecretStore::from_key(&[5; 32])),
        );
        let w = ControlWrites {
            control,
            creds,
            id,
            editable,
        };
        // Your own setup, then on (seeded as a build before the consent
        // gate did, since the API can't enable an Account source now).
        let change = lodestar_pipeline::control::SourceChange {
            config: Some(
                r#"{"url":"http://192.168.1.5:9696","manager":"jackett","agents":3}"#.into(),
            ),
            ..Default::default()
        };
        if let Err(e) = w.control.update_source(id, change).await {
            panic!("seeding your own config: {e:?}");
        }
        assert!(
            w.key(
                0,
                Some(Secret::new(String::from(
                    "your-own-key-0000000000000000000"
                )))
            )
            .await
        );
        store.lock().unwrap().enable_source(id, None).unwrap();
        (pipeline, w)
    }

    fn state(w: &ControlWrites) -> lodestar_store::SourceState {
        w.control
            .sources()
            .unwrap()
            .into_iter()
            .find(|v| v.status.id == w.id)
            .unwrap()
            .state
    }

    /// #645's delta review (B1): a successful hand-over leaves a running
    /// Prowlarr source on, with the companion's key and address.
    #[tokio::test]
    async fn a_hand_over_keeps_your_running_source_on() {
        let dir = tempfile::tempdir().unwrap();
        let (_p, w) = enabled_prowlarr(dir.path()).await;
        assert_eq!(state(&w), lodestar_store::SourceState::Enabled);
        hand_over_prowlarr(&w, Some(MINE), "http://127.0.0.1:9696", key())
            .await
            .unwrap();
        assert_eq!(state(&w), lodestar_store::SourceState::Enabled);
        assert_eq!(
            w.creds
                .get(PROWLARR_SOURCE, "api_key")
                .unwrap()
                .unwrap()
                .expose(),
            key().expose()
        );
    }

    /// A failed address write ends with the source off and no key: it
    /// fails closed.
    #[tokio::test]
    async fn a_failed_hand_over_turns_the_source_off() {
        let dir = tempfile::tempdir().unwrap();
        let (_p, w) = enabled_prowlarr(dir.path()).await;
        // A real refusal from `Control`: the source's settings can't be
        // saved now.
        w.editable.store(false, std::sync::atomic::Ordering::SeqCst);
        let got = hand_over_prowlarr(&w, Some(MINE), "http://127.0.0.1:9696", key()).await;
        assert_eq!(got, Err(SinkError::Rejected));
        assert_eq!(state(&w), lodestar_store::SourceState::Disabled);
        assert!(w.creds.get(PROWLARR_SOURCE, "api_key").unwrap().is_none());
    }

    #[test]
    fn target_urls_are_canonical() {
        let p = PortNumber::new(9696).unwrap();
        assert_eq!(target_url(&Target::Loopback(p)), "http://127.0.0.1:9696");
        let u = EnteredUrl::parse("HTTP://Prowlarr.LAN:9696/base/").unwrap();
        assert_eq!(
            target_url(&Target::Entered(u)),
            "http://prowlarr.lan:9696/base"
        );
        assert_eq!(canonical("http://h:1/"), canonical("HTTP://H:1"));
        // Only a parsed comparison passes these: default ports, case.
        assert_eq!(canonical("http://h"), canonical("http://h:80"));
        assert_eq!(canonical("https://h"), canonical("https://h:443"));
        assert_ne!(canonical("http://h:80"), canonical("http://h:8080"));
        assert_eq!(canonical("https://h/x/"), canonical("HTTPS://H:443/x"));
        assert_ne!(canonical("http://h"), canonical("https://h"));
        assert_eq!(canonical(" not a url/ "), "not a url");
    }

    #[test]
    fn installed_companions_count_only_once_checked() {
        let p = present(
            &installed(&[
                ("qbittorrent", manager::Health::Unknown),
                ("prowlarr", manager::Health::Healthy),
            ]),
            &Watched::new(),
            &[],
            None,
            &[],
        );
        assert_eq!(p.companions["qbittorrent"], requires::Health::Unhealthy);
        assert_eq!(p.companions["prowlarr"], requires::Health::Healthy);
        assert!(!p.external_client && !p.external_indexer);
    }

    #[test]
    fn watched_companions_follow_the_watch_and_missing_ones_leave() {
        let q = Target::Loopback(PortNumber::new(8080).unwrap());
        let pr = Target::Loopback(PortNumber::new(9696).unwrap());
        let added: Vec<Added> = vec![(id("qbittorrent"), q.clone()), (id("prowlarr"), pr.clone())];
        let mut w = Watched::new();
        w.insert(id("qbittorrent"), (q.clone(), Seen::Healthy));
        w.insert(id("prowlarr"), (pr.clone(), Seen::Missing));
        let p = present(&Snapshot::default(), &w, &[], None, &added);
        assert_eq!(p.companions.len(), 1);
        assert_eq!(p.companions["qbittorrent"], requires::Health::Healthy);
        // Removed, or moved to another target: the old result doesn't count.
        let p = present(&Snapshot::default(), &w, &[], None, &added[1..]);
        assert!(p.companions.is_empty());
        let moved: Vec<Added> = vec![(
            id("qbittorrent"),
            Target::Entered(EnteredUrl::parse("http://qbt.lan:8080").unwrap()),
        )];
        assert!(
            present(&Snapshot::default(), &w, &[], None, &moved)
                .companions
                .is_empty()
        );
        // Lodestar's own install is judged by its own health, not the watch.
        w.insert(id("prowlarr"), (pr, Seen::Healthy));
        let p = present(
            &installed(&[("prowlarr", manager::Health::Unhealthy)]),
            &w,
            &[],
            None,
            &added,
        );
        assert_eq!(p.companions["prowlarr"], requires::Health::Unhealthy);
    }

    #[test]
    fn external_clients_and_indexers_are_the_ones_you_set_up() {
        let none = Snapshot::default();
        let w = Watched::new();
        // The managed entry with no qBittorrent of Lodestar's is a client
        // like yours (forgotten, say); with one, it's that companion's.
        assert!(present(&none, &w, &[client(MANAGED_CLIENT)], None, &[]).external_client);
        let ours = installed(&[("qbittorrent", manager::Health::Healthy)]);
        assert!(!present(&ours, &w, &[client(MANAGED_CLIENT)], None, &[]).external_client);
        let added = [(
            id("qbittorrent"),
            Target::Loopback(PortNumber::new(8080).unwrap()),
        )];
        assert!(!present(&none, &w, &[client(MANAGED_CLIENT)], None, &added).external_client);
        assert!(
            present(
                &none,
                &w,
                &[client(MANAGED_CLIENT), client("home")],
                None,
                &[]
            )
            .external_client
        );
        let record: Added = (
            id("prowlarr"),
            Target::Entered(EnteredUrl::parse("http://prowlarr.lan:9696").unwrap()),
        );
        let ours = ["http://Prowlarr.lan:9696/".to_owned()];
        let theirs = ["http://indexer.example:9696".to_owned()];
        assert!(
            !present(&none, &w, &[], Some(&ours), std::slice::from_ref(&record)).external_indexer
        );
        assert!(
            present(&none, &w, &[], Some(&theirs), std::slice::from_ref(&record)).external_indexer
        );
        assert!(present(&none, &w, &[], Some(&ours), &[]).external_indexer);
        // Prowlarr installed by Lodestar is at loopback 9696.
        let loopback = ["http://127.0.0.1:9696".to_owned()];
        let inst = installed(&[("prowlarr", manager::Health::Healthy)]);
        assert!(!present(&inst, &w, &[], Some(&loopback), &[]).external_indexer);
        // The source off: nothing external.
        assert!(!present(&none, &w, &[], None, &[]).external_indexer);
    }

    #[test]
    fn the_client_batch_replaces_only_the_managed_entry() {
        let mut current = Integrations::default();
        let mut home = client("home");
        home.password = Some(crate::settings::SecretText("hunter2hunter2".into()));
        current.clients = vec![home, client(MANAGED_CLIENT)];
        let entry = ManagedClient {
            target: Target::Loopback(PortNumber::new(8080).unwrap()),
            username: "lodestar".into(),
            password: Secret::new(String::from("generated-password")),
            category: "lodestar".into(),
        };
        let batch = clients_batch(4, with_managed(&current, &entry).unwrap());
        assert_eq!(batch.0["integrations"].0["if_revision"], 4);
        let clients = batch.0["integrations"].0["clients"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(clients.len(), 2);
        // Your client keeps its saved password: none is sent.
        assert_eq!(clients[0]["id"], "home");
        assert!(clients[0].get("password").is_none());
        assert_eq!(clients[1]["id"], MANAGED_CLIENT);
        assert_eq!(clients[1]["url"], "http://127.0.0.1:8080/");
        assert_eq!(clients[1]["password"], "generated-password");
        // The batch's Debug names keys, never the password.
        assert!(!format!("{batch:?}").contains("generated-password"));
    }
}
