# CR-LOOKUP-END decisions

CR-LOOKUP-END is r5 "Path to 100" item 3 (`docs/build-plan.md` §0.5), from the review's blind spot 1 (`docs/reviews/2026-11-20-path-to-100-review.md` §3). It sits behind `dht.lookup_end`, or `LODESTAR_DHT_LOOKUP_END`, and is **off by default**. With the switch off, lookups end and start exactly as before.

## What changes with the switch on
- **End on values.** A peer request ends, and frees its slot, as soon as it holds `ENOUGH_VALUES` = **8** distinct, validated values that came from replies of at least `VALUE_RESPONDERS` = **2** distinct networks.
  - It ends every family leg at once. Queries still out are dropped, as they are on a cancel.
  - The caller gets the values, a final reply or a stream's `complete`, exactly as a converged lookup would give them.
  - The ledger counts it as `dht_lookup_ended_total{reason="enough_values"}`. That is a new value in the closed enum. Precedence: `cancelled`, `deadline`, `enough_values`, `query_cap`, then `values` / `no_values`.
- **End on the closest set.** A leg ends once the `K` (8) closest candidates that haven't failed have all answered. Candidates are kept in distance order, so no closer node is left to ask (`Lookup::closest_answered`).
  - **What's new:** the leg doesn't wait for queries still out to nodes outside that set, or for a hint or router query outside the candidate list.
  - **What's unchanged:** today's end (`Lookup::is_done`: nothing in flight and the 8 closest live candidates answered) still ends it too.
  - **The guard:** a leg in which nobody has answered never converges while something is still out, so an empty table can't drop a hint that's in flight. The ledger counts this end as `values` or `no_values`, as before.
- **Start on admission.** After each command the actor starts the queued requests at once (`start_admitted`), instead of on the next 1 s tick. The rules on the tick still apply: the pause, the bootstrap parking and the 32 slots.
  - Each start's queries still go through the query rate and the outstanding cap (`admit_query`). A throttled query waits for a later drive, as before. A test sends one request with an empty bucket and one with a full cap; neither sends anything over the limit.

## The constants
- **`ENOUGH_VALUES` = 8.** It's a candidate default (§2.19), to be tuned from `dht_lookup_values` once CR-AB-LC has run.
  - **What a fetch uses** (CR-0, `docs/build-plan.md` §0.5): about 4.7 metadata attempts a second for 1.286 admitted lookups a second, so about 3.7 fetch attempts per lookup.
  - **Why 8:** it's about twice that, which leaves room for the dead and NATed peers that make up most failures (H3), yet it's reached from one or two replies (a reply lists up to 16).
  - **Where it can fall short:** the pool holds up to 32 peers per key (`max_peers_per_key`). A key that runs out of peers before a fetch succeeds is re-looked-up later under the pool's existing retry rules, so a low value costs a retry, not the hash.
  - **What's unchanged:** the caps on values (16 kept per reply, 64 per leg and per request) are as before.
- **`VALUE_RESPONDERS` = 2,** counted as distinct networks (/24, IPv6 /64, the actor's existing `network()`), not distinct addresses.
  - **The hostile-reply rule:** one responder can't end a lookup on its own, however many addresses or ports it answers from within one network.
  - **What a hostile node can do:** with values from a single network, a request runs on as it does today, to its closest set, the deadline or the query cap. That's no regression.
  - **Which values count:** only values that pass the existing endpoint policy (no port 0, no martian, private or blocklisted address in production) and are de-duplicated. They also have to arrive in a reply matched to a query still awaiting it, from the address asked; spoofed, late and unknown-tid replies count for nothing.
  - **Memory:** the per-request state this adds is at most 2 network keys and a flag.

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
  - the closest set ends a leg with farther queries still out;
  - an admitted request starts at once under the rate and the cap.
  - Each is run with the switch off too, showing today's behaviour.
- **Property tests** (`proptest`, 64 cases each) on hostile reply scripts: empty, huge (up to 200), duplicate-heavy, port-0 and arbitrary values; spoofed replies from another network; unknown transaction ids. None ends a request on values, values stay at most 64, the responder list stays at one network, and the request ends at most once. Spoofed and unknown replies alone leave the request with no values.
- **A `Lookup` unit test** for `closest_answered`.
- **With the switch off,** the existing dht suite passes unchanged.

## Screen (CR-SIM)
Pending. `crates/crawler-sim/**` is X-crawler-sim-calibrate's (#974) until it merges. Then the calibrated scenario runs 8 seeds, off against on, reporting `W`, `q` and `y` beside stored per minute. The change is kept only if output per slot-second and output per query both rise. The numbers go here.

## New file
`crates/dht/src/node/lookup_end_tests.rs` holds the actor tests, following `hint_tests.rs` and `ledger_tests.rs`. It isn't in the brief's owns; the runner was told.
