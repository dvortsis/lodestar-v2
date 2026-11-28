//! The VPN hold (X-vpn-bind): while the crawl's network interface is
//! down, the crawler reads paused with a reason code whatever its setting
//! says, and the DHT plugin runs no node; when it's back, the setting
//! applies again (a pause the user chose stays on) and the node starts
//! again on fresh sockets.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{Config as DhtConfig, Intensity};
use lodestar_metainfo::InfoHash;
use lodestar_network_policy::iface::{Down, Guard, Interface, Link};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Control, DhtSource, Pipeline, PipelineConfig};
use lodestar_sources::{BoxFuture, Fetcher};
use lodestar_store::{Store, StoreConfig};

/// The longest any condition here is waited for.
const DEADLINE: Duration = Duration::from_secs(10);

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

/// A guard whose probe the test switches between up (`lo`'s real check
/// on Linux, a fixed link elsewhere) and down.
fn switched() -> (Arc<Guard>, Arc<AtomicBool>) {
    let up = Arc::new(AtomicBool::new(false));
    let u = up.clone();
    let guard = Guard::with_probe(Interface::parse("lo").unwrap(), move |i| {
        if !u.load(Ordering::SeqCst) {
            return Err(Down::Missing);
        }
        if cfg!(target_os = "linux") {
            lodestar_network_policy::iface::check(i)
        } else {
            Ok(Link::default())
        }
    });
    (guard, up)
}

struct Started {
    pipeline: Pipeline,
    control: Control,
    dht: Arc<DhtSource>,
    _dir: tempfile::TempDir,
}

/// A pipeline whose one source is the DHT on loopback, with `guard`, and
/// a control started with the setting's `paused`.
async fn start(guard: Arc<Guard>, paused: bool) -> Started {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    // Pinned to a device (Linux), so `lo`'s own addresses.
    let dht = Arc::new(DhtSource::new(DhtConfig {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        bootstrap: vec![],
        allow_private: true,
        guard: cfg!(target_os = "linux").then(|| guard.clone()),
        ..DhtConfig::default()
    }));
    let mut config = PipelineConfig {
        vpn: Some(guard),
        ..PipelineConfig::default()
    };
    config.pool.paused = paused;
    let pipeline = Pipeline::start_with(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![dht.clone()],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(
        pipeline.monitor(),
        Some(dht.clone()),
        store,
        paused,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    Started {
        pipeline,
        control,
        dht,
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

fn running(dht: &DhtSource) -> bool {
    dht.node_handle().lock().unwrap().is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_down_interface_holds_the_crawl_and_its_return_rebinds_the_dht() {
    let (guard, up) = switched();
    let s = start(guard.clone(), false).await;
    let st = s.control.status();
    assert!(st.paused, "down at start: paused, {st:?}");
    assert_eq!(st.vpn.unwrap().down, Some("missing"));
    // The setting says run; the hold still wins.
    s.control.sync(|| (false, Intensity::Balanced)).await;
    assert!(s.control.status().paused);
    assert!(!running(&s.dht), "no node, so no packet, while it's down");

    up.store(true, Ordering::SeqCst);
    guard.poll();
    until("the crawl to resume", || !s.control.status().paused).await;
    assert_eq!(s.control.status().vpn.unwrap().down, None);
    until("the node to start", || running(&s.dht)).await;

    // It goes: paused, and the node stops.
    up.store(false, Ordering::SeqCst);
    guard.poll();
    until("the hold", || s.control.status().paused).await;
    until("the node to stop", || !running(&s.dht)).await;

    // Back: a new node, on new sockets.
    up.store(true, Ordering::SeqCst);
    guard.poll();
    until("the node to start again", || running(&s.dht)).await;
    until("the crawl to resume again", || !s.control.status().paused).await;
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_the_user_chose_outlasts_the_hold() {
    let (guard, up) = switched();
    up.store(true, Ordering::SeqCst);
    guard.poll();
    let s = start(guard.clone(), false).await;
    assert!(!s.control.status().paused);
    s.control.set_paused(true).await;
    up.store(false, Ordering::SeqCst);
    guard.poll();
    until("the hold", || {
        s.control.status().vpn.unwrap().down == Some("missing")
    })
    .await;
    up.store(true, Ordering::SeqCst);
    guard.poll();
    until("the hold to lift", || {
        s.control.status().vpn.unwrap().down.is_none()
    })
    .await;
    assert!(s.control.status().paused, "the user's pause stays");
    s.pipeline.shutdown().await.unwrap();
}

#[tokio::test]
async fn without_a_guard_there_is_no_vpn_status() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(pipeline.monitor(), None, store, false, Intensity::Balanced)
        .await
        .unwrap();
    let st = control.status();
    assert!(st.vpn.is_none() && !st.paused, "{st:?}");
    pipeline.shutdown().await.unwrap();
}
