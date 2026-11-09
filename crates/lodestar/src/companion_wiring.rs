//! The companion wiring in the binary (CMP-5g-1; plan
//! 2026-11-06-v06-companions CMP-5g, CMP-10's contract in
//! `docs/decisions/2026-11-08-CMP-5.md`):
//!
//! - [`present`] and [`start`]: what the pipeline's companion gate
//!   (`lodestar_pipeline::requires`) reads. Every companion Lodestar
//!   installed, with its health; every one you added and confirmed
//!   (`lodestar_companions::guided`), watched every 30 s
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
use lodestar_companions::adapter::{BoxFuture, GuardedHttp, Target};
use lodestar_companions::configure::{
    ConfigSink, DownloadClientEntry, IndexerManagerEntry, SinkError,
};
use lodestar_companions::detect::{self, CompanionHttp, Runtime};
use lodestar_companions::guided::Record;
use lodestar_companions::manager::{self, Host, Snapshot};
use lodestar_companions::watch::{self, Seen, Watched};
use lodestar_companions::{Catalogue, CompanionId};
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

/// Compared without a trailing `/`, the scheme and host in lower case.
fn canonical(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            format!(
                "{}://{}{path}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            )
        }
        None => url.to_owned(),
    }
}

/// The companions you added and confirmed, from their `settings` rows.
/// A row that doesn't read is no record (`guided`'s rule). Blocking.
pub fn records(store: &Mutex<Store>, catalogue: &Catalogue, runtime: Runtime) -> Vec<Record> {
    let rows = match store.lock() {
        Ok(s) => s
            .settings_with_prefix_raw(lodestar_companions::credentials::ROW_PREFIX)
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let text = |key: &str| {
        rows.iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_ref().ok().cloned())
    };
    catalogue
        .entries()
        .iter()
        .filter_map(|e| Record::read(e, runtime, text))
        .collect()
}

/// Detection's runtime here (Q12: container names only under Docker's
/// DNS).
pub fn runtime(host: Host) -> Runtime {
    if host.in_container {
        Runtime::Container
    } else {
        Runtime::Native
    }
}

/// What the gate reads, from what's known:
/// - each companion Lodestar installed, healthy only when its last check
///   passed (not yet checked counts as unhealthy: it fails closed);
/// - each one you added, as the watch last saw it; a missing one isn't
///   listed, so its dependents need it;
/// - `external_client`: an `integrations.clients` entry that isn't
///   [`MANAGED_CLIENT`];
/// - `external_indexer`: the Prowlarr source is on and one of the addresses
///   it runs with isn't a companion's.
pub fn present(
    installed: &Snapshot,
    watched: &Watched,
    clients: &[DownloadClient],
    prowlarr: Option<&[String]>,
    records: &[Record],
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
    for (id, seen) in watched {
        let h = match seen {
            Seen::Healthy => requires::Health::Healthy,
            Seen::Unhealthy => requires::Health::Unhealthy,
            Seen::Missing => continue,
        };
        // A companion Lodestar installed is judged by its own health.
        companions.entry(id.as_str().to_owned()).or_insert(h);
    }
    let ours: Vec<String> = records
        .iter()
        .filter(|r| r.id.as_str() == PROWLARR_SOURCE)
        .map(|r| target_url(&r.target))
        .chain(
            installed
                .companions
                .iter()
                .filter(|c| c.id.as_str() == PROWLARR_SOURCE && c.version.is_some())
                .map(|_| canonical("http://127.0.0.1:9696")),
        )
        .collect();
    requires::Present {
        companions,
        external_client: clients.iter().any(|c| c.id != MANAGED_CLIENT),
        external_indexer: prowlarr
            .is_some_and(|endpoints| endpoints.iter().any(|e| !ours.contains(&canonical(e)))),
    }
}

/// The Prowlarr source's addresses, when it's on. `None` when it's off or
/// unknown.
async fn prowlarr_endpoints(ops: &dyn OpsApi) -> Option<Vec<String>> {
    let sources = ops.sources().await.ok()?;
    let s = sources
        .into_iter()
        .find(|s| s.plugin == PROWLARR_SOURCE && s.enabled)?;
    Some(s.endpoints)
}

/// The Prowlarr source's addresses when it's on ([`prowlarr_endpoints`];
/// a fake in tests).
type Endpoints = Arc<dyn Fn() -> BoxFuture<'static, Option<Vec<String>>> + Send + Sync>;

