# CR-SOFT-TIMEOUT decisions

CR-SOFT-TIMEOUT is `docs/build-plan.md` §0.5's "bounded soft timeout", built now rather than waiting for CR-5. The runner placed it in `docs/decisions/2026-11-23-runner-records-r14.md`, on the owner's 2026-11-23 delegation (`docs/owner-decisions.md` §2.82), and approved the step-0 design on 2026-11-23. It sits behind `dht.soft_timeout`, or `LODESTAR_DHT_SOFT_TIMEOUT`, and is **off by default**. With the switch off, lookups wait exactly as before: the CR-SIM goldens are unchanged, and no state is allocated (the actor's soft-query set stays empty and never grows).

## The problem
CR-0's ledger shows a lookup holding its slot for about 24 s of the 25 s `LOOKUP_DEADLINE`. A leg ends only when its closest live candidates have all answered. With 3 queries in flight, a 4 s `query_timeout` and about 38 % of queries unanswered, every silent node in the closest set holds a lane, and holds the leg's end, for 4–5 s.

## What changes with the switch on
- **Scope:** owned peer requests' `get_peers` legs only, as `dht.lookup_end`'s scope.
  - Scrapes, bootstrap, refresh and exploration `find_node` lookups don't change.
  - Neither do queries outside the candidate list (a hint or a router), which still hold the leg until the query timeout.
- **The soft expiry.** On the maintenance tick, after the hard timeouts, a leg's query to a candidate that has waited `SOFT_TIMEOUT` = **1 s** (never more than `query_timeout`) goes **soft**:
  - the candidate goes to a new state, `Slow`;
  - it frees its `ALPHA` lane, so the next candidate is asked (mechanism b);
  - it counts like a failed candidate in the choice of the `K` closest live candidates, so it no longer holds a closest set of `K` that has answered (mechanism a);
  - **but it isn't a failure for ending the leg.** While any slow query is out, both end tests (`Lookup::is_done`, `Lookup::closest_answered`) need `K` answers among the closest live candidates. A leg whose candidates have run out, with slow ones still out and fewer than `K` answers, waits for them to answer or reach the query timeout, as it would with the switch off.
    - **Why (from the self-review):** otherwise a leg whose only candidates left are slow would end at 1–2 s with nothing answered and lose their late answers. That ends a leg earlier than the same script with those answers on time, and not by its closest set or its values, which the runner's go ruled out.
  - the request's in-flight count drops by one at that moment, and never a second time.
  - **The value is checked on the existing 1 s tick,** as the hard timeout is. So a soft query waits 1–2 s, against today's 4–5 s.
  - **The order is timeouts, their drives, then soft expiries and their drives.** A leg whose slow queries just reached the query timeout can mark its next ones soft in the same tick, and the 3/4 rule (below) sees the lanes the timeouts' drives refilled.
- **The query stays out.** Its entry stays pending. So it keeps its place against the global outstanding cap, and against its address's pending count, until it is answered or reaches `query_timeout`.
- **A late answer** (after going soft, before the query timeout) is accepted.
  - It settles its candidate once (`Slow` → `Responded`), and its nodes and values are taken as any answer's would be.
  - The spoof check (only the address asked, on the family asked) runs first, as for every reply.
  - The transaction id is removed when it settles, so a repeat finds nothing and is dropped.
  - At the query timeout, a slow query fails once (`Slow` → `Failed`).
- **When a leg ends with soft queries out** (on its closest set, its values, its deadline, the query cap, a cancel or a shutdown), its soft queries aren't dropped as its other queries are.
  - They're re-tagged `Purpose::Detached` and stay against the outstanding cap until they're answered or time out.
  - **A detached answer feeds no lookup,** but it's handled as any matched answer is: it updates the routing table, offers its nodes to the sampler, votes on our external address and marks the address as answering. A detached timeout marks the node failed in the routing table and counts in `dht_timeouts_total`.
  - **This makes the two arms unlike on timeouts.** With the switch off, a query that outlives its leg is dropped uncounted. With it on, a detached query that never answers counts as a timeout. So the timeout rate (CR-SIM's `unanswered` column, and the live rate) is biased upward with the switch on. Compare it between arms with that in mind.
  - A detached query no longer names its lookup id, so a late answer can't reach a later lookup that reuses the id.
- **Two counters,** additive, in `docs/metrics.md` ("Soft timeout"): `dht_soft_timeouts_total` and `dht_soft_timeout_late_replies_total`.

## The bounds (runner, 2026-11-23, §2.26; flagged to the owner, §2.19)
- **A leg holds at most `ALPHA` (3) slow queries.** While it holds 3, no more go soft, and its in-flight queries keep their lanes until the query timeout, as today.
  - **So a leg can now have up to 6 queries outstanding (2 · `ALPHA`), not 3.** This is a runner refinement under §2.26.
  - The 3 lanes are shared by all of a request's family legs (`drive_lookup` gives a request `3 − in flight`), while the slow limit is per leg. So a request with two family legs holds at most 3 + 3 · 2 = 9 queries.
  - All of them are inside the global query rate and the outstanding cap, which are unchanged.
  - **No single node receives more than today.** A slow candidate is never asked again by its leg (it isn't fresh), and the same node in another lookup is asked exactly as today.
  - `ALPHA`, the fresh concurrency, is still 3.
- **Lookups keep within 3/4 of the outstanding cap (runner, 2026-11-23, §2.26; flagged §2.19).** A soft query frees a lane for one more query. So a query goes soft only while all lookups' queries out stay under 3/4 of the live preset's outstanding cap. That count includes soft and detached queries, and the maintenance lookups' queries. It also counts, per request, the lanes earlier soft queries freed that a throttled drive hasn't refilled yet (up to that request's soft queries out), since each is a query promised. Past that, the query keeps its lane exactly as with the switch off. The per-leg limit of 3 slow queries stays.
  - **Why:** with CR-1c on, Low has 32 slots and a 256 cap. At 9 per request with two families, lookups alone could hold 288 queries. The actor would still enforce the cap, but lookups could fill it and crowd out sampling, which stops at 4/5 of the cap, and maintenance. CR-1c's own rule (`docs/decisions/2026-11-21-CR-1c.md`) keeps slots × `ALPHA` × 2 families within 3/4 of the cap. This applies the same line to soft queries.
  - **When it binds:** Low with CR-1c on, and Balanced and Aggressive with CR-1c on and two families. With CR-1c off, 32 slots × 9 = 288, under 3/4 of Balanced's 1,024 (768), so it never binds there. Nor does it in the CR-SIM screen, which runs Balanced with one family.
  - **Tests:**
    - `soft_expiry_stops_at_three_quarters_of_the_cap` puts lookups within 10 of the line at Low with CR-1c on, and checks that exactly 10 go soft and that sampling still sends in the same tick. With the check removed, all 96 go soft and the test fails (mutation run on this branch).
    - `the_three_quarters_rule_counts_lanes_refilled_after_timeouts` makes half the slots' queries reach the query timeout in the tick where the other half's are due to go soft, with detached queries holding lookups at the line. Lookups stay within the line after every tick. With soft expiry run before the timeouts' drives (this branch's first order, which the self-review caught), they reach 234 against 192.
    - Two paused-clock runs, at Low with CR-1c on and at Balanced with CR-1c off, every slot full of silent candidates and the pool refilling, check that sampling and a maintenance lookup still send. They also check that requests' and detached queries never pass the line, and that lookups with the maintenance lookups stay under the cap. The maintenance lookups send apart from soft expiry, into the room the rule keeps (at Low, up to 48 more). On their own, these two runs don't catch the first mutation: the table's failed nodes leave later requests short of candidates.
- **Every other bound is unchanged:**
  - the 64-query per-request cap (every send counts, soft or not);
  - `LOOKUP_DEADLINE`, `query_timeout`, the query rate and the outstanding cap.
  - A freed lane's next query goes through `try_query` like any other, so it waits when the rate or the cap is spent.
- **S8-08 (low): a slow honest closest node no longer anchors the closest set.** With it slow, ids that answer at once near the target can end a leg sooner than they could with the switch off.
  - **Accepted, because** lookups already accept fast answers under CR-LOOKUP-END, whose threat model this stays inside, and the 64-query cap and the query rate still bound what is sent.
  - A slow candidate never counts as answered, and while one is out a leg needs `K` answers to end, so unreachable ids alone can't end a leg. `dht.lookup_end`'s two-network rule still applies to the candidates that did answer.
  - The most such ids can cost is one soft-expired query each, within the 64-query cap, the rate, and the 3-slow bound.
  - A candidate whose query is out (in flight or slow) is never dropped when closer candidates crowd the list, so a late answer always finds it. The list stays within `CAP + 2 · ALPHA`.
- **Memory:** at most 3 extra pending entries per leg beyond today's, plus one counter per lookup and one entry in the actor's soft-query set per soft query. Detached entries are bounded by the outstanding cap and expire at `query_timeout`.

## The value: 1 s, not 2 s
- **Where it comes from: the calibrated CR-SIM scenario** (`live-lookup`, X-crawler-sim-calibrate).
  - Every real reply there arrives within 460 ms, except the 15 % that are slow at 3 s. Stale contacts never answer, and they are what hold `W` near 24 s.
  - 1 s covers every fast reply with at least 2× margin. The 3 s replies become late replies, which exercises the late path in the screen.
- **CR-LEDGER has no time-to-answer series,** only each request's time to first values. So it couldn't place the value. The live calibration signal is the ratio of the two new counters, `late ÷ soft`: the cloud lane's mechanism check reads it beside `W`.
- **The runner's choice rule** (2026-11-23): keep 1 s if it passes the standing rule and its `late ÷ soft` isn't much worse than 2 s; otherwise use 2 s. 1 s passed on every seed, with `late ÷ soft` 0.134 against 0.133 (`lookup_end` off) and 0.147 against 0.144 (on). So 1 s stands. (These are the final code's figures, after the self-review's fixes; the first build's were 0.142 and 0.157, with smaller gains.)
  - **A screen-only knob** (`Config::soft_timeout_after`, default `SOFT_TIMEOUT`) lets CR-SIM run the 2 s variant. The app has no setting for it and always runs 1 s.

## The screen (calibrated `live-lookup`, 8 seeds, off against on)
Build-plan §0.5's standing rule: keep only if stored per slot-second and stored per DHT query both rise (item 32's CR-1c rule doesn't apply). Means over seeds 1–8:

| `lookup_end` | arm | `W` (Little) s | `q` | `y` | stored/min | stored/slot-s | stored/query | late ÷ soft |
|---|---|---|---|---|---|---|---|---|
| off | off | 23.11 | 24.4 | 0.164 | 13.13 | 0.0071 | 0.0028 | — |
| off | on, 1 s | 12.72 | 26.2 | 0.184 | 26.76 | 0.0145 | 0.0041 | 0.134 |
| off | on, 2 s | 15.35 | 26.0 | 0.185 | 22.35 | 0.0120 | 0.0038 | 0.133 |
| on | off | 16.20 | 19.4 | 0.164 | 18.64 | 0.0101 | 0.0038 | — |
| on | on, 1 s | 8.08 | 19.9 | 0.177 | 40.10 | 0.0219 | 0.0055 | 0.147 |
| on | on, 2 s | 9.88 | 19.7 | 0.177 | 33.24 | 0.0179 | 0.0052 | 0.144 |

- **Verdict:** in every arm, stored per minute is Better on 8/8 seeds, and stored per slot-second and stored per query each rose on 8/8.
- **What moved:** `W` falls by 45–50 % at 1 s (34–39 % at 2 s). `q` rises by under 8 %: the successors asked while a slow candidate was still out. `y` rises too. A likely reason is that more legs reach their closest set before the deadline, but the screen doesn't separate the causes.
- **What a screen can't show:** it never replaces a live run. The simulator is IPv4-only, with approximate routing and no churn, and its latency model is two-valued. Live RTTs are spread, so `late ÷ soft` there will differ, and that is what the cloud mechanism check reads.
- Reproduce with `cargo test -p lodestar-crawler-sim --test sim soft_timeout_screen -- --ignored --nocapture`.

## Tests
- **`crates/dht/src/node/soft_timeout_tests.rs`,** against the real actor on tokio's paused clock, with all four `soft_timeout` × `lookup_end` combinations where they apply:
  - off, the same script waits for the query timeout and nothing is allocated;
  - a silent node stops holding the leg;
  - freed lanes ask the next candidates, within the 3-slow and 6-out bound, with nobody asked twice;
  - a late answer feeds a running leg and settles once;
  - a slow query fails once at the timeout;
  - each permit comes back exactly once on every end (closest set, no values, deadline, cancel, shutdown), and the soft queries are detached, not dropped;
  - the 64-query cap and the deadline hold against endless closer ids, and a request that spends its 64 queries with soft ones out ends on the cap with them detached;
  - a leg whose only candidates left are slow waits for them, and their late answers feed it;
  - the values end (`dht.lookup_end`) with soft queries out detaches them and returns the permit once;
  - a late KRPC error fails a slow query once;
  - refresh and scrape lookups never go soft;
  - the 3/4 rule, alone and when timeouts and soft expiries share a tick;
  - a freed lane waits for the rate and for the outstanding cap;
  - a detached answer never reaches a lookup that reused its id.
- **Property tests:**
  - a scripted network of 80 fast, late and silent nodes (so the 64-query cap is reachable): with the switch on, a leg never ends earlier than the same script with every late node answering on time, except by a closest set of K answers or its values; a leg never ends with slow queries out and fewer than K answers; and the bookkeeping holds at every step. Putting the first build's end rule back fails it, and two other tests;
  - hostile replies to soft queries (spoofed, repeated, to detached queries, with ticks between) settle nothing twice, end nothing more than once and stay bounded;
  - in `lookup.rs`, any sequence of adds, sends, soft expiries, answers, failures and retries keeps the counters equal to the states and within `ALPHA`.
- **CR-SIM:** `soft_timeout_runs_deterministically` (not ignored) and the `#[ignore]` screen above.

## The fleet and collector (runner, 2026-11-23: owns extended for these lines)
- **`LODESTAR_DHT_SOFT_TIMEOUT` is a switch the collector records.** It's added to `SWITCH_ENV` in `docs/soak/tools/linux/collect.py` as `soft_timeout`, recorded and off unless set, with no treatment yet (as CR-SAMPLE-BACKOFF-ESCALATE was in #1029).
- **The fleet's VM stack passes it through by bare name** (`infra/fleet/steps/vm/compose.yaml`), so a cloud screen or an A/B block can set it and one that doesn't leaves it at its default.
- **Why:** X-fleet-switch-passthrough's test requires every `LODESTAR_DHT_*` variable `config.rs` reads to be one or the other, so a new switch can't be missed. Without these lines, CI's `infra` job fails on this PR.
- **The analyzer needed no change.** `analyze_cr.py` derives its switch lists from `SWITCH_ENV`, and its tests pass with the new entry.
