//! Plugins held for their companion (CMP-10): a plugin whose companion is
//! missing doesn't start and says what it needs, once; it starts when the
//! companion arrives and pauses while it's unhealthy, without a restart
//! and without touching other plugins. Unknown state never starts it.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_log::{Record, Sink, Value};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::requires::{Health, Present, Snapshot, State};
use lodestar_pipeline::{Monitor, Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Kind, Manifest, Network, PluginError, Priority, Risk, SourceCtx,
    SourcePlugin,
};
use lodestar_store::{Store, StoreConfig};
use tokio::sync::watch;

/// The longest any condition here is waited for.
const DEADLINE: Duration = Duration::from_secs(10);

const NEEDS: &str = "pipeline.source_needs_companion";
const UNHEALTHY: &str = "pipeline.source_companion_unhealthy";
const READY: &str = "pipeline.source_companion_ready";

// ---- Logs ----

/// Every companion event logged, as `(event id, plugin)`.
struct Capture(Mutex<Vec<(&'static str, &'static str)>>);

impl Sink for Capture {
    fn record(&self, r: &Record<'_>) {
        if !r.event.id.contains("companion") {
            return;
        }
        let plugin = r.fields.iter().find_map(|(k, v)| match (k, v) {
            (&"plugin", Value::Label(p)) => Some(*p),
            _ => None,
        });
        // Ids and codes only: nothing but the plugin and companion.
        let names: Vec<&str> = r.fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["plugin", "companion"], "{}", r.event.id);
        self.0
            .lock()
            .unwrap()
            .push((r.event.id, plugin.expect("a plugin label")));
    }
}

fn capture() -> &'static Capture {
    static SINK: OnceLock<&'static Capture> = OnceLock::new();
    SINK.get_or_init(|| {
        let sink: &'static Capture = Box::leak(Box::new(Capture(Mutex::new(Vec::new()))));
        lodestar_log::set_sink(sink).expect("the only sink in this test binary");
        sink
    })
}

/// How many times `event` was logged for `plugin`.
fn logged(event: &str, plugin: &str) -> usize {
    capture()
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(e, p)| *e == event && *p == plugin)
        .count()
}

/// The tests share plugin ids and the log, so they run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---- Plugins ----

/// Counts its starts and how many runs are live.
struct Probe {
    manifest: Manifest,
    starts: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}

impl SourcePlugin for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.live.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            ctx.shutdown.requested().await;
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
struct Counts {
    starts: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}

impl Counts {
    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

fn probe(id: &'static str, counts: &Counts) -> Arc<dyn SourcePlugin> {
    Arc::new(Probe {
        manifest: Manifest {
            id,
            name: id,
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
            always_adult: false,
            finds_private: false,
        },
        starts: counts.starts.clone(),
        live: counts.live.clone(),
    })
}

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(
        &self,
        _peer: SocketAddr,
        _key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        Box::pin(async { Err(FailureKind::Refused) })
    }
}

struct Started {
    pipeline: Pipeline,
    prowlarr: Counts,
    clients: Counts,
    other: Counts,
    _dir: tempfile::TempDir,
}

impl Started {
    fn monitor(&self) -> Monitor {
        self.pipeline.monitor()
    }

    fn running(&self, plugin: &str) -> bool {
        self.monitor()
            .sources()
            .iter()
            .find(|s| s.plugin == plugin)
            .unwrap()
            .running
    }
}

