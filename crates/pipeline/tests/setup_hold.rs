//! The setup hold (LEGAL-2): while the instance waits for setup, no
//! source runs and the crawler reads paused whatever its setting says;
//! once setup completes, the sources start and the setting applies, so
//! a pause the user chose stays on.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::Intensity;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Control, Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Kind, Manifest, Network, PluginError, Priority, Risk, SourceCtx,
    SourcePlugin,
};
use lodestar_store::{Store, StoreConfig};
use tokio::sync::watch;

/// The longest any condition here is waited for.
const DEADLINE: Duration = Duration::from_secs(10);

/// Records whether it ran: a stand-in for the DHT, which binds its
/// sockets only once `run` is called.
struct Probe {
    manifest: Manifest,
    ran: Arc<AtomicBool>,
}

impl SourcePlugin for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        self.ran.store(true, Ordering::SeqCst);
        Box::pin(async move {
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
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
    control: Control,
    ran: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

/// A pipeline with one enabled source, `setup_done` as its hold, and a
/// control started with the setting's `paused`.
async fn start(setup_done: Option<watch::Receiver<bool>>, paused: bool) -> Started {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let ran = Arc::new(AtomicBool::new(false));
    let probe = Arc::new(Probe {
        manifest: Manifest {
            id: "probe",
            name: "probe",
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
            always_adult: false,
        },
        ran: ran.clone(),
    });
    let mut config = PipelineConfig {
        setup_done,
        ..PipelineConfig::default()
    };
    config.pool.paused = paused;
    let pipeline = Pipeline::start_with(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![probe],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(pipeline.monitor(), None, store, paused, Intensity::Balanced)
        .await
        .unwrap();
    Started {
        pipeline,
        control,
        ran,
        _dir: dir,
    }
}

/// Waits for `ok`, up to [`DEADLINE`].
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while !ok() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Lets every task spawned so far run as far as it can (a current-thread
/// runtime runs nothing else in between).
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_held_pipeline_runs_no_source_until_setup_then_resumes() {
    let (setup, rx) = watch::channel(false);
    let s = start(Some(rx), false).await;
    settle().await;
    assert!(!s.ran.load(Ordering::SeqCst), "no source runs before setup");
    let st = s.control.status();
    assert!(st.paused && st.waiting_for_setup, "{st:?}");
    // The setting says run; the hold still wins until setup.
    s.control.sync(|| (false, Intensity::Balanced)).await;
    assert!(s.control.status().paused);
    settle().await;
    assert!(!s.ran.load(Ordering::SeqCst));

    setup.send_replace(true);
    until("the source to start", || s.ran.load(Ordering::SeqCst)).await;
    until("the crawler to resume", || !s.control.status().paused).await;
    assert!(!s.control.status().waiting_for_setup);
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_pause_the_user_chose_stays_on_after_setup() {
    // Paused by the setting from the start…
    let (setup, rx) = watch::channel(false);
    let s = start(Some(rx), true).await;
    setup.send_replace(true);
    until("the source to start", || s.ran.load(Ordering::SeqCst)).await;
    until("the hold to lift", || !s.control.status().waiting_for_setup).await;
    assert!(s.control.status().paused, "the user's pause stays");
    s.pipeline.shutdown().await.unwrap();

    // …or paused while it waited for setup.
    let (setup, rx) = watch::channel(false);
    let s = start(Some(rx), false).await;
    s.control.set_paused(true).await;
    setup.send_replace(true);
    until("the hold to lift", || !s.control.status().waiting_for_setup).await;
    assert!(s.control.status().paused, "the user's pause stays");
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_instance_already_set_up_starts_unheld() {
    let (_setup, rx) = watch::channel(true);
    let s = start(Some(rx), false).await;
    let st = s.control.status();
    assert!(!st.paused && !st.waiting_for_setup, "{st:?}");
    until("the source to start", || s.ran.load(Ordering::SeqCst)).await;
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_shutdown_before_setup_never_runs_a_source() {
    let (setup, rx) = watch::channel(false);
    let s = start(Some(rx), false).await;
    tokio::time::timeout(DEADLINE, s.pipeline.shutdown())
        .await
        .expect("a held pipeline stops promptly")
        .unwrap();
    assert!(!s.ran.load(Ordering::SeqCst));
    // Setup after the stop starts nothing.
    setup.send_replace(true);
    settle().await;
    assert!(!s.ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_dropped_signal_keeps_the_hold() {
    let (setup, rx) = watch::channel(false);
    let s = start(Some(rx), false).await;
    drop(setup);
    settle().await;
    assert!(!s.ran.load(Ordering::SeqCst));
    assert!(s.control.status().waiting_for_setup);
    s.pipeline.shutdown().await.unwrap();
}
