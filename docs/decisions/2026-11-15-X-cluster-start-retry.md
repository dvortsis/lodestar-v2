# X-cluster-start-retry decisions (#PR)

A cluster port that fails to open is now tried again while "Allow workers" stays on. Before, it stayed closed until someone turned workers off and on again, or a key rotation resealed the PKI (architecture §20, the cluster port). This is the runner's call, as UX: the owner shouldn't have to toggle workers to recover. The runner flags it under `docs/owner-decisions.md` §2.19. It was found while root-causing the `off_binds` flake (`2026-11-15-X-store-migrate-race.md`).

## The retry
- **When:** a start that fails while workers are allowed, on the toggle or after a reseal. It's tried again after 2 s, then twice as long each time, up to 5 minutes. The bound keeps a port another program holds from being tried in a tight loop, and 5 minutes is the longest a fixed cause (a port freed, a disk remounted) waits to be noticed.
- **Cancelled** when workers are turned off. **Cleared** when a start succeeds, so the next failure starts again at 2 s.
- **Unchanged at startup:** with `[cluster] enabled` on, `Cluster::start` still returns the error and serve doesn't start, so a broken config is caught at launch rather than retried in the background.

## What it says
- **The log:** `cluster.start_failed` is logged at the first failure of a run, and again only when the step (`bind`, `store`, `secrets`, `pki`, `server`) or the error kind changes. The tries in between aren't logged, so a port held for a day doesn't log 300 errors. `cluster.listening` marks the recovery. The event's doc and its English and Chinese help say so.
- **Metrics:** `cluster_start_failures_total` counts every try, retries included. `cluster_listening 0` with `cluster_allowed 1` is the failing state, as `docs/metrics.md` already said; its row now says the port is tried again.

## Tests
- `a_taken_port_fails_the_start_and_creates_nothing` now holds the port through a second try, with nothing toggled, then frees it and waits for the port to open. Without the retry it fails at "a second try".
- `a_failed_start_backs_off_and_logs_a_new_cause_only` checks the waits (2, 4, 8 … 256, then 300 s) and which failures are logged.
