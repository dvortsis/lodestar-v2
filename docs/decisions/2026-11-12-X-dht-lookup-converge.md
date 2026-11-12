# X-dht-lookup-converge decisions

M8-NET-E2E found it: about 1 run in 10, a known hash was harvested, its `get_peers` lookup completed with no peers, and the key waited out the pool's 120 s `retry_backoff`. The fix is in `crates/dht/src/node/actor.rs`, `lookup.rs` and `routing.rs`. The new tests are in `node/join_tests.rs`, plus an ignored loopback reproducer, `tests/lookup_converge.rs`.

## Two root causes, one symptom

The reproducer: 50 loopback nodes, 18 swarms announced to their true-closest 8 nodes, then a fresh node asking for all 18 at once.

- **Shape A, a lookup with nobody to ask.**
  - `start_peer_request` seeds an owned `get_peers` lookup from the routing table only.
  - Started before the bootstrap router answered, it has no candidates. So `next_limit` returns nothing, `Lookup::is_done` is true (nothing in flight, no fresh candidate), and `finish_lookup` answers empty, within microseconds.
  - In 2 of 15 rounds, all 18 lookups ended this way after 1–4 queries.
  - In production this hits every request made while the table is empty. The clearest case is a start with no `state.bin`, when the pool's restored fetch queue asks at once: every key got an instant empty answer, then 120 s of `retry_backoff`.
- **Shape B, a lookup trapped in one neighbourhood.**
  - The stuck lookups knew only nodes in the half of the keyspace away from their target, and all 8 closest answered, so `is_done` was right by Kademlia's rule.
  - But every responder's `nodes` were from that same half: none of them knew a single node near the target.
  - The cause: a node never refreshed its far buckets after joining. Bootstrap is `find_node(own id)`, which fills only the neighbourhood. `RoutingTable::refresh_targets` refreshes a bucket only once it's been untouched for `REFRESH_AFTER_SECS`, 15 minutes. Kademlia's join step, "refresh all buckets farther than the closest neighbour", was missing.
  - So for its first 15 minutes, a Lodestar node answered lookups for much of the keyspace with nodes that couldn't lead there, and its own table covered only its own neighbourhood.
  - On the real DHT, mature neighbours mask this for our own lookups, but our answers to other nodes were poor. In a young network such as the e2e test's, it traps lookups outright.
- **Ruled out:** the termination rule, the candidate cap (`CAP`), `ALPHA`, the per-request budget (`MAX_PEER_QUERIES`), dedupe across concurrent lookups, nodes-only answers, and the one-id-per-IP rule (loopback is exempt). There were no timeouts, throttling or send errors in any stuck run.
- **Correction to M8-NET-E2E's first reading:** its option-1 stalls (a passive GetPeers harvest "racing" the announce) were shape B, not a separate race. They came from the same keyspace region, and they recurred in a topology where no race was possible. This slice replaces the planned X-dht-passive-lookup-race.

## A: park, don't seed (runner, §2.33)

- **Parking.** While routers are configured, no bootstrap self-lookup has finished since the start, no family table holds K (8) nodes, and less than `PARK_DEADLINE` (30 s) has passed, owned `get_peers` requests stay in their queue: `Actor::parks_lookups`, checked where the tick starts queued requests.
  - The queue is the existing bounded one (`MAX_GET_PEERS`, 32). Admission already refuses beyond it, and the pool's permits hold the rest, so nothing is dropped silently.
  - They start when the first self-lookup ends (`Lookup::bootstrap` is set on bootstrap lookups), or once a table reaches K, whichever comes first.
  - If the bootstrap fails, its self-lookup still ends, with an empty table, and the requests start and end empty as before. `dht.bootstrap_unreachable` already reports that, so there's no new event or metric.
  - At the deadline they start anyway.
  - With no routers configured (tests, a saved table), nothing parks.
- **Router seeding was rejected.** Seeding every thin-table lookup with the bootstrap routers would fix shape A too. But a cold start's restored queue can hold 200,000 keys, so it would send thousands of queries to the public routers within seconds: the burst we must never send. With parking, routers see only the bootstrap's own queries.
- **`Lookup::bootstrap` is marked explicitly.** Comparing the lookup's target with `ids[0]` misses once BEP 42 gives the node a new id mid-lookup (it does in crawler-sim, whose node has a public address).

