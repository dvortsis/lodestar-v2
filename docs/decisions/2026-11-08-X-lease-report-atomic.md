# X-lease-report-atomic decisions

This closes WK-10d's residual window between a worker's floor report and the refusal of its key (#607's reviews, L3/NB1, review item 7). Brief: runner, 2026-11-08T23:15Z. WK-10d's own decisions file stays as it was; this file says the window is closed.

## The window
- `LeaseAuthority::report` ended the lease, and only afterwards did the server hand the outcome to the sink, which puts the key in its floor-report map. The claim check reads that map.
- A claim of the same key from another worker could land in between, pass the store check (the map and the store didn't know the key yet) and be granted. That cost one extra fetch, which the next worker's floor dropped (WK-10d, "A residual window between the report and the map").

## The fix
- **A `Blocked` loss is remembered as a commit, under the authority's lock** (`crates/cluster/src/leases.rs`). When `report` ends a lease with the loss `LOSS_BLOCKED` (7, `funnel::Loss::Blocked`), it calls `remember_commit` for the key in the same critical section that releases the lease. This is the fix WK-10d's decisions named.
- **From the moment `report` returns, a remote claim of the key is refused as stored:** `claim_batch` answers `Committed`, and the server denies the key as `Stored`. So is a late result's lease (`restore_if_free`).
- **That holds for a claim already in flight.** Its store check ran before the report, so its `checked_at` is older, and the report counts as a commit since.
  - If more than `RECENT_COMMITS` commits and floor drops happen meanwhile, the claim is refused as busy anyway (the existing forgotten-window rule).
  - The memory lasts about ten seconds at the §19.8 ingest rate. The sink fills its map right after `report`, on the same task, and the map then holds the key until the writer's tombstone is in the store. So the store, the map and the authority now overlap with no gap.
- **Only `Blocked` is remembered.** Any other loss (peer found, no peers, timeout and so on) leaves the key free to claim again, as before: the DHT should find it and someone may fetch it.
- **`LOSS_BLOCKED` moved from `server.rs` to `leases.rs`** (`pub(crate)`), so both use one constant. The re-grant path's `KeyCheck::Held` report, which already used it, is now remembered too. That's harmless: the key is held in the store.

## What it doesn't change
- **The built-in worker** claims through `LeaseAuthority::claim`, which never consults the recent commits, for commits as for reports. It was already covered by WK-10d: the sink puts the reported key in its `HeldSet`, which its intake, its parked keys and its commit refuse. A built-in claim racing that is refused at commit, so nothing is stored. The window this slice closes was the remote one.
- `InboundSink`, the wire protocol and the store are unchanged.

## Tests
- `a_blocked_report_refuses_the_key_at_once` (`leases.rs`): after a blocked report, another worker's `claim_batch` is `Committed` and a late lease is refused. A key lost for another reason is granted. A stale report remembers nothing.
- `a_claim_waiting_during_a_blocked_report_is_refused` (`leases.rs`, deterministic): two threads interleaved by barriers. The claim's store check runs first, then the report, then the claim, which is refused.
- `a_claim_racing_a_blocked_report_never_gets_the_key` (`leases.rs`, 200 rounds): the report and the claim start together from a barrier. Whichever takes the lock first, the claimer never gets a lease: it finds the key held, or refused.
- `a_floor_drop_is_refused_to_the_next_claim` (`crates/cluster/tests/transport.rs`): end to end over real sessions, with no store check or sink. Worker A reports `Lost { reason: 7 }`, and worker B's claim is denied as `Stored`. A key lost with reason 1 is granted to B.
- No fixed sleeps. Barriers order the threads, and the transport test waits on a heartbeat's answer.
- **Mutation-checked:** without the `remember_commit` call in `report`, all four fail.
