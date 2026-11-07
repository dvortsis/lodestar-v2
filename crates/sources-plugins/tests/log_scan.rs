//! The no-profiles log scan (SRC-G, architecture §18.7, §18.9;
//! `docs/decisions/2026-11-07-SRC-G.md`). In its own test binary: it
//! installs the process's only log sink and turns every event on, which
//! would reach any other test running beside it.

use std::collections::BTreeSet;
use std::sync::Mutex;

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::Pipeline;
use lodestar_sources::{Found, SourcePlugin};
use lodestar_store::{Store, StoreConfig};
use std::sync::Arc;

#[path = "support/harness.rs"]
mod harness;
#[path = "support/plugins.rs"]
mod plugins;

use harness::{core_with, pipeline_config, until};
use plugins::{fixture_runs, relay};

/// This crate's folder, for the shared fixture runs.
const PLUGINS_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Every event the pipeline, filter, DHT, peer wire and integrations
/// declare is on, and every record is kept.
struct Capture(Mutex<Vec<(String, String, bool)>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, r: &lodestar_log::Record<'_>) {
        let all = lodestar_log::Mask(u32::MAX).without_opt_ins();
        self.0.lock().unwrap().push((
            r.text(all, None),
            r.text(lodestar_log::Mask::PRIVATE, None),
            r.pairs_torrent(),
        ));
    }
}

/// The log scan (§18.9): a fixture run with every event on, a failed
/// fetch from a peer included, shows no peer address beside a torrent
/// unless the opt-in Peer activity kind is on, even with every other kind
/// (hashes and peer addresses included) shown. With the default preset
/// no key a source found is printed at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_log_scan_finds_no_peer_address_beside_a_torrent() {
    static CAPTURE: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();
    let capture = CAPTURE.get_or_init(|| Capture(Mutex::new(Vec::new())));
    lodestar_log::set_sink(capture).expect("this binary's only sink");
    for c in [
        &lodestar_pipeline::log_events::CATALOGUE,
        &lodestar_filter::log_events::CATALOGUE,
        &lodestar_dht::log_events::CATALOGUE,
        &lodestar_peer_wire::log_events::CATALOGUE,
        &lodestar_integrations::log_events::CATALOGUE,
    ] {
        for e in c.events {
            e.set_on(true);
        }
    }
    // A peer that refuses: its address and the key meet in a failed fetch.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = closed.local_addr().unwrap();
    drop(closed);
    let runs = fixture_runs();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    let mut senders = Vec::new();
    for r in &runs {
        let (p, tx) = relay(&r.manifest);
        plugins.push(p);
        senders.push((r, tx));
    }
    let mut config = pipeline_config(core_with(&["lexa"], &["rolea"]));
    config.fetch.allow_private = true;
    config.pool.max_attempts = 1;
    let fetched = InfoHash([0x66; 20]);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config, store, plugins).await.unwrap();
    let mut keys: BTreeSet<InfoHash> = BTreeSet::from([fetched]);
    for (r, tx) in &senders {
        keys.extend(r.finds.iter().map(|f| f.key));
        let mut round = r.finds.clone();
        if r.kind == "nyaa" {
            round.push(Found::key(fetched, 0).with_peers(vec![peer]));
        }
        tx.send(round).await.unwrap();
    }
    until("the failed fetch logged", || {
        capture
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(all, _, pairs)| *pairs && all.contains("Metadata fetch failed"))
    })
    .await;
    pipeline.shutdown().await.unwrap();
    let lines = capture.0.lock().unwrap().clone();
    let address = peer.ip().to_string();
    for (all, private, pairs) in &lines {
        if *pairs {
            assert!(!all.contains(&address), "a peer beside a torrent: {all}");
        }
        // The address shows only where no torrent is named at all.
        let names_a_key = keys.iter().any(|k| {
            let hex = k.to_string();
            all.contains(&hex) || all.contains(&hex.to_uppercase())
        });
        assert!(
            !(names_a_key && all.contains(&address)),
            "a peer beside a key: {all}"
        );
        for k in &keys {
            let (lower, upper) = (k.to_string(), k.to_string().to_uppercase());
            assert!(
                !private.contains(&lower) && !private.contains(&upper),
                "a key at the default preset: {private}"
            );
        }
    }
    // The scan saw the case it's for: a peer and a key in one record,
    // with the key shown and the peer not.
    assert!(
        lines.iter().any(|(all, _, pairs)| *pairs
            && all.contains(&fetched.to_string())
            && all.contains("peer=[hidden]")),
        "{lines:?}"
    );
}
