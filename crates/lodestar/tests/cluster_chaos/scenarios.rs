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

    // Not before the TTL: a little short of it, two ticks later, the dead
    // worker still holds all three and nothing waits for a re-grant.
    let margin = Duration::from_secs(30);
    h.advance(h.lease_ttl() - margin);
    let ticks = h.fleet().ticks;
    h.wait("two ticks", DEADLINE, || h.fleet().ticks >= ticks + 2)
        .await;
    let f = h.fleet();
    let dead = f.workers.iter().find(|w| w.worker == h.worker_id(0));
    assert_eq!(
        (f.pending, dead.map(|w| w.leases)),
        (0, Some(3)),
        "expired early: {}",
        h.diagnosis()
    );

    h.advance(margin + Duration::from_secs(1));
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

/// CLUSTER-01: a worker killed while it holds leases and started again
/// well inside the lease TTL, with nothing spooled. Its new session lists
/// what it holds (nothing), so the coordinator gives back every lease the
/// old process lost at once, instead of renewing them on the new
/// session's heartbeats with nobody fetching them. The clock never moves:
/// no lease waits for its expiry. Each key is stored once and the funnel
/// reconciles.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_restarted_inside_the_ttl_gets_its_lost_leases_regranted() {
    let mut h = Harness::start(Options::workers(2)).await;
    let keys = torrents(&h, "Restarted inside the TTL", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;
    let before = h.fleet().reconcile_returned;
    let authority = h.server().authority().clone();
    let old: Vec<u64> = keys
        .iter()
        .map(|k| authority.get(k).expect("leased").token.0)
        .collect();
    h.kill(0).await;
    // The killed process fetches nothing more: what's fetched from here
    // on is under the leases given back.
    h.swarm.handed_back(0);
    h.wait_reconciled(|f| f.harvested == 3 && f.in_flight == 3)
        .await;
    h.swarm.release(0);

    h.restart(0).await;
    h.wait_connected(0).await;
    h.wait("the lost leases given back", DEADLINE, || {
        h.fleet().reconcile_returned >= before + 3
    })
    .await;
    // Given back, then granted again under new tokens: none is left on
    // record under an old one. The killed process can't use those tokens
    // any more, so any later fetch is under a lease given back
    // (`handed_back(0)` above).
    h.wait_leased(3).await;
    for &k in &keys {
        let now = authority.get(&k).map(|l| l.token.0);
        assert!(
            now.is_none_or(|t| !old.contains(&t)),
            "{k:?} still under its old lease\n{}",
            h.diagnosis()
        );
    }
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
    assert_eq!(h.fleet().reconcile_refused, 0, "{}", h.diagnosis());
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    assert_eq!(h.starts(0), 2);
    h.shutdown().await;
}

/// CLUSTER-05 and CLUSTER-02 end to end. A worker stopped locally while
/// it holds leases hands them back as it stops, so they're granted again
/// with no clock movement, long before the TTL. The worker taking them has
/// a lease cap of 2 (its pool cap of 1, set through the placement): it's
/// granted two, and the third waits for a re-grant until the cap is
/// lifted. Each key is stored once, the funnel reconciles, and no key is
/// fetched by two workers outside a hand-back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_workers_leases_go_back_at_once_within_the_takers_cap() {
    let mut h = Harness::start(Options::workers(2)).await;
    let authority = h.server().authority().clone();
    let taker = h.worker_id(1);
    let placement = h.coord().placement.clone();
    let cap = move |pool_cap| {
        let s = lodestar_cluster::placement::WorkerSettings {
            pool_cap,
            ..Default::default()
        };
        placement.set_settings(taker, s).unwrap();
    };
    cap(1);
    h.wait("the taker's cap follows its pool cap", DEADLINE, || {
        authority.lease_cap(taker) == Some(2)
    })
    .await;

    let keys = torrents(&h, "Stopped locally", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;
    // What the taker says is held while it's granted the keys, so its
    // pool's evictions (below) can't free cap room before we look.
    h.partition(1, Cut::ToCoordinator);
    // From here on worker 0 starts no fetch: its stop hands them back.
    h.swarm.handed_back(0);
    assert_eq!(h.stop(0).await, Exit::Shutdown);
    assert!(
        h.lines(0)
            .contains("stopping: handing the current work back")
    );
    h.swarm.release(0);

    h.wait("two granted to the taker, one waiting", DEADLINE, || {
        authority.held_by(taker) == 2 && authority.pending() == 1
    })
    .await;
    let f = h.fleet();
    assert!(
        f.workers
            .iter()
            .all(|w| w.worker != h.worker_id(0) || !w.connected),
        "{}",
        h.diagnosis()
    );

    h.heal(1);
    cap(lodestar_cluster::assign::DEFAULT_POOL_CAP);
    h.wait_stored_sighting(&keys, DEADLINE).await;
    // The taker's pool keeps its first cap (1) for its stages' run, so a
    // key granted past it is evicted and lost, then harvested again when
    // it's sighted: losses here are the pool's, and each is counted once.
    let f = h
        .wait_reconciled(|f| f.stored == 3 && f.in_flight == 0)
        .await;
    assert_eq!((f.harvested - f.lost, f.late), (3, 0), "{}", h.diagnosis());
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    let fetched_by_taker = h.swarm.fetched_by(1);
    assert!(keys.iter().all(|k| fetched_by_taker.contains(k)));
    h.shutdown().await;
}

