# CR-LOOKUP-END decisions

CR-LOOKUP-END is r5 "Path to 100" item 3 (`docs/build-plan.md` §0.5), from the review's blind spot 1 (`docs/reviews/2026-11-20-path-to-100-review.md` §3). It sits behind `dht.lookup_end`, or `LODESTAR_DHT_LOOKUP_END`, and is **off by default**. With the switch off, lookups end and start exactly as before.

## What changes with the switch on
- **End on values.** A peer request ends, and frees its slot, as soon as it holds `ENOUGH_VALUES` = **8** distinct, validated values, and replies from at least `VALUE_RESPONDERS` = **2** distinct networks have each added a value it didn't already hold.
  - It ends every family leg at once. Queries still out are dropped, as they are on a cancel.
  - The caller gets the values, a final reply or a stream's `complete`, exactly as a converged lookup would give them.
  - The ledger counts it as `dht_lookup_ended_total{reason="enough_values"}`. That is a new value in the closed enum. Precedence: `cancelled`, `deadline`, `enough_values`, `query_cap`, then `values` / `no_values`.
- **End on the closest set.** A leg ends once three things hold (`Lookup::closest_answered`):
  - the `K` (8) closest candidates that haven't failed have all answered. Candidates are kept in distance order, so no closer node is left to ask;
  - their answers came from at least `VALUE_RESPONDERS` (2) distinct networks;
  - no query outside the candidate list is still out (a hint or a router query).
  - **What's new:** the leg doesn't wait for queries still out to candidates outside that set.
  - **Why it waits for a hint:** a hint is at most one per leg and bounded by the 4 s query timeout, and it's often the leg's best source of values. Ending without it would also count it as `unanswered` in the ledger, though it was sent.
  - **Why the network rule:** without it, one operator could list 8 made-up ids just next to the target, all at its own address, answer for them at once, and end the leg early. That would drop the honest queries still out. With one network in the set, the leg waits as it does today.
  - **What's unchanged:** today's end (`Lookup::is_done`: nothing in flight and the 8 closest live candidates answered) still ends it too, whatever the networks. The ledger counts a closest-set end as `values` or `no_values`, as before.
- **Start on admission.** After each command the actor starts the queued requests at once (`start_admitted`), instead of on the next 1 s tick. The rules on the tick still apply: the pause, the bootstrap parking and the 32 slots.
  - **A freed slot needs no refill hook.** Admission takes a request only while queued plus running is under 32. So a queued request always has a free slot, unless lookups are paused or parked, and both of those are released on the tick. When a request ends early, its permit goes back to the pool, and the pool's next command starts the next request at once.
  - Each start's queries still go through the query rate and the outstanding cap (`admit_query`). A throttled query waits for a later drive, as before. A test sends one request with an empty bucket and one with a full cap; neither sends anything over the limit.

## The constants
- **`ENOUGH_VALUES` = 8.** It's a candidate default, accepted by the runner and flagged to the owner (§2.19). Tune it from `dht_lookup_values` once CR-AB-LC has run.
  - **What a fetch uses** (CR-0, `docs/build-plan.md` §0.5): about 4.7 metadata attempts a second for 1.286 admitted lookups a second, so about 3.7 fetch attempts per lookup.
  - **Why 8:** it's about twice that, which leaves room for the dead and NATed peers that make up most failures (H3), yet one or two replies can reach it (an honest reply usually lists up to about 16 values; an inbound reply is cut at 64).
  - **Where it can fall short:** the pool holds up to 32 peers per key (`max_peers_per_key`). A key that runs out of peers before a fetch succeeds is re-looked-up later under the pool's existing retry rules, so a low value costs a retry, not the hash.
  - **What's unchanged:** the caps on values are as before: 64 taken from one inbound reply, and 64 per leg and per request.
