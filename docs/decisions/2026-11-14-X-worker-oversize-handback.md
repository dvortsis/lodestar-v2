# X-worker-oversize-handback decisions

This builds on X-fetch-room-fix (`2026-11-14-X-fetch-room-fix.md`), which held a worker's largest dict to just under 3 MiB and left larger ones to this slice. Architecture §19 (workers and the coordinator). This file says only what the code now does.

## Calls
- **A key too large for a remote worker goes to the built-in worker instead of being lost.** A worker still reports it as it always has: a `Lost` outcome with the oversize reason, under its lease's token. No wire change and no new protocol version.
  - The coordinator's server routes that report to `LeaseAuthority::hand_to_local`. Under one lock, if the worker holds the key under that token, the lease is granted to the built-in worker under a new token. The key keeps its cohort, sources and strategy, and it isn't a loss or a new harvest.
  - The server's sink then starts the built-in worker's fetch (`RemoteIntake::take_handed`), as it does for a parked key: refused first if the built-in worker refuses it (held, or blocked by the filter), which ends its lease as blocked; otherwise into the built-in worker's pool, keeping its privacy (§18.7).
  - **If the pool is full,** or no sink takes it, the key waits for a re-grant instead, so a lease is never stranded.
  - The built-in worker fetches up to its own limit, 47 MiB. A key too large there too is lost as `oversize`, once.
- **Each key is handed back once** (`HANDED_BACK`, 16,384 keys remembered, the oldest forgotten first). A key handed back before, found again and too large for a worker again, is lost as before, so no key goes round. The built-in worker's own oversize report is always a loss.
- **Counted** as `cluster_handed_back_total` (`docs/metrics.md`).
- **What's left:** peers in three networks agreeing on a false "too large" (the size rule) can move a key from a worker to the built-in worker, at one fetch each. That's bounded by the size rule and by the once-only memory.

## Tests
- **Lease authority** (`leases.rs`): an oversize key is handed to the built-in worker once, only by its holder under its current token, keeping its cohort and sources, not as a loss, and a second time it's a loss; the keys handed back are bounded.
- **Server** (`tests.rs`): a remote worker's oversize report hands the key to the built-in worker through the sink, counted as handed back, not as an outcome; if the sink can't take it, the key waits for a re-grant.
- **Intake** (`tests/remote_intake.rs`): keys handed back reach the built-in worker's pool with their privacy kept; one it refuses ends as blocked; one that finds the pool full is left for the server to requeue. The parked-key test, which now shares the same path, passes unchanged.
