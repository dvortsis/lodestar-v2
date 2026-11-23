# CR-SOFT-TIMEOUT decisions

CR-SOFT-TIMEOUT is `docs/build-plan.md` §0.5's "bounded soft timeout", built now rather than waiting for CR-5. The runner placed it in `docs/decisions/2026-11-23-runner-records-r14.md`, on the owner's 2026-11-23 delegation (`docs/owner-decisions.md` §2.82). It sits behind `dht.soft_timeout`, or `LODESTAR_DHT_SOFT_TIMEOUT`, and is **off by default**. With the switch off, lookups wait exactly as before: the CR-SIM goldens are unchanged, and no state is allocated (the actor's soft-query set stays empty and never grows).

## The problem
CR-0's ledger shows a lookup holding its slot for about 24 s of the 25 s `LOOKUP_DEADLINE`. A leg ends only when its closest live candidates have all answered. With 3 queries in flight, a 4 s `query_timeout` and about 38 % of queries unanswered, every silent node in the closest set holds the leg's end for 4–5 s.

## The design: A-window (runner, 2026-11-23, §2.26; flagged to the owner, §2.19)
- **What the switch does.** A query that goes soft keeps its `ALPHA` lane, so a leg never has more than `ALPHA` queries out, as today.
  - **It gives its place among the `K` closest up,** as a failed candidate does. So when one of the leg's lanes frees, the next candidate is asked, and the leg can end once `K` others have answered.
  - It sends no query beyond the caps, and no single query's timer is ever lengthened.
- **The three designs the slice went through:**
  - **C, lane freeing:** the approved step-0 design. A soft query freed its lane at once, bounded by 2 · `ALPHA` out per leg and a 3/4-of-the-cap check. The fresh review's I2 asked that no leg ever end later than with the switch off, in every scenario. No lane-freeing design can promise that: a node asked sooner can list a closer one, which the leg must then ask.
  - **A-strict:** a soft query kept its lane and its place among the `K`; only the end tests skipped it. The leg asked exactly whom, and when, the switch-off leg would, and provably never ended later. But it gained almost nothing (`W` −0.3 s) and failed the standing rule.
  - **A-window (chosen):** lanes are held as in A-strict, but a slow candidate gives its place up, so the walk moves past it once a lane frees.
- **The I2 change is the runner's** (2026-11-23). Per-scenario dominance was its rule, not the owner's. It is replaced by four conditions:
  - no single wait is lengthened;
  - never more than `ALPHA` queries out per leg;
  - the outstanding cap, the rate limit and the 64-query cap are unchanged;
  - the switch passes a distribution rule on the screen: the standing rule on 8/8 seeds, and lookup duration p90 and p99 no worse than off, meaned over the seeds.
- **Why A-window:** it adds no concurrency and no bound beside the owner's bounds against slow peers, so the remaining risk is the tail of the lookup-time distribution, and the screen tests that directly.
- **Parked: X-soft-lane-free.** C (lane freeing) as its own switch, with its own screen and review. Not launched.

## What changes with the switch on
- **Scope:** owned peer requests' `get_peers` legs only, as `dht.lookup_end`'s scope.
  - Scrapes, bootstrap, refresh and exploration `find_node` lookups don't change.
  - Neither do queries outside the candidate list (a hint or a router), which still hold the leg until the query timeout.
- **The soft expiry.** On the maintenance tick, after the hard timeouts and their drives, a leg's query to a candidate that has waited `SOFT_TIMEOUT` = **1 s** goes **soft**: the candidate goes to a new state, `Slow`.
  - The wait is never more than `query_timeout`.
  - **It keeps its lane:** the leg's and the request's in-flight counts don't change, so a leg still has at most `ALPHA` out.
  - **It gives its place among the `K` up.** When the leg chooses whom to ask, and in the end tests (`Lookup::is_done`, `Lookup::closest_answered`), it looks at the `K` closest candidates that are neither failed nor slow. So the next candidate is asked once a lane frees, and if those `K` have all answered (and, for `is_done`, no other query is in flight), the leg ends.
  - **But it isn't a failure.** While any slow query is out, the end tests need `K` answers. A leg with fewer waits for its slow queries to answer or reach the query timeout, as with the switch off. So unreachable ids alone can't end a leg.
  - **Checked on the existing 1 s tick,** as the hard timeout is, so a query goes soft after 1–2 s.
- **A late answer** (after going soft, before the query timeout) is accepted.
  - It settles its candidate once (`Slow` → `Responded`) and frees its lane, and its nodes and values are taken as any answer's would be.
  - The spoof check (only the address asked, on the family asked) runs first, as for every reply.
  - The transaction id is removed when it settles, so a repeat finds nothing and is dropped.
  - At the query timeout, a slow query fails once (`Slow` → `Failed`).
- **When a leg ends with soft queries out** (on its closest set, its values, its deadline, a cancel or a shutdown), its soft queries aren't dropped as its other queries are.
  - They're re-tagged `Purpose::Detached` and stay against the outstanding cap until they're answered or time out.
  - A detached query no longer names its lookup id, so a late answer can't reach a later lookup that reuses the id.
  - **A detached answer feeds no lookup,** but it's handled as any matched answer is: it updates the routing table, offers its nodes to the sampler, votes on our external address and marks the address as answering. A detached timeout marks the node failed in the routing table and counts in `dht_timeouts_total`.
  - **This makes the two arms unlike on timeouts.** With the switch off, a query that outlives its leg is dropped uncounted. With it on, a detached query that never answers counts as a timeout. So the timeout rate (CR-SIM's `unanswered` column, and the live rate) reads higher with the switch on.
  - **The query cap's end waits for every query in flight,** slow ones included, as with the switch off. So it detaches nothing.
- **Two counters,** additive, in `docs/metrics.md` ("Soft timeout"): `dht_soft_timeouts_total` and `dht_soft_timeout_late_replies_total`.

## The bounds
- **No concurrency is added:** a leg has at most `ALPHA` queries out, as today, and no query's timer is lengthened. Every bound stays as it is:
  - the 64-query per-request cap;
  - `LOOKUP_DEADLINE` and `query_timeout`;
  - the query rate and the outstanding cap.
- **S8-08 (low): a slow honest closest node no longer anchors the closest set.** While it's slow, ids that answer at once near the target can end a leg sooner than they could with the switch off.
  - **Accepted, because** lookups already accept fast answers under CR-LOOKUP-END, whose threat model this stays inside, and the 64-query cap, the query rate and the `ALPHA` lanes still bound what is sent. The candidate asked in a slow one's place is the next closest the leg knows, asked on a lane the leg already holds.
  - A slow candidate never counts as answered, and while one is out a leg needs `K` answers to end. `dht.lookup_end`'s two-network rule still applies to the candidates that did answer.
- **A candidate whose query is out** (in flight or slow) is never dropped when closer candidates crowd the list, so a late answer always finds it.
- **Memory:** one counter per lookup, and one entry in the actor's soft-query set per soft query. Detached entries are bounded by the outstanding cap and expire at `query_timeout`.

## The value: 1 s
- **Where it comes from: the calibrated CR-SIM scenario** (`live-lookup`, X-crawler-sim-calibrate).
  - Every real reply there arrives within 460 ms, except the 15 % that are slow at 3 s. Stale contacts never answer, and they are what hold `W` near 24 s.
  - 1 s covers every fast reply with at least 2× margin. The 3 s replies become late replies, which exercises the late path in the screen.
- **CR-LEDGER has no time-to-answer series,** only each request's time to first values, so it couldn't place the value. The live calibration signal is the ratio of the two new counters, `late ÷ soft`: the cloud lane's mechanism check reads it beside `W`.
- **Chosen by the distribution rule:** 1 s passes it with `lookup_end` off and on, and 2 s fails it (stored per slot-second and per query rise on 7/8 seeds with `lookup_end` off). A screen-only knob (`Config::soft_timeout_after`, default `SOFT_TIMEOUT`) lets CR-SIM run 2 s beside it. The app has no setting for it and always runs 1 s.

## The screen (calibrated `live-lookup`, 8 seeds, off against on)
Build-plan §0.5's standing rule: keep only if stored per slot-second and stored per DHT query both rise (item 32's CR-1c rule doesn't apply). Means over seeds 1–8.

**A-window, as built** (stored/slot-s and stored/query: seeds on which each rose; lookup duration is each request's slot residence after the warmup, each seed's percentile meaned over the seeds):

| `lookup_end` | arm | `W` (Little) s | stored/min | stored/slot-s | stored/query | slot-s ↑ | query ↑ | p50 ms | p90 ms | p99 ms | passes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| off | off | 23.11 | 13.13 | 0.0071 | 0.0028 | | | 25,000 | 25,000 | 25,000 | |
| off | on, 1 s | 20.83 | 16.50 | 0.0088 | 0.0033 | 8/8 | 8/8 | 21,534 | 25,000 | 25,000 | yes |
| off | on, 2 s | 21.92 | 15.18 | 0.0081 | 0.0031 | 7/8 | 7/8 | 23,188 | 25,000 | 25,000 | no |
| on | off | 16.20 | 18.64 | 0.0101 | 0.0038 | | | 25,000 | 25,619 | 25,964 | |
| on | on, 1 s | 13.34 | 26.21 | 0.0141 | 0.0048 | 8/8 | 8/8 | 18,887 | 24,115 | 25,732 | yes |
| on | on, 2 s | 14.38 | 22.82 | 0.0123 | 0.0044 | 8/8 | 7/8 | 20,609 | 25,000 | 25,884 | no |

- **The tail.** With `lookup_end` off, p90 and p99 sit at the 25 s deadline in both arms. So "no worse" holds there, but the screen can't see a tail change below the deadline. The cloud lane's ledger series can. Late ÷ soft is 0.131 (`lookup_end` off) and 0.147 (on) at 1 s.

**A-strict** (the slow candidate kept its place among the `K`; provably never later than off):

| `lookup_end` | arm | `W` (Little) s | stored/min | stored/slot-s | stored/query | slot-s ↑ | query ↑ |
|---|---|---|---|---|---|---|---|
| off | on, 1 s | 22.84 | 14.54 | 0.0079 | 0.0031 | 6/8 | 6/8 |
| off | on, 2 s | 22.97 | 13.26 | 0.0072 | 0.0029 | 5/8 | 5/8 |
| on | on, 1 s | 15.96 | 18.42 | 0.0100 | 0.0037 | 4/8 | 4/8 |
| on | on, 2 s | 16.30 | 17.32 | 0.0094 | 0.0035 | 2/8 | 2/8 |

**C** (lane freeing, 2 · `ALPHA` per leg, the 3/4 check):

| `lookup_end` | arm | `W` (Little) s | stored/min | stored/slot-s | stored/query | slot-s ↑ | query ↑ |
|---|---|---|---|---|---|---|---|
| off | on, 1 s | 12.72 | 26.76 | 0.0145 | 0.0041 | 8/8 | 8/8 |
| off | on, 2 s | 15.35 | 22.35 | 0.0120 | 0.0038 | 8/8 | 8/8 |
| on | on, 1 s | 8.08 | 40.10 | 0.0219 | 0.0055 | 8/8 | 8/8 |
| on | on, 2 s | 9.88 | 33.24 | 0.0179 | 0.0052 | 8/8 | 8/8 |

- **Reproduce:** `cargo test -p lodestar-crawler-sim --test sim soft_timeout_screen -- --ignored --nocapture`.
- **What a screen can't show:** it never replaces a live run. The simulator is IPv4-only, with approximate routing and no churn, and its latency model is two-valued.

## Tests
- **`crates/dht/src/node/soft_timeout_tests.rs`,** against the real actor on tokio's paused clock, with all four `soft_timeout` × `lookup_end` combinations where they apply:
  - a silent closest node stops holding the leg once K others have answered, and nothing more is sent; off, the leg waits for the query timeout;
  - soft queries keep their lanes: with tokens and room to spare, going soft sends nothing until a lane frees;
  - a late answer feeds a running leg, settles once and frees its lane;
  - a slow query fails once at the timeout;
  - each permit comes back exactly once on every end (closest set, no values, deadline, cancel, shutdown), and soft queries are detached, not dropped;
  - the 64-query cap and the deadline hold against endless closer ids;
  - the query cap's end waits for its slow queries;
  - a leg whose only candidates left are slow waits for them, and their late answers feed it;
  - the values end (`dht.lookup_end`) with a soft query out detaches it and returns the permit once;
  - a late KRPC error fails a slow query once;
  - refresh and scrape lookups never go soft;
  - a detached answer, and its repeat, never reach a lookup that reused its id;
  - off allocates nothing.
- **Property tests:**
  - **The bounds,** on a scripted network of 80 fast, late and silent nodes, with `dht.lookup_end` off and on, switch on and off:
    - a leg never has more than `ALPHA` queries out and a request never sends more than 64, at every step;
    - no query's timer is extended: after every tick, nothing is still out past the query timeout, soft and detached queries included;
    - a leg that ends with slow queries out has its K answers.
  - **The mutation run:** freeing the lane (C) fails this test on "a leg has 4 out".
  - **The dominance test was dropped, not kept behind a cfg.** It only holds for A-strict, which has no code path of its own any more. A-window can't satisfy it, by design.
  - Hostile replies to soft queries (spoofed, repeated, to detached queries, with ticks between) settle nothing twice, end nothing more than once and stay bounded.
  - In `lookup.rs`, any sequence of adds, sends, soft expiries, answers, failures and retries keeps the counters equal to the states and at most `ALPHA` out.
- **CR-SIM:** `soft_timeout_runs_deterministically` (not ignored) and the `#[ignore]` screen above.
  - The screen reads each request's slot residence from a seam the dht crate has only with its `testing` feature (`Stats::residences`, `Node::residences`), recorded where the ledger records it.
  - A first try timed requests in the simulator's glue, through an extra task. That moved the switch-off baseline by about a quarter, so it was dropped. With the seam, the switch-off rows match the calibrated baseline exactly.
- **`crates/lodestar/src/metrics.rs`:** both counters render as counters with their values, and each has its row in `docs/metrics.md`.
  - They're rendered by a small helper, `render_soft_timeout`, that `render()` calls. A full `render()` call needs a running pipeline.

## Owns (runner, 2026-11-23)
- **Ratified into this slice's owns:**
  - `crates/dht/src/node/soft_timeout_tests.rs`, registered in `actor.rs` beside the other `*_tests.rs` modules (they need the actor's private fields);
  - `crates/dht/proptest-regressions/node/soft_timeout_tests.txt`.
- **The fleet and collector, one line each:**
  - **`docs/soak/tools/linux/collect.py`:** `soft_timeout` → `LODESTAR_DHT_SOFT_TIMEOUT` in `SWITCH_ENV`, recorded and off unless set, with no treatment yet (as CR-SAMPLE-BACKOFF-ESCALATE was in #1029).
  - **`infra/fleet/steps/vm/compose.yaml`:** the switch passed through by bare name, so a cloud screen or an A/B block can set it, and one that doesn't leaves it at its default.
  - **`infra/fleet/steps/vm/block-lib.sh`:** the switch in `CRAWLER_SWITCHES`, so a block unsets it before Lodestar starts, as it does the others.
  - **Why:** X-fleet-switch-passthrough's tests require every `LODESTAR_DHT_*` variable `config.rs` reads to be known to all three, so a new switch can't be missed. Without these lines, CI's `infra` job fails on this PR.
  - **The analyzer needed no change:** `analyze_cr.py` derives its switch lists from `SWITCH_ENV`, and its tests pass with the new entry.
- **`crates/lodestar/src/metrics.rs`:** the two counters, their helper and its test.