/// A worker cut off both ways while it holds leases: they expire and go
/// to the other workers under new tokens. While those new leases are live
/// (their holders' fetches are held), the link heals and the cut-off
/// worker's results arrive under its old tokens: each is dropped as stale
/// because a newer token holds the key (fencing), and nothing is stored.
/// Then the new holders finish, each key is stored once, and the funnel
/// reconciles.
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

    // The new holders' fetches will wait, so their leases stay live.
    h.swarm.hold(1);
    h.swarm.hold(2);
    h.advance(h.lease_ttl() + Duration::from_secs(1));
    h.wait_leased(3).await;
    // The others' DHTs see them too, with peers (a re-grant carries none).
    let new_holders_fetching = || {
        let by: HashSet<InfoHash> = h
            .swarm
            .fetched_by(1)
            .union(&h.swarm.fetched_by(2))
            .copied()
            .collect();
        keys.iter().all(|k| by.contains(k))
    };
    h.wait_sighting(
        "the new holders fetching",
        &keys,
        DEADLINE,
        new_holders_fetching,
    )
    .await;

    // Seen again while those leases are live: each claim must only relay
    // hints to the holder. A coordinator that gave a live key to the
    // claimer would show here as a second worker fetching it.
    let relayed = h.fleet().hints_relayed;
    h.wait_sighting("the sightings relayed as hints", &keys, DEADLINE, || {
        h.fleet().hints_relayed >= relayed + 3 || !h.fetch_violations().is_empty()
    })
    .await;
    assert_eq!(h.fetch_violations(), Vec::<String>::new());

    // The cut-off worker's fetches finish and the link heals, in either
    // order: its results come in under tokens a newer lease replaced.
    let stats = h.remote_stats();
    let before = stats.stale.load(Ordering::Relaxed);
    h.swarm.release(0);
    h.heal(0);
    wait_until("the stale results ignored", DEADLINE, || {
        let n = stats.stale.load(Ordering::Relaxed) - before;
        (n >= 3, (n, h.lines(0)))
    })
    .await;
    assert_eq!(h.stored(&keys), 0, "a stale result was stored");
    let authority = h.coord().pipeline.monitor().lease_authority().clone();
    let live: Vec<_> = keys
        .iter()
        .map(|k| authority.get(k).map(|l| l.holder))
        .collect();
    assert!(
        live.iter()
            .all(|w| *w == Some(h.worker_id(1)) || *w == Some(h.worker_id(2))),
        "the new leases were live: {live:?}"
    );

    h.swarm.release(1);
    h.swarm.release(2);
    h.wait_stored(&keys).await;
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

