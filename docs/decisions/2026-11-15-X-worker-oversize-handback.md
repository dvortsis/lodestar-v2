# X-worker-oversize-handback decisions

**Where it came from:** the build was parked on `parked/X-worker-oversize-handback` (be79123a, 2026-11-14Z). The runner relaunched it as this slice on 2026-11-15Z: the parked commit, carried over to current `main`, plus the refusal-before-grant change and the tests the runner asked for (below).

This builds on X-fetch-room-fix (`2026-11-14-X-fetch-room-fix.md`), which held a worker's largest dict to just under 3 MiB and left larger ones to this slice. Architecture §19 (workers and the coordinator). This file says only what the code now does.

## Calls
- **A key too large for a remote worker goes to the built-in worker instead of being lost.** A worker still reports it as it always has: a `Lost` outcome with the oversize reason, under its lease's token. No wire change and no new protocol version.
  - **The refusal check comes first** (runner). Before any grant, the server asks its sink whether the built-in worker refuses the key (`InboundSink::refuses_local`: held, or blocked by the filter, the same check its own intake runs). A refused key ends there as a blocked loss, remembered as any blocked loss is (`LeaseAuthority::report`). It is never granted to the built-in worker or put in its pool. In the parked build the grant came first and the refusal only afterwards, though still before any fetch.
  - Otherwise the coordinator's server routes that report to `LeaseAuthority::hand_to_local`. Under one lock, if the worker holds the key under that token, the lease is granted to the built-in worker under a new token. The key keeps its cohort, sources and strategy, and it isn't a loss or a new harvest.
  - The server's sink then starts the built-in worker's fetch (`RemoteIntake::take_handed`), as it does for a parked key: into the built-in worker's pool, keeping its privacy (§18.7). `take_handed` checks the refusals again, for a key whose hold or filter verdict changed in the moment between, and ends that lease as blocked.
  - **If the pool is full,** or no sink takes it, the key waits for a re-grant instead, so a lease is never stranded.
  - The built-in worker fetches up to its own limit, 47 MiB. A key too large there too is lost as `oversize`, once.
  - **A worker's report is trusted for nothing but "hand it over".** It carries no size, and the key reaches the built-in worker's pool as a bare key built from the coordinator's own lease (source, strategy, privacy), with no peers, claims or provenance from the worker. So a worker that lies can't lend the built-in worker a bound or skip it. All it can do is move a key it holds to the built-in worker, once per key, at the cost of that worker's own lease.
- **Each key is handed back once** (`HANDED_BACK`, 16,384 keys remembered, the oldest forgotten first). A key handed back before, found again and too large for a worker again, is lost as before, so no key goes round. The built-in worker's own oversize report is always a loss.
- **Counted** as `cluster_handed_back_total` (`docs/metrics.md`).
- **What's left:** a lying worker can spend one built-in fetch per key it holds (see above). Peers in three networks agreeing on a false "too large" (the size rule) can move a key from a worker to the built-in worker, at one fetch each. That's bounded by the size rule and by the once-only memory.

## Ride-along
- `leases.rs`: the parked commit had put `HandBack` between `LeaseAuthority`'s doc comment and the struct. The doc is back on `LeaseAuthority`.

## Tests
- **Lease authority** (`leases.rs`): an oversize key is handed to the built-in worker once, only by its holder under its current token, keeping its cohort and sources, not as a loss, and a second time it's a loss; the keys handed back are bounded.
- **Server** (`tests.rs`):
  - A remote worker's oversize report hands the key to the built-in worker through the sink, counted as handed back, not as an outcome. If the sink can't take it, the key waits for a re-grant on the built-in worker's lease. A repeat of the same report is stale.
  - A second hand-back of a key, claimed again by another worker after the built-in worker's fetch ended, counts as a loss and isn't handed over again.
  - An oversize claim for a key the built-in worker refuses ends as blocked: it isn't handed over, and isn't granted to the built-in worker or left waiting.
- **Intake** (`tests/remote_intake.rs`): keys handed back reach the built-in worker's pool with their privacy kept; one it refuses ends as blocked; one that finds the pool full is left for the server to requeue. A key handed back carries nothing from the report: the pool gets exactly `Found::key` with the lease's own source and strategy. `refuses_local` agrees with the intake's refusals. The parked-key test, which now shares the same path, passes unchanged.
