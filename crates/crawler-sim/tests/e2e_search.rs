//! The in-process DHT test network (architecture §17, M8-NET-E2E): about
//! 50 real DHT nodes on loopback and fake peers serving known info dicts
//! over the real peer wire. A fresh install's crawler joins, and every
//! known torrent must be returned by `/search` within 5 s of the crawler
//! harvesting it:
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
//!
//! The clock (runner, 2026-11-12): §17 reads "harvest → fetch → store →
//! index → search … within 5 s", so each hash's 5 s runs from the
//! crawler's first sighting of it to `/search` returning it. How long
//! the DHT takes to show the crawler a hash is asserted separately, with
//! a 30 s deadline and no bar (its tail is the sampler-frontier finding
//! in `docs/decisions/2026-11-12-M8-NET-E2E.md`).

mod e2e;

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use e2e::app::App;
use e2e::harvests;
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
/// The pass bar (§17), per hash, from harvest to searchable.
const BAR: Duration = Duration::from_secs(5);
/// Every served hash must be harvested by then (no bar; see above).
const HARVEST_DEADLINE: Duration = Duration::from_secs(30);
/// The search wait: past the last possible harvest plus the bar, so a
/// miss reports how long it took.
const SEARCH_DEADLINE: Duration = Duration::from_secs(40);

fn hash(t: &Torrent) -> InfoHash {
    InfoHash(t.seed.info_hash)
}

