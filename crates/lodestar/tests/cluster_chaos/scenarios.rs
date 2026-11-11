//! The chaos harness's smoke tests (WK-20): each injector once, against the
//! real coordinator and worker runtime. WK-G's §19.8 gate tests build on
//! the same harness.
//!
//! The `lodestar` package is a binary with no library target, so a test
//! binary here couldn't link the runtime. This file is compiled into the
//! binary's own tests instead, as a child of `src/cluster/testing.rs`
//! (`#[path]`), and runs with
//! `cargo test -p lodestar --bin lodestar cluster::testing`.
//! With no `main.rs` in this folder, cargo doesn't build it on its own.

use std::time::Duration;

use super::*;

const MINUTE_MS: i64 = 60_000;

/// Fresh torrents in the swarm, named after the scenario.
fn torrents(h: &Harness, what: &str, n: usize) -> Vec<InfoHash> {
    (0..n)
        .map(|i| h.swarm.add(&format!("{what} {i}")))
        .collect()
}

/// A worker killed while it holds leases: the coordinator keeps them until
/// they expire, then grants them to the worker still alive, which stores
/// them. The funnel reconciles, no key was fetched by two workers within a
/// lease's life, and the killed worker comes back from its saved
/// credentials.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_killed_mid_lease_has_its_keys_released_after_expiry() {
    let mut h = Harness::start(Options::workers(3)).await;
    let keys = torrents(&h, "Killed mid-lease", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;
    h.kill(0).await;
    assert!(!h.running(0));

    // Before expiry nobody else holds them: nothing is stored.
    h.wait_reconciled(|f| f.harvested == 3 && f.in_flight == 3)
        .await;
    assert_eq!(h.stored(&keys), 0);

    h.advance(h.lease_ttl() + Duration::from_secs(1));
    h.wait_leased(3).await;
    // The others' DHTs see them too, with peers (a re-grant carries none).
    h.wait_stored_sighting(&keys, DEADLINE).await;
    let f = h
        .wait_reconciled(|f| f.stored == 3 && f.in_flight == 0)
        .await;
    assert_eq!(
        (f.harvested, f.lost, f.late),
        (3, 0, 0),
        "{}",
        h.diagnosis()
    );
    let others: HashSet<InfoHash> = h
        .swarm
        .fetched_by(1)
        .union(&h.swarm.fetched_by(2))
        .copied()
        .collect();
    assert_eq!(others, keys.iter().copied().collect());
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    // The re-grants went to the live workers; the dead one left the plan.
    let regrants: u64 = (1..3)
        .map(|i| h.seen(i).events.get("regranted").copied().unwrap_or(0))
        .sum();
    assert!(regrants >= 1, "{}", h.diagnosis());
    // A live worker may be between sessions (re-planned when it's back).
    h.wait("the plan without worker 0", DEADLINE, || {
        h.planned(0).is_none() && (1..3).all(|i| h.planned(i).is_some_and(|a| a.fetch))
    })
    .await;

    h.swarm.release(0);
    h.restart(0).await;
    h.wait_connected(0).await;
    assert_eq!(h.starts(0), 2);
    h.shutdown().await;
}

/// A worker cut off both ways while it holds leases: they expire and go
/// to the other worker. Once the link heals, the cut-off worker's results
/// arrive under its old tokens and are ignored as stale (fencing): each
/// key is stored once and the funnel still reconciles.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partitioned_workers_stale_results_are_fenced_off() {
    let h = Harness::start(Options::workers(3)).await;
    let keys = torrents(&h, "Partitioned", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;
    h.partition(0, Cut::Both);
    h.wait_reconciled(|f| f.harvested == 3 && f.in_flight == 3)
        .await;

    h.advance(h.lease_ttl() + Duration::from_secs(1));
    h.wait_leased(3).await;
    // The others' DHTs see them too, with peers (a re-grant carries none).
    h.wait_stored_sighting(&keys, DEADLINE).await;
    h.wait_reconciled(|f| f.stored == 3 && f.in_flight == 0)
        .await;

    // Its fetches finish while it's still cut off; then the link heals.
    let stale = h.remote_stats();
    let before = stale.stale.load(Ordering::Relaxed);
    h.swarm.release(0);
    h.heal(0);
    wait_until("the stale results ignored", DEADLINE, || {
        let n = stale.stale.load(Ordering::Relaxed) - before;
        (n >= 3, (n, h.lines(0)))
    })
    .await;
    h.wait_connected(0).await;
    let f = h.wait_reconciled(|f| f.in_flight == 0).await;
    assert_eq!(
        (f.harvested, f.stored, f.lost, f.late),
        (3, 3, 0, 0),
        "stored once each: {f:?}"
    );
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    h.shutdown().await;
}

