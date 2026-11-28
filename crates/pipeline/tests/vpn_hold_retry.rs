//! X-vpn-bind: a DHT start whose sockets can't be pinned holds the crawl
//! down as `bind_failed` across its retries, without flapping. In its own
//! test binary, so the pin counter it watches counts only this test's
//! attempts. Linux: it's where an unprivileged test can name an interface
//! that doesn't exist.
#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{Config as DhtConfig, Intensity};
use lodestar_metainfo::InfoHash;
use lodestar_network_policy::iface::{self, Guard, Interface, Link};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Control, DhtSource, Pipeline, PipelineConfig};
use lodestar_sources::{BoxFuture, Fetcher};
use lodestar_store::{Store, StoreConfig};

/// Two retries come 2 s and 4 s after the first attempt; this leaves
/// room for a slow runner.
const DEADLINE: Duration = Duration::from_secs(30);

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

/// Waits for `ok`, up to [`DEADLINE`].
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while !ok() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A probe that says up for an interface that doesn't exist: every start
/// fails to pin (`ENODEV`), as an old kernel's `EPERM` would.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pin_that_fails_holds_the_crawl_down_across_its_retries() {
    let guard = Guard::with_probe(Interface::parse("lsnone0").unwrap(), |_| {
        Ok(Link {
            index: Some(1),
            addrs: vec!["127.0.0.1".parse().unwrap()],
        })
    });
    assert!(guard.state().is_up(), "the probe says up");
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let dht = Arc::new(DhtSource::new(DhtConfig {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        bootstrap: vec![],
        allow_private: true,
        guard: Some(guard.clone()),
        ..DhtConfig::default()
    }));
    let pipeline = Pipeline::start_with(
        PipelineConfig {
            vpn: Some(guard.clone()),
            ..PipelineConfig::default()
        },
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
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    until("the hold", || {
        control.status().vpn.unwrap().down == Some("bind_failed")
    })
    .await;
    let first = iface::pins();
    // Two more attempts, each followed by checks that see the link up.
    until("two retries", || {
        guard.poll();
        iface::pins() >= first + 2
    })
    .await;
    let st = control.status();
    assert_eq!(st.vpn.unwrap().down, Some("bind_failed"), "{st:?}");
    assert!(st.paused);
    assert!(dht.node_handle().lock().unwrap().is_none(), "no node runs");
    assert_eq!(guard.trips(), 1, "one trip across the retries: no flapping");
    pipeline.shutdown().await.unwrap();
}
