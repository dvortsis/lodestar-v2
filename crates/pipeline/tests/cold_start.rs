//! A start doesn't wait for the have-filter (X-healthz-cold-start), and
//! nothing reads it as complete before it is: the fill is held here
//! between its scan and its mark, so
//! the pipeline is up, the intake waits, the filter answers "stored" to
//! every hash, and no key reaches the fetcher. Once the fill is let go,
//! the stored hash is a rediscovery, never fetched, and a new key is
//! fetched, only ever with the filter filled.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{FillGate, Monitor, Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Found, Kind, Manifest, Network, PluginError, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use lodestar_store::{NewTorrent, Store, StoreConfig};
use tokio::sync::mpsc;

/// A source the test drives through a channel.
struct Scripted {
    manifest: Manifest,
    rx: Mutex<Option<mpsc::Receiver<Found>>>,
}

impl SourcePlugin for Scripted {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut rx = self.rx.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(f) = rx.recv() => {
                        ctx.emitter.emit(f).await.map_err(|_| PluginError("closed".into()))?;
                    }
                }
            }
        })
    }
}

fn scripted() -> (Arc<Scripted>, mpsc::Sender<Found>) {
    let (tx, rx) = mpsc::channel(64);
    let s = Scripted {
        manifest: Manifest {
            id: "feed",
            name: "feed",
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
        rx: Mutex::new(Some(rx)),
    };
    (Arc::new(s), tx)
}

/// Records each key it's asked for, and whether the have-filter was
/// filled when it was; fetches nothing.
#[derive(Default)]
struct Recording {
    monitor: OnceLock<Monitor>,
    asked: Mutex<Vec<(InfoHash, bool)>>,
}

struct Fetch(Arc<Recording>);

impl Fetcher for Fetch {
    fn fetch(
        &self,
        _peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let filled = self.0.monitor.get().is_some_and(|m| m.have().is_filled());
        self.0.asked.lock().unwrap().push((key, filled));
        Box::pin(async { Err(FailureKind::Refused) })
    }
}

/// A gate the fill thread waits at until the test opens it.
#[derive(Default)]
struct Gate {
    entered: AtomicBool,
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    fn pass(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// Opens the gate when dropped, so a failed assertion doesn't leave the
/// fill thread waiting, and the runtime's shutdown with it.
struct OpenOnDrop(Arc<Gate>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Polls until a 10 s deadline (standards §2).
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn stored_info() -> Info {
    let mut d = BTreeMap::new();
    d.insert(b"length".to_vec(), OwnedValue::Int(1_000));
    d.insert(
        b"name".to_vec(),
        OwnedValue::from("stored before the start"),
    );
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![7; 20]));
    Info::parse(&to_vec(&OwnedValue::Dict(d))).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_start_doesnt_wait_for_the_fill_and_nothing_is_admitted_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let info = stored_info();
    let stored = info.v1.unwrap();
    {
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let src = store.register_source("importer", "1", 0).unwrap();
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: 1_600_000_000,
                metadata_at: 1_600_000_000,
                extra_flags: 0,
                source: src,
            }])
            .unwrap();
    }
    let fresh = InfoHash([0x42; 20]);

    let gate = Arc::new(Gate::default());
    let _open = OpenOnDrop(gate.clone());
    let config = PipelineConfig {
        fill_gate: Some(FillGate::new({
            let gate = gate.clone();
            move || gate.pass()
        })),
        batch_wait: Duration::from_millis(20),
        flush_every: Duration::from_millis(100),
        ..PipelineConfig::default()
    };
    let (source, feed) = scripted();
    let fetcher = Arc::new(Recording::default());
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    // Returns with the fill held: the start doesn't wait for it.
    let pipeline = Pipeline::start_with(
        config,
        store,
        vec![source],
        Arc::new(Fetch(fetcher.clone())),
    )
    .await
    .unwrap();
    let m = pipeline.monitor();
    assert!(fetcher.monitor.set(m.clone()).is_ok());
    until("the fill to reach its gate", || {
        gate.entered.load(Ordering::SeqCst)
    })
    .await;

    // Held: not filled, and it answers "stored" to every hash, even one
    // the empty bloom has never seen, and never asks for a rebuild.
    let have = m.have();
    assert!(!have.is_filled());
    assert!(have.contains(&fresh) && have.contains(&stored));
    assert!(!have.is_full());

    // Both keys are sent while the fill is held. The intake is parked on
    // the fill before it reads one: an intake that didn't wait would
    // never be counted here, and this would time out.
    for key in [stored, fresh] {
        feed.send(Found::key(key, 0).with_peers(vec!["127.0.0.1:6881".parse().unwrap()]))
            .await
            .unwrap();
    }
    until("the intake to wait for the fill", || have.waiting() >= 1).await;
    assert_eq!(m.intake_stats().received.load(Ordering::Relaxed), 0);
    assert!(fetcher.asked.lock().unwrap().is_empty());

    gate.release();
    until("the fill to finish", || have.is_filled()).await;
    assert!(have.contains(&stored));
    assert!(!have.contains(&fresh));
    until("both keys through the intake", || {
        m.intake_stats().received.load(Ordering::Relaxed) == 2
    })
    .await;
    until("the new key to be fetched", || {
        fetcher
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|(k, _)| *k == fresh)
    })
    .await;
    assert_eq!(m.intake_stats().known.load(Ordering::Relaxed), 1);
    let asked = fetcher.asked.lock().unwrap().clone();
    assert!(
        asked.iter().all(|(k, filled)| *k != stored && *filled),
        "fetched a stored key, or before the fill: {asked:?}"
    );
    drop(feed);
    pipeline.shutdown().await.unwrap();
}

/// The id bound holds the count, and a start over a file store fills
/// the filter completely, sized for twice the count.
#[tokio::test]
async fn the_bound_holds_the_count_and_the_filter_is_filled() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(store.torrent_bound().unwrap(), 0);
    let info = stored_info();
    let src = store.register_source("importer", "1", 0).unwrap();
    store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: 1_600_000_000,
            metadata_at: 1_600_000_000,
            extra_flags: 0,
            source: src,
        }])
        .unwrap();
    let bound = store.torrent_bound().unwrap();
    assert!(bound >= store.count().unwrap());
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        store,
        Vec::new(),
        Arc::new(Fetch(Arc::default())),
    )
    .await
    .unwrap();
    let m = pipeline.monitor();
    tokio::time::timeout(Duration::from_secs(10), m.have().filled())
        .await
        .expect("filled within 10 s");
    assert!(m.have().contains(&info.v1.unwrap()));
    assert!(m.have().capacity() >= 2 * m.torrents());
    pipeline.shutdown().await.unwrap();
}