/// The coordinator restarts on the same store: every worker reconnects to
/// the new epoch through the same address and keeps working, and the
/// funnel reconciles across the restart. Keys in flight at the restart
/// are restored from the coordinator's own record as its built-in
/// worker's leases, so the worker's earlier-epoch results for them are
/// dropped as stale (plan Q7: the key is held), and the coordinator
/// fetches them itself once a sighting brings it peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workers_reconnect_after_a_coordinator_restart() {
    let mut h = Harness::start(Options::workers(3)).await;
    let done = torrents(&h, "Before the restart", 2);
    for &k in &done {
        h.find(0, k);
    }
    h.wait_stored(&done).await;
    let in_flight = torrents(&h, "Across the restart", 2);
    h.swarm.hold(1);
    for &k in &in_flight {
        h.find(1, k);
    }
    h.wait_fetched(1, &in_flight).await;
    h.wait_reconciled(|f| f.stored == 2 && f.in_flight == 2)
        .await;

    let epoch = h.epoch();
    h.restart_coordinator().await;
    h.wait_all_connected().await;
    assert!(h.epoch() > epoch, "a new epoch");
    h.wait_reconciled(|f| f.harvested == 4 && f.in_flight == 2)
        .await;

    // New work goes through the new epoch.
    let after = torrents(&h, "After the restart", 2);
    for &k in &after {
        h.find(2, k);
    }
    h.wait_stored(&after).await;

    // The held fetches finish: stale under the restored leases.
    let stats = h.remote_stats();
    h.swarm.release(1);
    wait_until("the earlier epoch's results dropped", DEADLINE, || {
        let n = stats.stale.load(Ordering::Relaxed);
        (n >= 2, (n, stats.late.load(Ordering::Relaxed)))
    })
    .await;
    assert_eq!(h.stored(&in_flight), 0);

    h.wait_stored_sighting(&in_flight, DEADLINE).await;
    assert!(
        in_flight.iter().all(|k| h
            .swarm
            .log()
            .iter()
            .any(|r| r.key == *k && r.node == COORDINATOR && r.generation == 1)),
        "{:?}",
        h.swarm.log()
    );
    let f = h
        .wait_reconciled(|f| f.stored == 6 && f.in_flight == 0)
        .await;
    assert_eq!((f.harvested, f.lost, f.late), (6, 0, 0), "{f:?}");
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    h.shutdown().await;
}

/// Keys whose holder died and that no live worker can take: once their
/// leases expire they wait for a re-grant, and the funnel counts them in
/// flight all along.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_keys_with_nobody_to_take_them_stay_in_flight() {
    let mut h = Harness::start(Options::workers(1)).await;
    let keys = torrents(&h, "Nobody left", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;
    h.kill(0).await;
    h.advance(h.lease_ttl() + Duration::from_secs(1));
    h.wait("expired, waiting for a re-grant", DEADLINE, || {
        h.fleet().pending == 3
    })
    .await;
    let f = h
        .wait_reconciled(|f| f.harvested == 3 && f.in_flight == 3)
        .await;
    assert_eq!((f.stored, f.lost), (0, 0), "{f:?}");
    h.shutdown().await;
}