- **`VALUE_RESPONDERS` = 2,** also accepted by the runner (§2.19), counted as distinct networks (/24, IPv6 /64, the actor's existing `network()`), not distinct addresses.
  - **The hostile-reply rule:** one responder can't end a lookup on its own, however many addresses or ports it answers from within one network.
  - **What a hostile node can do:** with values from a single network, a request runs on as it does today, to its closest set, the deadline or the query cap. That's no regression.
  - **What it can't do:** a second network counts only when its reply adds a value the request didn't already hold, so repeating values already held can't complete the pair. Two cooperating networks can still end a request holding mostly their values. That meets the brief's rule, and it's no worse than today's final reply, which returns whatever values the lookup collected.
  - **Which values count:** only values that pass the existing endpoint policy (no port 0, no martian, private or blocklisted address in production) and are de-duplicated. They also have to arrive in a reply matched to a query still awaiting it, from the address asked; spoofed, late and unknown-tid replies count for nothing.
  - **Memory:** the per-request state this adds is at most 2 network keys and a flag. With the switch on, the request's value set fills as replies arrive instead of at each leg's end, still bounded at 64.
  - **Mixed address families:** the network that counts is the responder's, never its values'. A reply's values count toward `ENOUGH_VALUES` whatever their family (IPv4 and IPv6 peers alike, once validated). The responder that sent them counts once, by its own address: an IPv4 address by its /24, an IPv6 address by its /64, and an IPv4-mapped IPv6 address as its IPv4 /24, so a node can't count twice by answering in both forms. A dual-stack request's two legs share one count, so an IPv4 responder and an IPv6 responder are two networks, even if one operator runs both. That's accepted: it takes two addresses in different families, and a single responder still can't end a lookup on its own.

## What doesn't change, either way
`LOOKUP_DEADLINE` (25 s), the 64-query cap, `ALPHA` (3), the query rate, the outstanding cap and the 32 lookup slots are unchanged.

## Ledger
- `enough_values` is added to `dht_lookup_ended_total` and `dht_lookup_residence_seconds_total`. Its `docs/metrics.md` row and the render test cover it.
- Every early end goes through `finish_lookup`, so each request still ends exactly once. Purposes still sum to `dht_queries_out_total`.
- #975's optional review items are folded in:
  - a test that each `ALL[i] as usize == i`;
  - the slot gauges read (0, 0) when idle;
  - both gauges are zeroed at shutdown.

## Tests
- **Actor tests on tokio's paused clock** (`node/lookup_end_tests.rs`):
  - enough values from two networks end a request before it converges;
  - one network's values never end it;
  - a network counts only for new values;
  - the closest set ends a leg with farther queries still out;
  - one network answering for made-up close ids can't converge a leg early;
  - an admitted request starts at once under the rate and the cap;
  - each new end returns its permit exactly once, as `completed`, for a final reply and for a stream that reads `complete`.
  - The first four with an off arm are run with the switch off too, showing today's behaviour.
- **Property tests** (`proptest`, 64 cases each):
  - **Hostile reply scripts:** empty, huge (up to 200), duplicate-heavy, port-0 and arbitrary values; spoofed replies from another network; unknown transaction ids. None ends a request on values. Values stay at most 64, each with a nonzero port and allowed by the endpoint policy. The responder list stays at one network, and the request ends at most once.
  - **Spoofed and unknown replies alone** leave the request with no values.
  - **Two networks with fewer than 8 distinct valid values,** duplicates and port 0 included, never end on values.
  - The deterministic end-on-values tests are the positive control.
- **`Lookup` unit tests** for `closest_answered`: farther queries, a failed node, a hint out, and one network.
- **Equivalence with the switch off:** the existing dht suite passes unchanged, and the CR-SIM goldens are the packet-level check. When #974 has merged, the screen run shows they still match with the switch off.

## Screen (CR-SIM)
Pending. `crates/crawler-sim/**` is X-crawler-sim-calibrate's (#974) until it merges. Then the calibrated scenario runs 8 seeds, off against on, reporting `W`, `q` and `y` beside stored per minute. The change is kept only if output per slot-second and output per query both rise. The numbers go here.

## Files outside the brief's list
- `crates/dht/src/node/lookup_end_tests.rs` holds the actor tests, following `hint_tests.rs` and `ledger_tests.rs`. The runner accepted it into the owns.
- `crates/lodestar/src/metrics.rs`: one line in the ledger render test, for the new `enough_values` label. The brief asks for the render test to cover it.