/// One line on the test's real stderr, past libtest's capture, so CI logs
/// keep the measured times of a passing run.
fn report(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_harvested_torrent_is_searchable_within_five_seconds() {
    harvests::record();
    let mut rng = Rng::new(SEED);
    let known = generate(&mut rng, PEERS * PER_PEER, "Known");
    let forged = generate(&mut rng, 1, "Forged").remove(0);
    let orphan = generate(&mut rng, 1, "Orphan").remove(0);
    let mut client_id = [0u8; 20];
    client_id[..8].copy_from_slice(&rng.next().to_be_bytes());

    // The network and an install, ready before any peer serves.
    let net = Network::start(NODES).await;
    let prepared = App::prepare();

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
            client.announce(&net, hash(t), peer.addr().port()).await;
        }
    }
    client
        .announce(&net, hash(&forged), liar.addr().port())
        .await;
    // Asked about, so the network lists it, but nobody has it.
    client.ask_only(&net, hash(&orphan)).await;
    // The crawler starts once the swarms are announced: a node it had
    // already sampled would wait out its BEP 51 interval (at least 60 s)
    // before listing them (decisions file, option 1).
    let app = prepared.start(net.bootstrap().to_string()).await;

    // When `/search` first returned each known torrent (by name). Polled,
    // so each is at or after the real moment: the measure only errs long.
    let mut searchable: BTreeMap<[u8; 20], Instant> = BTreeMap::new();
    let app_ref = &app;
    let known_ref = &known;
    let seen = &mut searchable;
    let waited = Instant::now();
    let results = until(
        "every known torrent to be searchable",
        SEARCH_DEADLINE,
        async || {
            let r = app_ref.search(WORD, 100).await;
            let now = Instant::now();
            for t in known_ref {
                if r.hits.iter().any(|h| h.name == t.name) {
                    seen.entry(t.seed.info_hash).or_insert(now);
                }
            }
            let missing: Vec<String> = known_ref
                .iter()
                .filter(|t| !seen.contains_key(&t.seed.info_hash))
                .map(|t| {
                    let harvested = harvests::sampled(&t.seed.info_hash).is_some();
                    format!("{} (harvested: {harvested})", t.name)
                })
                .collect();
            if missing.is_empty() {
                Ok(r)
            } else if waited.elapsed() + Duration::from_secs(1) < SEARCH_DEADLINE {
                // Cheap until the last second: the pool snapshot and stats
                // below are for the timeout message only.
                Err(format!("{} of {} missing", missing.len(), known_ref.len()))
            } else {
                let queue: Vec<String> = app_ref
                    .pipeline
                    .monitor()
                    .pool()
                    .snapshot()
                    .await
                    .iter()
                    .map(|k| {
                        format!(
                            "{:02x}{:02x}:{}",
                            k.info_hash.0[0], k.info_hash.0[1], k.attempts
                        )
                    })
                    .collect();
                let pool = app_ref.pipeline.pool_stats();
                let dht = app_ref.dht.stats().unwrap_or_default();
                Err(format!(
                    "{} of {} missing: {missing:?}; queue (hash:attempts) {queue:?}; \
                     fetched={} lost={:?} failures={:?}; dht lookups_completed={} \
                     peer_admitted={} peer_rejected={} table={} timeouts={} throttled={} \
                     denied={} send_errors={} lookup_deadlines={} peer_queued_dropped={:?} \
                     peer_cancelled={} answered={}",
                    missing.len(),
                    known_ref.len(),
                    pool.fetched.load(Ordering::Relaxed),
                    pool.lost.each_ref().map(|f| f.load(Ordering::Relaxed)),
                    pool.failures.each_ref().map(|f| f.load(Ordering::Relaxed)),
                    dht.lookups_completed,
                    dht.peer_admitted,
                    dht.peer_rejected,
                    dht.routing_nodes,
                    dht.timeouts,
                    dht.throttled,
                    dht.denied,
                    dht.send_errors,
                    dht.lookup_deadlines,
                    dht.peer_queued_dropped,
                    dht.peer_cancelled,
                    dht.peer_lookups_answered,
                ))
            }
        },
    )
    .await;

    // Every served hash was harvested, within the harvest deadline.
    let forged_hash = forged.seed.info_hash;
    until(
        "the forged hash to be harvested",
        HARVEST_DEADLINE,
        async || {
            harvests::sampled(&forged_hash)
                .map(|_| ())
                .ok_or_else(|| "not yet".to_string())
        },
    )
    .await;
    let mut harvest_at = Vec::new();
    for t in known.iter().chain([&forged]) {
        let at = harvests::sampled(&t.seed.info_hash)
            .unwrap_or_else(|| panic!("{} became searchable without a harvest", t.name));
        let after = at.saturating_duration_since(started);
        assert!(
            after <= HARVEST_DEADLINE,
            "{} harvested {after:?} after its peer served",
            t.name
        );
        harvest_at.push(after);
    }

    // The clock's premise: only the crawler samples, so each hash's first
    // `sample` event is its own.
    assert_eq!(net.harvested_sample(), 0, "a network node sampled");

    // The bar, per hash: harvest → searchable.
    let mut lag: Vec<(Duration, &str)> = known
        .iter()
        .map(|t| {
            let h = harvests::sampled(&t.seed.info_hash).expect("checked above");
            let s = searchable[&t.seed.info_hash];
            assert!(s >= h, "{} searchable before it was harvested", t.name);
            (s.duration_since(h), t.name.as_str())
        })
        .collect();
    harvest_at.sort();
    let dht = app.dht.stats().unwrap_or_default();
    report(&format!(
        "e2e_search: harvest->searchable ms per hash {:?}; max {} ms (bar {} ms); \
         T0->harvest ms sorted {:?}; dht samples_sent={} harvested_sample={} \
         lookups_completed={} timeouts={}; network dropped/timeouts={:?}",
        lag.iter().map(|(d, _)| ms(*d)).collect::<Vec<_>>(),
        lag.iter().map(|(d, _)| ms(*d)).max().unwrap_or(0),
        ms(BAR),
        harvest_at.iter().map(|d| ms(*d)).collect::<Vec<_>>(),
        dht.samples_sent,
        dht.harvested_sample,
        dht.lookups_completed,
        dht.timeouts,
        net.dropped_and_timeouts(),
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
    until(
        "the forged metadata to be rejected",
        SEARCH_DEADLINE,
        async || {
            let n = pool.failure_count(FailureKind::HashMismatch);
            (n >= 1)
                .then_some(())
                .ok_or(format!("hash mismatches: {n}"))
        },
    )
    .await;
    // The orphan is harvested and looked up, and the lookup finds no
    // peer: it waits in the queue with an attempt spent.
    let orphan_hash = hash(&orphan);
    let monitor = app.pipeline.monitor();
    until(
        "the orphan's lookup to come back empty",
        SEARCH_DEADLINE,
        async || {
            let snap = monitor.pool().snapshot().await;
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

    lag.sort();
    // The owner's budget is "< 5 s" (§1.6): exactly 5 s misses it.
    let over: Vec<_> = lag.iter().filter(|(d, _)| *d >= BAR).collect();
    assert!(
        over.is_empty(),
        "harvest to searchable at or over the {BAR:?} bar: {over:?}"
    );
}
