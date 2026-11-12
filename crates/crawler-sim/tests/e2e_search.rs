//! The in-process DHT test network (architecture §17, M8-NET-E2E): about
//! 50 real DHT nodes on loopback and fake peers serving known info dicts
//! over the real peer wire. A fresh install's crawler joins, and every
//! known torrent must be found by `/search` within 5 s of its peer
//! starting to serve:
//!
//! ```text
//!  fake peers ──announce──▶ DHT nodes ◀──BEP 51 sample, get_peers── crawler (DhtSource)
//!      ▲                                                               │
//!      └────────────── BEP 9/10 metadata (peer-wire fetcher) ◀── fetch pool
//!                                                                      │
//!   /api/v1/search ◀── SearchIndex ◀── live indexer ◀── SQLite ◀── writer
//! ```
//!
//! Everything from the crawler's socket to the search response is the
//! production code; only the network around it is the test's.

mod e2e;

use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use e2e::app::App;
use e2e::net::{Client, Network};
use e2e::torrents::{Rng, Torrent, WORD, generate};
use e2e::wait::until;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::FailureKind;
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer};

/// Every name, size and file list comes from this seed.
const SEED: u64 = 0x4c4f_4445_5354_4152;
/// DHT nodes besides the crawler's.
const NODES: usize = 50;
/// Honest peers, and the torrents each seeds.
const PEERS: usize = 8;
const PER_PEER: usize = 2;
/// The pass bar (§17).
const BAR: Duration = Duration::from_secs(5);
/// The wait's own deadline: longer than the bar, so a miss says by how
/// much.
const DEADLINE: Duration = Duration::from_secs(20);

fn hash(t: &Torrent) -> InfoHash {
    InfoHash(t.seed.info_hash)
}