/// What [`start`] runs, stopped when dropped.
#[derive(Debug)]
pub struct Wired {
    /// The companions you added, for CMP-5g-2's endpoints to send after a
    /// change to their rows.
    #[allow(
        dead_code,
        reason = "CMP-5g-2's endpoints send the records after a change"
    )]
    pub records: channel::Sender<Vec<Record>>,
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
    records: Vec<Record>,
    installed: channel::Receiver<Snapshot>,
    integrations: channel::Receiver<Integrations>,
    ops: Arc<dyn OpsApi>,
    gate: channel::Sender<requires::Snapshot>,
) -> Result<Wired, &'static str> {
    let http: Arc<dyn GuardedHttp> = Arc::new(CompanionHttp::new().map_err(|_| "companion_http")?);
    let endpoints: Endpoints = Arc::new(move || {
        let ops = ops.clone();
        Box::pin(async move { prowlarr_endpoints(ops.as_ref()).await })
    });
    start_with(
        records,
        installed,
        integrations,
        endpoints,
        gate,
        http,
        Arc::new(|id: &CompanionId| detect::health(id)),
        watch::EVERY,
    )
}

/// At serve: reads the records, then [`start`]s. A failure is logged by
/// code and leaves the gate `Unknown`: gated plugins wait (fail closed).
pub async fn serve(
    companions: &crate::companions::Companions,
    store: Arc<Mutex<Store>>,
    settings: &crate::settings::Registry,
    ops: Arc<dyn OpsApi>,
    gate: channel::Sender<requires::Snapshot>,
) -> Option<Wired> {
    let catalogue = lodestar_companions::builtin().ok()?;
    let rt = runtime(crate::companions::host());
    let found = tokio::task::spawn_blocking(move || records(&store, catalogue, rt))
        .await
        .unwrap_or_default();
    match start(
        found,
        companions.snapshot.clone(),
        settings.subscribe(),
        ops,
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
    records: Vec<Record>,
    mut installed: channel::Receiver<Snapshot>,
    mut integrations: channel::Receiver<Integrations>,
    endpoints: Endpoints,
    gate: channel::Sender<requires::Snapshot>,
    http: Arc<dyn GuardedHttp>,
    checker: watch::Checker,
    every: Duration,
) -> Result<Wired, &'static str> {
    let list = |r: &[Record]| -> Vec<(CompanionId, Target)> {
        r.iter().map(|r| (r.id.clone(), r.target.clone())).collect()
    };
    let (records_tx, mut records_rx) = channel::channel(records);
    let (list_tx, list_rx) = channel::channel(list(&records_rx.borrow()));
    let (mut watched, watching) = watch::spawn(http, checker, list_rx, every);
    let composing = tokio::spawn(async move {
        let mut tick = tokio::time::interval(REFRESH.min(every * 3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let endpoints = endpoints().await;
            let snapshot = {
                let records = records_rx.borrow_and_update().clone();
                list_tx.send_if_modified(|l| {
                    let next = list(&records);
                    let changed = *l != next;
                    *l = next;
                    changed
                });
                requires::Snapshot::Known(present(
                    &installed.borrow_and_update(),
                    &watched.borrow_and_update(),
                    &integrations.borrow_and_update().clients,
                    endpoints.as_deref(),
                    &records,
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
                r = watched.changed() => if r.is_err() { return },
                r = integrations.changed() => if r.is_err() { return },
                r = records_rx.changed() => if r.is_err() { return },
            }
        }
    });
    Ok(Wired {
        records: records_tx,
        tasks: vec![watching, composing],
    })
}

/// CMP-8b's `ConfigSink` over the settings registry and the sources.
#[allow(
    dead_code,
    reason = "CMP-5g-2's confirmation hands configured companions over through it"
)]
pub struct Sink {
    pub settings: Arc<dyn SettingsApi>,
    pub integrations: channel::Receiver<Integrations>,
    pub ops: Arc<dyn OpsApi>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink").finish_non_exhaustive()
    }
}

/// What the managed client entry holds, from CMP-8b's entry.
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
struct ManagedClient<'a> {
    target: &'a Target,
    username: &'a str,
    password: &'a Secret,
    category: &'a str,
}