/// The holder sights its own keys again after it has fetched them but
/// before their commit reaches it, as a busy DHT does. Its claim gets its
/// own lease back; that must never end the lease as lost nor send the key
/// to another worker (X-wk-own-lease-lost).
///
/// Made deterministic with the relay: the coordinator's answers to those
/// claims are held until the holder's pool has fetched the keys, so they
/// land in the window between the fetch and the commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_sighting_its_own_keys_keeps_their_leases() {
    let h = Harness::start(Options::workers(3)).await;
    let keys = torrents(&h, "Sighted by its holder", 3);
    h.swarm.hold(0);
    for &k in &keys {
        h.find(0, k);
    }
    h.wait_fetched(0, &keys).await;

    // Its sightings' claims reach the coordinator (its heartbeats say
    // so), and the answers, "your own lease", wait in the relay.
    h.partition(0, Cut::ToWorker);
    let w0 = h.worker_id(0);
    let claimed = || {
        h.fleet()
            .workers
            .iter()
            .find(|w| w.worker == w0)
            .map_or(0, |w| w.stats.claimed)
    };
    let before = claimed();
    for &k in &keys {
        h.find(0, k);
    }
    h.wait("the holder's claims sent", DEADLINE, || {
        claimed() >= before + 3
    })
    .await;

    // The fetches finish and the results are committed; then the answers
    // reach the holder, whose pool has fetched the keys already.
    h.swarm.release(0);
    h.wait_stored(&keys).await;
    h.heal(0);
    let counts = h.counts(0).expect("worker 0 runs");
    h.wait("the commits acknowledged", DEADLINE, || {
        counts.committed.load(Ordering::Relaxed) >= 3
    })
    .await;
    let f = h
        .wait_reconciled(|f| f.stored == 3 && f.in_flight == 0)
        .await;
    assert_eq!(
        (counts.lost.load(Ordering::Relaxed), f.harvested, f.lost),
        (0, 3, 0),
        "the holder's own lease came back and ended as lost: {}",
        h.diagnosis()
    );
    assert_eq!(h.fetch_violations(), Vec::<String>::new());
    let fetchers: HashSet<usize> = h.swarm.log().iter().map(|r| r.node).collect();
    assert_eq!(fetchers, HashSet::from([0]), "{}", h.diagnosis());
    // Committed: nothing is left out, and nothing held.
    let leases = h.leases(0).expect("worker 0 runs");
    h.wait("the results no longer out", DEADLINE, || {
        leases.pending_len() == 0 && leases.is_empty()
    })
    .await;
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
fn fetches_by_two_workers_outside_an_expiry_are_violations() {
    let key = InfoHash([1; 20]);
    let (a, b) = (WorkerId(1), WorkerId(2));
    let ttl = 600_000;
    let mut seq = 0;
    let mut at = |node, at_ms, lease| {
        seq += 1;
        FetchRecord {
            key,
            node,
            peer: peer(1),
            generation: 0,
            at_ms,
            lease,
            seq,
        }
    };
    let none: &[HandBack] = &[];
    // After an expiry's worth of time, under a new lease: fine.
    let log = [at(0, 0, Some((a, 1))), at(1, ttl, Some((b, 2)))];
    assert!(violations(&log, none, ttl).is_empty());
    // A new lease sooner than an expiry could give one: a coordinator that
    // re-granted a live key. The leases don't excuse it.
    let log = [at(0, 0, Some((a, 1))), at(1, ttl - 1, Some((b, 2)))];
    assert_eq!(violations(&log, none, ttl).len(), 1);
    // ...unless the test recorded an early hand-back (a drain, a revoke)
    // of the first node's leases between the two.
    let marks = [HandBack {
        node: 0,
        seq: log[1].seq,
    }];
    assert!(violations(&log, &marks, ttl).is_empty());
    // The same lease seen by two nodes, at any gap (heartbeats renew it).
    let log = [at(0, 0, Some((a, 1))), at(1, 2 * ttl, Some((a, 1)))];
    assert_eq!(violations(&log, none, ttl).len(), 1);
    // A node's own retries are never a violation.
    let log = [at(0, 0, Some((a, 1))), at(0, 5, Some((a, 1)))];
    assert!(violations(&log, none, ttl).is_empty());
    // With no lease seen, time decides.
    let log = [at(0, 0, None), at(1, ttl - 1, None)];
    assert_eq!(violations(&log, none, ttl).len(), 1);
    // A coordinator restart (a new generation) ends every lease.
    let first = at(0, 0, Some((a, 1)));
    let later = FetchRecord {
        generation: 1,
        ..at(1, 0, Some((WorkerId::LOCAL, 9)))
    };
    assert!(violations(&[first, later], none, ttl).is_empty());
}

/// A hand-back the test records sits between the fetches before and after
/// it, and excuses a second node's fetch only after it.
#[test]
fn a_recorded_hand_back_excuses_only_later_fetches() {
    let swarm = Swarm::new(Arc::new(ManualClock::new(0)));
    let key = InfoHash([2; 20]);
    let at = |node, seq| FetchRecord {
        key,
        node,
        peer: peer(1),
        generation: 0,
        at_ms: 0,
        lease: None,
        seq,
    };
    lock(&swarm.log).push(at(0, 0));
    swarm.handed_back(0);
    lock(&swarm.log).push(at(1, 1));
    assert_eq!(swarm.hand_backs(), vec![HandBack { node: 0, seq: 1 }]);
    assert!(violations(&swarm.log(), &swarm.hand_backs(), 600_000).is_empty());
    // Without it, the same two fetches are a violation.
    assert_eq!(violations(&swarm.log(), &[], 600_000).len(), 1);

    // Recorded after both fetches, it excuses neither: the second came
    // before the hand-back.
    let late = Swarm::new(Arc::new(ManualClock::new(0)));
    lock(&late.log).push(at(0, 0));
    lock(&late.log).push(at(1, 1));
    late.handed_back(0);
    assert_eq!(late.hand_backs(), vec![HandBack { node: 0, seq: 2 }]);
    assert_eq!(
        violations(&late.log(), &late.hand_backs(), 600_000).len(),
        1
    );
}