/// One line on the test's real stderr, past libtest's capture, so CI logs
/// keep the measured time of a passing run.
fn report(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announced_torrents_are_searchable_within_five_seconds() {
    let mut rng = Rng::new(SEED);
    let known = generate(&mut rng, PEERS * PER_PEER, "Known");
    let forged = generate(&mut rng, 1, "Forged").remove(0);
    let orphan = generate(&mut rng, 1, "Orphan").remove(0);
    let mut client_id = [0u8; 20];
    client_id[..8].copy_from_slice(&rng.next().to_be_bytes());

    // The network and an install, ready before any peer serves.
    let net = Network::start(NODES).await;
    let prepared = App::prepare();
    let first = std::env::var_os("E2E_CRAWLER_FIRST").is_some();
    let (mut early, mut prepared) = if first {
        let app = prepared.start(net.bootstrap().to_string()).await;
        app.joined().await;
        (Some(app), None)
    } else {
        (None, Some(prepared))
    };

    // The clock starts as the peers start serving.
    let started = Instant::now();
    let mut peers = Vec::new();
    for chunk in known.chunks(PER_PEER) {
        let seeds = chunk.iter().map(|t| t.seed.clone()).collect();
        peers.push(FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await);
    }
    // Serves bytes whose SHA-1 isn't the hash it announces.
    let liar = FakePeer::spawn(
        vec![forged.seed.clone()],
        Accept::Both,
        Behavior::WrongData,
        vec![],
    )
    .await;
    let mut client = Client::new(client_id).await;
    for (chunk, peer) in known.chunks(PER_PEER).zip(&peers) {
        for t in chunk {
            client
                .announce(net.bootstrap(), hash(t), peer.addr().port())
                .await;
        }
    }
    client
        .announce(net.bootstrap(), hash(&forged), liar.addr().port())
        .await;
    // Asked about, so the network lists it, but nobody has it.
    client.ask_only(net.bootstrap(), hash(&orphan)).await;
    let announced = started.elapsed();
    // The crawler starts once its swarms are announced: a node it had
    // already sampled would wait out its BEP 51 interval (at least 60 s)
    // before listing them.
    let app = match early.take() {
        Some(app) => app,
        None => {
            prepared
                .take()
                .unwrap()
                .start(net.bootstrap().to_string())
                .await
        }
    };

    let app_ref = &app;
    let known_ref = &known;
    // When the writer had stored them all: the rest is the indexer's.
    let mut stored_at = None;
    let stored_ref = &mut stored_at;
    let mut stages: Vec<(u64, u64, Duration)> = Vec::new();
    let stages_ref = &mut stages;
    let results = until(
        "every known torrent to be searchable",
        DEADLINE,
        async || {
            let stored = app_ref
                .pipeline
                .writer_stats()
                .stored
                .load(Ordering::Relaxed);
            let rx = app_ref
                .pipeline
                .intake_stats()
                .received
                .load(Ordering::Relaxed);
            let f = app_ref
                .pipeline
                .pool_stats()
                .fetched
                .load(Ordering::Relaxed);
            if stages_ref.last().map(|l| (l.0, l.1)) != Some((rx, f)) {
                stages_ref.push((rx, f, started.elapsed()));
            }
            if stored_ref.is_none() && stored >= known_ref.len() as u64 {
                *stored_ref = Some(started.elapsed());
            }
            let r = app_ref.search(WORD, 100).await;
            let missing: Vec<&str> = known_ref
                .iter()
                .filter(|t| !r.hits.iter().any(|h| h.name == t.name))
                .map(|t| t.name.as_str())
                .collect();
            if missing.is_empty() {
                Ok(r)
            } else {
                Err(format!(
                    "{} of {} missing: {missing:?}",
                    missing.len(),
                    known_ref.len()
                ))
            }
        },
    )
    .await;
    let took = started.elapsed();
    report(&format!("stages (received, fetched, at): {stages:?}"));
    let dht = app.dht.stats().unwrap_or_default();
    report(&format!(
        "e2e_search: {} torrents searchable {took:?} after their peers started serving \
         (bar {BAR:?}; announced by {announced:?}, stored by {stored_at:?}); dht samples_sent={} harvested_sample={} get_peers={} announce={} \
         lookups_completed={} lookup_ms={}/{} timeouts={} throttled={} peer_rejected={}; \
         network dropped/timeouts={:?}; pool fetched={} failures={:?}",
        known.len(),
        dht.samples_sent,
        dht.harvested_sample,
        dht.harvested_get_peers,
        dht.harvested_announce,
        dht.lookups_completed,
        dht.peer_lookup_millis,
        dht.peer_lookups_answered,
        dht.timeouts,
        dht.throttled,
        dht.peer_rejected,
        net.dropped_and_timeouts(),
        app.pipeline.pool_stats().fetched.load(Ordering::Relaxed),
        app.pipeline
            .pool_stats()
            .failures
            .each_ref()
            .map(|f| f.load(Ordering::Relaxed)),
    ));

    // Each by name, with the size and file count of the dict its peer
    // served, under its own hash.
    for t in &known {
        let hits: Vec<_> = results.hits.iter().filter(|h| h.name == t.name).collect();
        assert_eq!(hits.len(), 1, "{}: {hits:?}", t.name);
        let h = hits[0];
        assert_eq!(h.hash, t.hash_hex(), "{}", t.name);
        assert_eq!((h.size, h.files), (t.size, t.files), "{}", t.name);
    }

    // The forged dict is fetched and rejected: the failure is counted
    // as a hash mismatch and nothing is stored.
    let pool = app.pipeline.pool_stats();
    until("the forged metadata to be rejected", DEADLINE, async || {
        let n = pool.failure_count(FailureKind::HashMismatch);
        (n >= 1)
            .then_some(())
            .ok_or(format!("hash mismatches: {n}"))
    })
    .await;
    // The orphan is harvested and looked up, and the lookup finds no
    // peer: it waits in the queue with an attempt spent.
    let orphan_hash = hash(&orphan);
    let queue = app.pipeline.monitor();
    until(
        "the orphan's lookup to come back empty",
        DEADLINE,
        async || {
            let snap = queue.pool().snapshot().await;
            match snap.iter().find(|k| k.info_hash == orphan_hash) {
                Some(k) if k.attempts >= 1 => Ok(()),
                Some(k) => Err(format!("queued, {} attempts", k.attempts)),
                None => Err(format!("not queued ({} keys are)", snap.len())),
            }
        },
    )
    .await;
    assert!(
        !app.stored(&forged.seed.info_hash),
        "forged metadata stored"
    );
    assert!(
        !app.stored(&orphan.seed.info_hash),
        "a hash with no peer stored"
    );
    assert_eq!(
        app.pipeline.writer_stats().stored.load(Ordering::Relaxed),
        known.len() as u64,
        "only the known torrents are stored"
    );
    // Searched again after both were decided: still only the known ones.
    let after = app.search(WORD, 100).await;
    assert_eq!(after.total, known.len(), "{:?}", after.hits);
    for name in [&forged.name, &orphan.name] {
        assert!(
            after.hits.iter().all(|h| &h.name != name),
            "{name} searchable"
        );
    }

    app.shutdown().await;
    net.shutdown().await;
    drop((peers, liar));

    assert!(
        took <= BAR,
        "the known torrents took {took:?} to become searchable, over the {BAR:?} bar"
    );
}