## B: the join refresh

- **What it does.** When the first bootstrap self-lookup ends with nodes in the table, the node plans one `find_node` per prefix length shorter than the one it shares with its closest neighbour (`RoutingTable::join_targets`), and runs them once per start. That's Kademlia's join step.
- **Why prefix lengths, not buckets.** The first cut refreshed every existing bucket but the last. It left islands in the reproducer: a young table has split into only 1–2 buckets, so a whole quarter of the keyspace never got a target. Counting by prefix length is Kademlia's own wording, and it's independent of how far the table has split. The runner accepted this refinement.
- **Capped at `JOIN_LEVELS` = 32 levels,** from the farthest. A neighbour that shares a long prefix by chance or design can't inflate the cost: 2^32 nodes is far beyond the DHT's ~2^23.
- **Paced.** The refresh starts lookups only while maintenance lookups number fewer than half of `MAX_LOOKUPS` (8 of 16), so the stale-bucket refresh and the re-bootstrap always have room. Owned `get_peers` lookups don't count against `MAX_LOOKUPS`, so it never takes their slot.
- **Query cost on the real DHT, named to the owner under §2.19 by the runner:**
  - one `find_node` lookup per level up to the closest neighbour, about log2(N) ≈ 20–23 lookups of ~8–20 queries each;
  - about 200–450 queries once per start;
  - at Balanced (200 queries/s, half of it for sampling) that's 1–2 s of spare budget;
  - nothing changes on the wire.

## Lookup cost and behaviour

- A parked request sends nothing until it starts, then costs what it always did. A lookup per request is unchanged: same `ALPHA`, K, `MAX_PEER_QUERIES` and termination.
- The pool's `retry_backoff` after an empty lookup is reported, not changed. 120 s is harsh for a key harvested under a second earlier, but with A and B in place those empty lookups no longer happen at start. It isn't part of the root cause, and it's a §2.26 call for the runner.

## Tests

- **`node/join_tests.rs`: the real actor on a scripted network, answered by hand.** No socket is read and no clock is waited on.
  - A request made before any node is known is parked (queued, not started). After the bootstrap it finds its peer, and the router gets no more `get_peers` than it does in a control run where the same request comes after the bootstrap. While parked, the router saw only the bootstrap's one query.
  - A bootstrap that times out ends parked requests empty.
  - `PARK_DEADLINE` releases them even with the bootstrap still out (the actor's epoch is moved past it, not slept to).
  - With no routers, nothing parks.
  - A joined node whose neighbours know nothing far plans one target per level above its closest neighbour (11 here), keeps maintenance lookups at 8 or fewer, learns the far half, and plans nothing on a second bootstrap round.
  - Without the fix, the peer test fails (empty peers), the far-half test fails (no far node known), and the deadline test fails (not parked).
- **`routing.rs`:** `join_targets` covers exactly levels 0..closest and honours its cap.
- **`tests/lookup_converge.rs`: the loopback reproducer,** `#[ignore]` because it measures on real time (about 45 s).
  - Before: 5 of the first 8 rounds had empty lookups, and the run aborted in round 8.
  - After: 15/15 rounds clean, in each of 6 runs (90 rounds).
- **crawler-sim goldens re-blessed (runner-approved shared edit).** The join refresh changes the query sequence. Steady stored per minute:

| Scenario | Single seed (golden), before → after | 8-seed mean (`noise_floor`), before → after | 8-seed spread, before / after |
|---|---|---|---|
| healthy | 81.3 → 84.4 | 76.0 → 77.6 (+2.1%) | 10.9% / 13.1% |
| high-latency | 30.3 → 28.0 | 33.1 → 31.6 (−4.5%) | 15.1% / 24.4% |
| admission-pressure | 30.7 → 27.9 | 29.6 → 30.9 (+4.4%) | 15.4% / 26.3% |
| restart-honoured (leg 2) | 27.7 → 27.9 | — | — |

  Every move is inside the seed spread, and the 8-seed means go both ways. Neither yield nor cost is visibly affected beyond the noise.
