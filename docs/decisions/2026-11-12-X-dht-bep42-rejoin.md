# X-dht-bep42-rejoin decisions

From rv-756 N-3, a follow-up named in `2026-11-12-X-dht-lookup-converge.md`. When external-IP votes give the node new BEP 42 ids, `Actor::on_external_ip` rebuilds the routing tables around the new `ids[0]`, but nothing looked up the new id's neighbourhood until the 15-minute stale refresh or a thin-table re-bootstrap. A self-lookup still running for the old id could also plan the join refresh, counting levels against the new id from nodes near the old one. The change is in `crates/dht/src/node/actor.rs` only.

- **One self-lookup per family per id change.** An id change sets a per-family `rejoin` flag, clears `join_planned` and drops a join plan still queued (it targets the old id's levels). The maintenance tick (`Actor::rejoin_tick`) starts one `find_node` self-lookup for the new `ids[0]` per flagged family and clears the flag.
  - They're flags, not a queue, so votes can't pile up. Repeated votes for the same address, an exempt address and ids that already fit all return early, as before, and set nothing.
  - The lookup goes through `start_lookup_as` and the existing limits. It waits while maintenance lookups are at `MAX_LOOKUPS`, rather than being lost.
  - It's marked `bootstrap`, so its end follows the existing rules: it can settle the bootstrap (and release parked requests) if it ends with nodes, and it plans the family's join refresh. The join is planned once for the new id, at the existing pace (`JOIN_LEVELS`, half of `MAX_LOOKUPS`).
- **Only a self-lookup for the current `ids[0]` plans the join.** A self-lookup for a replaced id still settles the bootstrap as before (its nodes are in the rebuilt table, so parking can end), but it no longer plans the join from the old neighbourhood. That keeps the plan to one per id and ends the bootstrap no earlier than before.
- **A self-lookup for the old id still running is left to finish, not cancelled.** It carries the bootstrap round's router queries, and its answers land in the rebuilt table. Cancelling it would throw that away, and early in a start the rebuilt table may be empty: a self-lookup started then would end at once with nobody to ask. So the rejoin waits until that lookup ends (at most `LOOKUP_DEADLINE`) and is seeded from the table it filled.
- **Shards are unchanged.** The new ids still come from `secure_id::virtual_ids_in` in the shard's slots (WK-4). The rejoin only targets `ids[0]`.
- **No change without an id change.** No flag is set, so the tick does nothing, and the extra `done.target == ids[0]` check holds for every self-lookup, because `start_bootstrap` targets `ids[0]`.
- **Cost:** one extra `find_node` lookup per family (about 8–20 queries), plus the join refresh again (about 200–450 queries, as in `2026-11-12-X-dht-lookup-converge.md`), once per change of public address. No message format changes.

## crawler-sim goldens (shared edit, runner-approved)

The sim's node has a public address, so in every scenario it takes a new BEP 42 id once, mid-bootstrap. It now runs the rejoin and a join planned around the new id, instead of a join planned from the old id's neighbourhood. The four goldens in `crates/crawler-sim/tests/sim/golden/` were re-blessed with `CRAWLER_SIM_BLESS=1`.

| Scenario | Min-1 admitted | Steady completed /min | Steady fetched /min | queries_out |
|---|---|---|---|---|
| healthy | 116 → 128 | 271.6 → 266.1 (−2.0%) | 77.6 → 77.9 | 41166 → 40950 |
| high-latency | 106 → 102 | 103.4 → 106.4 (+2.9%) | 28.0 → 32.6 | 27257 → 26182 |
| admission-pressure | 285 → 259 | 284.1 → 280.9 (−1.1%) | 27.9 → 27.0 | 56509 → 55958 |
| restart-honoured | 46 → 45 | 116.6 → 114.8 (−1.5%) | 27.9 → 28.1 | 144975 → 143107 |

- **Why they move.** The join refresh's lookups now target the new id's levels, and the rejoin self-lookup fills the new id's own neighbourhood. That changes which nodes the table holds, and so which nodes later lookups and samples ask. In this deterministic sim, a different query order shifts every later count a little.
  - Minute 1 moves either way, depending on whether the earlier neighbourhood fill helps or delays the first owned lookups.
  - `queries_out` falls by 0.5–4%: with a join counted against the right id and closer neighbours, there are fewer levels to look up and lookups converge sooner.
  - Steady-state completions stay within −2.0% to +2.9%. The runner accepted the −2% on `healthy` as the cost of correct neighbourhood coverage (§2.26).
- **One extra self-lookup per id change, checked.** With a temporary trace (not committed), every sim run printed exactly one id change and exactly one rejoin self-lookup for its one family, in all four golden scenarios and in the sim's other tests. `restart-honoured`'s second run restores ids that already fit, so it issues none.