/// The `integrations` batch that makes `entry` the managed client: the
/// list as it is, with [`MANAGED_CLIENT`] replaced or added. Other clients
/// go without their passwords, which keeps the saved ones.
#[allow(dead_code, reason = "used by Sink, which CMP-5g-2 calls")]
fn client_batch(current: &Integrations, entry: &ManagedClient<'_>) -> SettingsBatch {
    let mut clients: Vec<serde_json::Value> = current
        .clients
        .iter()
        .filter(|c| c.id != MANAGED_CLIENT)
        .map(|c| {
            let mut c = c.clone();
            c.password = None;
            serde_json::to_value(c).expect("a client serializes")
        })
        .collect();
    clients.push(serde_json::json!({
        "id": MANAGED_CLIENT,
        "name": "qBittorrent (Lodestar)",
        "kind": "qbittorrent",
        "url": format!("{}/", target_url(entry.target)),
        "username": entry.username,
        "password": entry.password.expose(),
        "category": entry.category,
    }));
    let mut patch = serde_json::Map::new();
    patch.insert("clients".into(), serde_json::Value::Array(clients));
    SettingsBatch(std::iter::once(("integrations".to_owned(), SettingsPatch(patch))).collect())
}

impl ConfigSink for Sink {
    fn download_client<'a>(
        &'a self,
        entry: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            let managed = ManagedClient {
                target: &entry.target,
                username: entry.username.as_str(),
                password: &entry.password,
                category: entry.category.as_ref().map_or("", |c| c.as_str()),
            };
            let batch = client_batch(&self.integrations.borrow(), &managed);
            let settings = self.settings.clone();
            tokio::task::spawn_blocking(move || settings.save(batch))
                .await
                .map_err(|_| SinkError::Unavailable)?
                .map(|_| ())
                .map_err(|_| SinkError::Rejected)
        })
    }

    fn indexer_manager<'a>(
        &'a self,
        entry: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            let sources = self
                .ops
                .sources()
                .await
                .map_err(|_| SinkError::Unavailable)?;
            let id = sources
                .iter()
                .find(|s| s.plugin == PROWLARR_SOURCE)
                .map(|s| s.id)
                .ok_or(SinkError::Unavailable)?;
            // The key first: an address with no key fails its checks
            // rather than reaching Prowlarr unauthenticated.
            self.ops
                .patch_source_credentials(id, vec![("api_key".into(), Some(entry.api_key))])
                .await
                .map_err(|_| SinkError::Rejected)?;
            let patch = SourcePatch {
                config: Some(serde_json::json!({
                    "url": target_url(&entry.target),
                    "manager": "prowlarr",
                })),
                ..SourcePatch::default()
            };
            self.ops
                .patch_source(id, patch, false)
                .await
                .map(|_| ())
                .map_err(|_| SinkError::Rejected)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_companions::adapter::EnteredUrl;
    use lodestar_companions::guided::Origin;
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
        let endpoints: Endpoints = Arc::new(|| Box::pin(async { None }));
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
            Duration::from_millis(20),
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
        w.records.send_replace(vec![Record {
            id: id("prowlarr"),
            target: Target::Loopback(PortNumber::new(9696).unwrap()),
            origin: Origin::Confirmed,
        }]);
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
        w.records.send_replace(Vec::new());
        tokio::time::timeout(
            deadline,
            gate.wait_for(|g| known(g, &|p| p.companions.is_empty())),
        )
        .await
        .expect("the gate kept a companion you removed")
        .unwrap();
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
        let mut w = Watched::new();
        w.insert(id("qbittorrent"), Seen::Healthy);
        w.insert(id("prowlarr"), Seen::Missing);
        let p = present(&Snapshot::default(), &w, &[], None, &[]);
        assert_eq!(p.companions.len(), 1);
        assert_eq!(p.companions["qbittorrent"], requires::Health::Healthy);
        // Lodestar's own install is judged by its own health, not the watch.
        w.insert(id("prowlarr"), Seen::Healthy);
        let p = present(
            &installed(&[("prowlarr", manager::Health::Unhealthy)]),
            &w,
            &[],
            None,
            &[],
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
        let record = Record {
            id: id("prowlarr"),
            target: Target::Entered(EnteredUrl::parse("http://prowlarr.lan:9696").unwrap()),
            origin: Origin::Confirmed,
        };
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
        let target = Target::Loopback(PortNumber::new(8080).unwrap());
        let password = Secret::new(String::from("generated-password"));
        let entry = ManagedClient {
            target: &target,
            username: "lodestar",
            password: &password,
            category: "lodestar",
        };
        let batch = client_batch(&current, &entry);
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
