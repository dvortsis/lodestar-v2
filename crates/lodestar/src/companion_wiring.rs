//! The companion wiring in the binary (CMP-5g-1; plan
//! 2026-11-06-v06-companions CMP-5g, CMP-10's contract in
//! `docs/decisions/2026-11-08-CMP-5.md`):
//!
//! - [`present`] and [`start`]: what the pipeline's companion gate
//!   (`lodestar_pipeline::requires`) reads. Every companion Lodestar
//!   installed, with its health; every one you added and confirmed
//!   (CMP-5g-2's records, sent through [`Wired::added`]), watched every 30 s
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
///   [`MANAGED_CLIENT`];
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
    requires::Present {
        companions,
        external_client: clients.iter().any(|c| c.id != MANAGED_CLIENT),
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
    /// The companions you added, for CMP-5g-2's endpoints to send after a
    /// change to their records.
    #[allow(
        dead_code,
        reason = "CMP-5g-2's endpoints send the records after a change"
    )]
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

/// At serve: [`start`]s with no companions added (CMP-5g-2 reads its
/// records and sends them). A failure is logged by
/// code and leaves the gate `Unknown`: gated plugins wait (fail closed).
pub async fn serve(
    companions: &crate::companions::Companions,
    settings: &crate::settings::Registry,
    ops: Arc<dyn OpsApi>,
    store: Arc<Mutex<Store>>,
    gate: channel::Sender<requires::Snapshot>,
) -> Option<Wired> {
    match start(
        Vec::new(),
        companions.snapshot.clone(),
        settings.subscribe(),
        ops,
        store,
        gate,
    ) {
        Ok(w) => Some(w),
        Err(code) => {
            lodestar_log::log_event!(
                lodestar_companions::log_events::INSTALL_FAILED,
                companion = "all",
                version = "",
                step = "wire",
                reason = code
            );
            None
        }
    }
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
#[allow(
    dead_code,
    reason = "CMP-5g-2's confirmation hands configured companions over through it"
)]
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
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
struct ManagedClient {
    target: Target,
    username: String,
    password: Secret,
    category: String,
}

/// The `integrations` batch that makes `entry` the managed client: the
/// list as it is, with [`MANAGED_CLIENT`] replaced or added. Other clients
/// go without their passwords, which keeps the saved ones (the registry
/// keeps a saved secret for an item of the same id, kind and address).
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
fn client_batch(current: &Integrations, entry: &ManagedClient) -> Result<SettingsBatch, SinkError> {
    let mut clients = Vec::new();
    for c in current.clients.iter().filter(|c| c.id != MANAGED_CLIENT) {
        let mut c = c.clone();
        c.password = None;
        clients.push(serde_json::to_value(c).map_err(|_| SinkError::Rejected)?);
    }
    clients.push(serde_json::json!({
        "id": MANAGED_CLIENT,
        "name": "qBittorrent (Lodestar)",
        "kind": "qbittorrent",
        "url": format!("{}/", target_url(&entry.target)),
        "username": entry.username,
        "password": entry.password.expose(),
        "category": entry.category,
    }));
    let mut patch = serde_json::Map::new();
    patch.insert("clients".into(), serde_json::Value::Array(clients));
    Ok(SettingsBatch(
        std::iter::once(("integrations".to_owned(), SettingsPatch(patch))).collect(),
    ))
}

/// Saves `managed` as the managed client, reading the list right before
/// the save so a save you made meanwhile isn't overwritten with an old
/// one. Blocking.
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
fn save_client(
    settings: &crate::settings::Registry,
    managed: &ManagedClient,
) -> Result<(), SinkError> {
    let current = settings.subscribe::<Integrations>().borrow().clone();
    let batch = client_batch(&current, managed)?;
    settings
        .save(batch)
        .map(|_| ())
        .map_err(|_| SinkError::Rejected)
}

/// The Prowlarr source's new config: the companion's address, keeping any
/// other setting you saved (its `agents`).
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
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
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
trait SourceWrites: Send + Sync {
    /// The Prowlarr source's id; `None` when it isn't registered.
    fn source<'a>(&'a self) -> BoxFuture<'a, Option<u8>>;
    fn config<'a>(&'a self, id: u8, config: serde_json::Value) -> BoxFuture<'a, bool>;
    /// Stores (`Some`) or removes (`None`) the source's `api_key`.
    fn key<'a>(&'a self, id: u8, key: Option<Secret>) -> BoxFuture<'a, bool>;
}

#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
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
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
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
        assert!(!present(&none, &w, &[client(MANAGED_CLIENT)], None, &[]).external_client);
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
        let batch = client_batch(&current, &entry).unwrap();
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