/// A worker whose clock is off: past the allowed offset (60 s) it is
/// flagged and told to fix its clock; past the certificates' slack
/// (5 min) the TLS handshake refuses it. Either way it stays up, holds no
/// lease, and connects as soon as its clock is right again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skewed_worker_clock_is_flagged_then_refused_cleanly() {
    let h = Harness::start(Options::workers(1)).await;
    let w = h.worker_id(0);
    let flagged = || {
        h.fleet()
            .workers
            .iter()
            .any(|s| s.worker == w && s.connected && s.skewed)
    };

    // Two minutes ahead: flagged at its next heartbeat, and told.
    h.skew(0, 2 * MINUTE_MS);
    wait_until("flagged as skewed", DEADLINE, || {
        (flagged(), h.fleet().skew_flags)
    })
    .await;
    wait_until("told to fix its clock", DEADLINE, || {
        let lines = h.lines(0);
        (lines.contains("clock is 120 s off"), lines)
    })
    .await;

    // Ten minutes behind: the coordinator's certificate isn't valid yet
    // by its clock, so it can't connect again, and says why.
    h.skew(0, -10 * MINUTE_MS);
    h.sever(0);
    wait_until("refused at the handshake", DEADLINE, || {
        let lines = h.lines(0);
        let refused = lines.matches("couldn't connect").count() >= 2;
        (refused && !h.connected(0), lines)
    })
    .await;
    assert!(h.running(0), "it keeps trying: {}", h.lines(0));
    assert_eq!(h.fleet().leases, 0);

    // A correct clock again: it connects, and isn't flagged.
    h.skew(0, 0);
    h.wait_connected(0).await;
    wait_until("no longer flagged", DEADLINE, || {
        (!flagged(), h.fleet().workers)
    })
    .await;

    // The offset is relative: a coordinator two minutes behind flags it too.
    h.skew_coordinator(-2 * MINUTE_MS);
    wait_until("flagged by the coordinator's clock", DEADLINE, || {
        (flagged(), h.fleet().workers)
    })
    .await;
    h.shutdown().await;
}

/// The relay's cuts hold bytes, in order, and lose none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_holds_one_direction_until_healed() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.into_split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let relay = Relay::start(Some(echo_addr)).await;
    let mut c = TcpStream::connect(relay.addr()).await.unwrap();
    let mut buf = [0u8; 2];
    for cut in [Cut::ToCoordinator, Cut::ToWorker, Cut::Both] {
        relay.cut(cut);
        c.write_all(b"hi").await.unwrap();
        // Held at the cut (on the way there, or on the echo's way back),
        // so nothing has come back.
        wait_until("held", DEADLINE, || (relay.held() > 0, relay.held())).await;
        assert!(
            matches!(c.try_read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "{cut:?} let it through"
        );
        relay.cut(Cut::None);
        tokio::time::timeout(DEADLINE, c.read_exact(&mut buf))
            .await
            .expect("arrives once healed")
            .unwrap();
        assert_eq!(&buf, b"hi");
    }
    relay.sever();
    let closed = tokio::time::timeout(DEADLINE, c.read(&mut buf)).await;
    assert!(matches!(closed, Ok(Ok(0) | Err(_))), "{closed:?}");
}

#[test]
fn fetches_by_two_workers_under_one_lease_are_violations() {
    let key = InfoHash([1; 20]);
    let (a, b) = (WorkerId(1), WorkerId(2));
    let at = |node, at_ms, lease| FetchRecord {
        key,
        node,
        peer: peer(1),
        generation: 0,
        at_ms,
        lease,
    };
    // Handed over (an expiry): fine, however close in time.
    assert!(violations(&[at(0, 0, Some((a, 1))), at(1, 0, Some((b, 2)))], 600_000).is_empty());
    // The same lease seen by two nodes: a second fetch before any
    // hand-over, however far apart in time (heartbeats renew a lease).
    assert_eq!(
        violations(
            &[at(0, 0, Some((a, 1))), at(1, 900_000, Some((a, 1)))],
            600_000
        )
        .len(),
        1
    );
    // The old holder fetching again under the new holder's lease.
    let log = [
        at(0, 0, Some((a, 1))),
        at(1, 600_001, Some((b, 2))),
        at(0, 600_002, Some((b, 2))),
    ];
    assert_eq!(violations(&log, 600_000).len(), 1);
    // A node's own retries are never a violation.
    assert!(violations(&[at(0, 0, Some((a, 1))), at(0, 5, Some((a, 1)))], 600_000).is_empty());
    // With no lease seen, time decides, within a generation.
    assert_eq!(
        violations(&[at(0, 0, None), at(1, 599_999, None)], 600_000).len(),
        1
    );
    assert!(violations(&[at(0, 0, None), at(1, 600_000, None)], 600_000).is_empty());
    let later = FetchRecord {
        generation: 1,
        ..at(1, 0, None)
    };
    assert!(violations(&[at(0, 0, None), later], 600_000).is_empty());
}
