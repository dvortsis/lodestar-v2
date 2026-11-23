# CR-SOFT-TIMEOUT decisions

CR-SOFT-TIMEOUT is `docs/build-plan.md` §0.5's "bounded soft timeout", built now rather than waiting for CR-5. The runner placed it in `docs/decisions/2026-11-23-runner-records-r14.md`, on the owner's 2026-11-23 delegation (`docs/owner-decisions.md` §2.82). It sits behind `dht.soft_timeout`, or `LODESTAR_DHT_SOFT_TIMEOUT`, and is **off by default**. With the switch off, lookups wait exactly as before: the CR-SIM goldens are unchanged, and no state is allocated (the actor's soft-query set stays empty and never grows).

## The problem
CR-0's ledger shows a lookup holding its slot for about 24 s of the 25 s `LOOKUP_DEADLINE`. A leg ends only when its closest live candidates have all answered. With 3 queries in flight, a 4 s `query_timeout` and about 38 % of queries unanswered, every silent node in the closest set holds the leg's end for 4–5 s.

## The design: mechanism (a) only (runner, 2026-11-23, §2.26; flagged to the owner, §2.19)
- **What the switch does.** A query that goes soft stops holding the leg's end once `K` others have answered. That's all it does: it keeps its `ALPHA` lane.
- **What the leg gains.** It asks exactly whom it would with the switch off, at the same times, and sends nothing more. It ends at the earlier of the switch-off end and the soft end, so it never ends later than with the switch off.
- **Why not free the lane (mechanisms b and c).** The approved step-0 design also freed the soft query's lane for the next candidate, and the first build did. The runner's review then asked that a leg never end later than with the switch off, in every scenario.
  - **No lane-freeing design can promise that.** A freed lane asks a node the switch-off leg wouldn't have asked yet. That node can list a node closer than any the switch-off leg knows, and then the leg must ask it too. So a leg can end later.
  - **So the runner chose (a) alone.** It meets the brief's "only shortens" exactly, sends no extra queries, and adds no bound beside the owner's bounds against slow peers.
  - With it go the 2 · `ALPHA` per-leg allowance and the 3/4-of-the-cap check the first build needed. Under (a) the check could never fire.
- **Parked: X-soft-lane-free.** Lane freeing as its own switch, judged by a distribution rule rather than per-scenario dominance. Not launched; it would need its own screen and review. The (c) screen below shows what it gains.

## What changes with the switch on
- **Scope:** owned peer requests' `get_peers` legs only, as `dht.lookup_end`'s scope.
  - Scrapes, bootstrap, refresh and exploration `find_node` lookups don't change.
  - Neither do queries outside the candidate list (a hint or a router), which still hold the leg until the query timeout.
- **The soft expiry.** On the maintenance tick, after the hard timeouts and their drives, a leg's query to a candidate that has waited `SOFT_TIMEOUT` = **1 s** goes **soft**: the candidate goes to a new state, `Slow`.
  - The wait is never more than `query_timeout`.
  - **It keeps its lane:** the leg's and the request's in-flight counts don't change. So the leg's choice of whom to ask, which still counts the slow candidate among its `K`, is the switch-off leg's.
  - **The end tests skip it.** `Lookup::is_done` and `Lookup::closest_answered` look at the `K` closest candidates that are neither failed nor slow. If those have all answered (and, for `is_done`, no other query is in flight), the leg ends.
  - **But it isn't a failure.** While any slow query is out, the end tests need `K` answers. A leg with fewer waits for its slow queries to answer or reach the query timeout, as with the switch off. So unreachable ids alone can't end a leg.
  - **Never later than off:** whenever the switch-off end test holds, these do.
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
- **Nothing new is sent:** no query, rate, cap or `ALPHA` changes. A leg has at most `ALPHA` queries out, as today, and every bound stays as it is:
  - the 64-query per-request cap;
  - `LOOKUP_DEADLINE` and `query_timeout`;
  - the query rate and the outstanding cap.
- **S8-08 (low): a slow honest closest node no longer anchors the closest set.** While it's slow, ids that answer at once near the target can end a leg sooner than they could with the switch off.
  - **Accepted, because** lookups already accept fast answers under CR-LOOKUP-END, whose threat model this stays inside, and the 64-query cap and the query rate still bound what is sent.
  - A slow candidate never counts as answered, and while one is out a leg needs `K` answers to end. `dht.lookup_end`'s two-network rule still applies to the candidates that did answer.
- **A candidate whose query is out** (in flight or slow) is never dropped when closer candidates crowd the list, so a late answer always finds it.
- **Memory:** one counter per lookup, and one entry in the actor's soft-query set per soft query. Detached entries are bounded by the outstanding cap and expire at `query_timeout`.

## The value: 1 s
- **Where it comes from: the calibrated CR-SIM scenario** (`live-lookup`, X-crawler-sim-calibrate).
  - Every real reply there arrives within 460 ms, except the 15 % that are slow at 3 s. Stale contacts never answer, and they are what hold `W` near 24 s.
  - 1 s covers every fast reply with at least 2× margin. The 3 s replies become late replies, which exercises the late path in the screen.
- **CR-LEDGER has no time-to-answer series,** only each request's time to first values, so it couldn't place the value. The live calibration signal is the ratio of the two new counters, `late ÷ soft`: the cloud lane's mechanism check reads it beside `W`.
- **The runner's call** (2026-11-23): 1 s. A screen-only knob (`Config::soft_timeout_after`, default `SOFT_TIMEOUT`) lets CR-SIM run 2 s beside it. The app has no setting for it and always runs 1 s.

## The screen (calibrated `live-lookup`, 8 seeds, off against on)
Build-plan §0.5's standing rule: keep only if stored per slot-second and stored per DHT query both rise (item 32's CR-1c rule doesn't apply). Means over seeds 1–8.

SCREEN_TABLE

- **Reproduce:** `cargo test -p lodestar-crawler-sim --test sim soft_timeout_screen -- --ignored --nocapture`.
- **What a screen can't show:** it never replaces a live run. The simulator is IPv4-only, with approximate routing and no churn, and its latency model is two-valued.

## Tests
- **`crates/dht/src/node/soft_timeout_tests.rs`,** against the real actor on tokio's paused clock, with all four `soft_timeout` × `lookup_end` combinations where they apply:
  - a silent closest node stops holding the leg once K others have answered, and nothing more is sent; off, the leg waits for the query timeout;
  - soft queries keep their lanes and send nothing more, with tokens and room to spare;
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
  - **The guarantee,** on a scripted network of 80 fast, late and silent nodes, with `dht.lookup_end` off and on:
    - with the switch on, a leg never ends later than with it off;
    - until it ends, it sends exactly the same queries at the same times;
    - a leg that ends with slow queries out has its K answers;
    - the bookkeeping holds at every step (a leg has at most `ALPHA` out).
  - **The mutation run:** freeing the lane, as the first build did, fails this test on "the same queries before the end", and fails 11 other tests.
  - Hostile replies to soft queries (spoofed, repeated, to detached queries, with ticks between) settle nothing twice, end nothing more than once and stay bounded.
  - In `lookup.rs`, any sequence of adds, sends, soft expiries, answers, failures and retries keeps the counters equal to the states and at most `ALPHA` out.
- **CR-SIM:** `soft_timeout_runs_deterministically` (not ignored) and the `#[ignore]` screen above.
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