/// Prowlarr, the download-client source and Nyaa (needs nothing), with
/// `companions` as the gate.
async fn start(companions: Option<watch::Receiver<Snapshot>>) -> Started {
    capture();
    let dir = tempfile::tempdir().unwrap();
    let (prowlarr, clients, other) = (Counts::default(), Counts::default(), Counts::default());
    let pipeline = Pipeline::start_with(
        PipelineConfig {
            companions,
            ..PipelineConfig::default()
        },
        Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap(),
        vec![
            probe("prowlarr", &prowlarr),
            probe("download_clients", &clients),
            probe("nyaa", &other),
        ],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    Started {
        pipeline,
        prowlarr,
        clients,
        other,
        _dir: dir,
    }
}

fn known(pairs: &[(&str, Health)]) -> Snapshot {
    with_client(pairs, false)
}

/// `pairs`, and whether a download client Lodestar doesn't manage is
/// configured in Integrations.
fn with_client(pairs: &[(&str, Health)], external_client: bool) -> Snapshot {
    Snapshot::Known(Present {
        companions: pairs
            .iter()
            .map(|(k, h)| (k.to_string(), *h))
            .collect::<BTreeMap<_, _>>(),
        external_client,
    })
}

/// Waits for `ok`, up to [`DEADLINE`].
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while !ok() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Lets every task spawned so far run as far as it can. Every test here
/// runs on a current-thread runtime (pinned below), where a task runs
/// only when this one yields, and the gate needs one poll per snapshot,
/// so after this nothing it was sent is still unseen. The positive
/// conditions are waited for with [`until`]; this backs only the
/// "nothing happened" assertions.
async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

// ---- Tests ----

#[tokio::test]
async fn the_mapped_plugins_are_in_the_registry_and_none_is_a_peer_finder() {
    let manifests: Vec<&Manifest> = lodestar_sources_plugins::catalogue::manifests().collect();
    for (plugin, _) in lodestar_companions::requires::entries() {
        let m = manifests
            .iter()
            .find(|m| m.id == plugin)
            .unwrap_or_else(|| panic!("{plugin} isn't a plugin of this build"));
        // A paused peer finder would come back without its requests.
        assert!(!m.is(Kind::PeerFinder), "{plugin}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_missing_companion_holds_the_plugin_once_then_it_starts_when_it_arrives() {
    let _serial = SERIAL.lock().await;
    let (needs0, ready0) = (logged(NEEDS, "prowlarr"), logged(READY, "prowlarr"));
    let (tx, rx) = watch::channel(known(&[]));
    let s = start(Some(rx)).await;

    until("nyaa to start", || s.other.live() == 1).await;
    until("the needs line", || logged(NEEDS, "prowlarr") == needs0 + 1).await;
    assert_eq!(
        s.monitor().companion_state("prowlarr"),
        State::Needs("prowlarr")
    );
    assert_eq!(
        s.monitor().companion_state("download_clients"),
        State::Needs("qbittorrent")
    );
    assert_eq!(s.monitor().companion_state("nyaa"), State::Ok);

    // Snapshots that change nothing for it: no start, no more lines.
    for i in 0..50 {
        tx.send_replace(known(&[(
            if i % 2 == 0 { "gluetun" } else { "other" },
            Health::Healthy,
        )]));
        settle().await;
    }
    settle().await;
    assert_eq!(s.prowlarr.starts(), 0, "never started while missing");
    assert_eq!(s.clients.starts(), 0);
    assert!(!s.running("prowlarr"));
    assert_eq!(logged(NEEDS, "prowlarr"), needs0 + 1, "logged once");

    tx.send_replace(known(&[("prowlarr", Health::Healthy)]));
    until("prowlarr to start", || s.prowlarr.live() == 1).await;
    until("prowlarr running", || s.running("prowlarr")).await;
    assert_eq!(s.prowlarr.starts(), 1);
    assert_eq!(logged(READY, "prowlarr"), ready0 + 1);
    assert_eq!(s.monitor().companion_state("prowlarr"), State::Ok);
    // Still no download client.
    assert_eq!(s.clients.starts(), 0);
    tx.send_replace(known(&[
        ("prowlarr", Health::Healthy),
        ("qbittorrent", Health::Healthy),
    ]));
    until("the client source to start", || s.clients.live() == 1).await;
    assert_eq!(
        s.prowlarr.starts(),
        1,
        "an unchanged state doesn't restart it"
    );
    assert_eq!(s.other.starts(), 1);

    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn an_unhealthy_companion_pauses_its_plugin_alone_until_healthy() {
    let _serial = SERIAL.lock().await;
    let unhealthy0 = logged(UNHEALTHY, "prowlarr");
    let (tx, rx) = watch::channel(known(&[
        ("prowlarr", Health::Healthy),
        ("qbittorrent", Health::Healthy),
    ]));
    let s = start(Some(rx)).await;
    until("all three to start", || {
        s.prowlarr.live() == 1 && s.clients.live() == 1 && s.other.live() == 1
    })
    .await;

    for _ in 0..20 {
        tx.send_replace(known(&[
            ("prowlarr", Health::Unhealthy),
            ("qbittorrent", Health::Healthy),
        ]));
        settle().await;
    }
    until("prowlarr to pause", || s.prowlarr.live() == 0).await;
    until("prowlarr not running", || !s.running("prowlarr")).await;
    assert_eq!(
        s.monitor().companion_state("prowlarr"),
        State::Unhealthy("prowlarr")
    );
    assert_eq!(logged(UNHEALTHY, "prowlarr"), unhealthy0 + 1, "logged once");
    assert_eq!(s.prowlarr.starts(), 1, "no restart while unhealthy");
    // The others keep running, never restarted.
    assert_eq!((s.clients.live(), s.clients.starts()), (1, 1));
    assert_eq!((s.other.live(), s.other.starts()), (1, 1));
    assert!(s.running("download_clients") && s.running("nyaa"));

    tx.send_replace(known(&[
        ("prowlarr", Health::Healthy),
        ("qbittorrent", Health::Healthy),
    ]));
    until("prowlarr to resume", || s.prowlarr.live() == 1).await;
    assert_eq!(s.prowlarr.starts(), 2);
    assert_eq!(s.other.starts(), 1);

    s.pipeline.shutdown().await.unwrap();
    assert_eq!(s.prowlarr.live() + s.clients.live() + s.other.live(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_or_lost_state_never_starts_a_gated_plugin() {
    let _serial = SERIAL.lock().await;
    let (tx, rx) = watch::channel(Snapshot::Unknown);
    let s = start(Some(rx)).await;
    until("nyaa to start", || s.other.live() == 1).await;
    settle().await;
    assert_eq!((s.prowlarr.starts(), s.clients.starts()), (0, 0));
    assert_eq!(
        s.monitor().companion_state("prowlarr"),
        State::Needs("prowlarr")
    );

    tx.send_replace(known(&[
        ("prowlarr", Health::Healthy),
        ("qbittorrent", Health::Healthy),
    ]));
    until("both to start", || {
        s.prowlarr.live() == 1 && s.clients.live() == 1
    })
    .await;
    // Unreadable again: both pause.
    tx.send_replace(Snapshot::Unknown);
    until("both to pause", || {
        s.prowlarr.live() == 0 && s.clients.live() == 0
    })
    .await;
    tx.send_replace(known(&[
        ("prowlarr", Health::Healthy),
        ("qbittorrent", Health::Healthy),
    ]));
    until("both to resume", || {
        s.prowlarr.live() == 1 && s.clients.live() == 1
    })
    .await;
    // The manager goes away: fail closed, and nothing restarts after.
    drop(tx);
    until("both to pause", || {
        s.prowlarr.live() == 0 && s.clients.live() == 0
    })
    .await;
    settle().await;
    assert_eq!((s.prowlarr.starts(), s.clients.starts()), (2, 2));
    // The API says so too, not the last snapshot sent.
    assert_eq!(
        s.monitor().companion_state("prowlarr"),
        State::Needs("prowlarr")
    );
    assert_eq!(
        s.monitor().companion_state("download_clients"),
        State::Needs("qbittorrent")
    );
    assert_eq!((s.other.live(), s.other.starts()), (1, 1));

    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn with_the_gate_off_nothing_is_held() {
    let _serial = SERIAL.lock().await;
    let s = start(None).await;
    until("all three to start", || {
        s.prowlarr.live() == 1 && s.clients.live() == 1 && s.other.live() == 1
    })
    .await;
    assert_eq!(s.monitor().companion_state("prowlarr"), State::Ok);
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_strict_gate_on_a_closed_channel_starts_nothing_gated() {
    let _serial = SERIAL.lock().await;
    // The sender is gone before the pipeline starts; it last said all
    // was well, which no longer counts.
    let (tx, rx) = watch::channel(with_client(&[("prowlarr", Health::Healthy)], true));
    drop(tx);
    let s = start(Some(rx)).await;
    until("nyaa to start", || s.other.live() == 1).await;
    settle().await;
    assert_eq!((s.prowlarr.starts(), s.clients.starts()), (0, 0));
    assert!(!s.running("prowlarr") && !s.running("download_clients"));
    assert_eq!(
        s.monitor().companion_state("prowlarr"),
        State::Needs("prowlarr")
    );
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_configured_in_integrations_meets_the_download_client_source() {
    let _serial = SERIAL.lock().await;
    let unhealthy0 = logged(UNHEALTHY, "download_clients");
    // An external client and an unhealthy managed qBittorrent: the
    // external one meets it; Prowlarr still needs its own companion.
    let (tx, rx) = watch::channel(with_client(&[("qbittorrent", Health::Unhealthy)], true));
    let s = start(Some(rx)).await;
    until("the client source to start", || s.clients.live() == 1).await;
    assert_eq!(s.monitor().companion_state("download_clients"), State::Ok);
    settle().await;
    assert_eq!(s.prowlarr.starts(), 0, "an external client isn't Prowlarr");

    // The external client is removed: only the unhealthy companion is left.
    tx.send_replace(known(&[("qbittorrent", Health::Unhealthy)]));
    until("the client source to pause", || s.clients.live() == 0).await;
    until("it reads stopped", || !s.running("download_clients")).await;
    assert_eq!(
        s.monitor().companion_state("download_clients"),
        State::Unhealthy("qbittorrent")
    );
    assert_eq!(logged(UNHEALTHY, "download_clients"), unhealthy0 + 1);
    // And with neither, it needs one, suggesting qBittorrent.
    tx.send_replace(known(&[]));
    settle().await;
    assert_eq!(
        s.monitor().companion_state("download_clients"),
        State::Needs("qbittorrent")
    );
    assert_eq!(s.clients.starts(), 1);
    s.pipeline.shutdown().await.unwrap();
}
