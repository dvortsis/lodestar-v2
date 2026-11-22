# X-cluster-start-retry decisions (#929)

A cluster port that fails to open is now tried again while "Allow workers" stays on, instead of staying closed until "Allow workers" changes or a reseal comes (architecture §20, the cluster port). This is the runner's call, as UX: a user shouldn't have to toggle workers to recover. The runner flags it under `docs/owner-decisions.md` §2.19. It was found while root-causing the `off_binds` flake (`2026-11-15-X-store-migrate-race.md`).

## Not live yet

- **Nothing in production reaches this path today.** `ClusterHandle::set_allowed` and `reseal` have no production callers yet, so users can't toggle workers. A failed start at boot, with `[cluster] enabled` on, is still fatal (`services.rs`).
- **The retry goes live** once WK-14's workers API wires `set_allowed`, or secrets-key rotation wires `reseal`. The runner adds that dependency to WK-14's notes.
- **Until then** it's exercised by this slice's tests, and `start_failure` and the Overview row stay empty in practice.

## The retry

- **When:** a start that fails while workers are allowed, on the toggle or after a reseal. It's tried again after 2 s, then twice as long each time, up to 5 minutes. The bound keeps a port another program holds from being tried in a tight loop, and 5 minutes is the longest a fixed cause (a port freed, a disk remounted) waits to be noticed.
- **Cancelled** when workers are turned off. **Cleared** when a start succeeds, so the next failure starts again at 2 s, and when the cluster shuts down.
- **Only a change wakes it.** `set_allowed` uses `send_if_modified`, so a settings save that leaves "Allow workers" as it was neither tries a failing start at once nor resets its back-off, once WK-14 wires it.
- **Unchanged at startup:** with `[cluster] enabled` on, `Cluster::start` still returns the error and serve doesn't start, so a broken config is caught at launch rather than retried in the background.

## What it says

- **Status reaches the user, not only the operator (runner, 2026-11-15: options A and B together).** The owner asked for UX-led decisions, and a user won't read a metrics gauge. So the failure is in the workers API for Settings → Workers to show, as well as in a gauge.
  - **`GET /workers`** now always carries `start_failure`: null when nothing fails, else `{step, kind, next_retry_at}`. While it's set, `allowed` is false, since `allowed` means the port is serving. The field's doc says so, rather than `allowed` changing meaning (the runner's review left the choice open).
    - `step` and `kind` are fixed enum labels, the same ones the log line uses. They're never error text, an address, a path or key material.
    - `next_retry_at` is in unix seconds.
    - `openapi.json` and `schema.d.ts` are regenerated, not hand-edited.
  - **Overview → Needs attention shows it now (runner, 2026-11-15).** The Workers screen (WK-16) isn't built yet, and Needs attention is where a problem belongs.
    - **The row says what to do (the runner's review).** For example: "Workers can't start yet: another program is using the cluster port. Close that program, or set another port under [cluster] in config.toml. Trying again in 2 minutes."
    - **Where the fix is the user's** (the port in use, an address that isn't this computer's, no permission), the row names the change in `config.toml`, since Settings has no cluster port field yet, and has no link.
    - **For the other reasons** (the port otherwise, the database, the secrets key, the workers' certificates, the workers' server), it says the logs say why and links to them with the existing "Open the logs" link, so it isn't a dead end.
    - **It's a warning,** and it goes away on the next load after the port opens, or once workers are turned off.
    - **Overview reads `GET /workers` once,** alongside `/system` and the network check, and best-effort: a failure leaves the other rows. It doesn't poll.
    - **The retry time is worked out in the screen's own helper (`retryIn`)**, not `formatRelative`. Importing `formatRelative` into Overview grew six routes' own chunks by about 110 B each, through how the shared format chunk splits, and four of them went over their budgets. The wait is at most 5 minutes, so whole minutes or seconds is enough.
    - **The e2e mock** answers `listWorkers` as a single machine by default (`web/e2e/support/test.ts`, a shared one-line edit), so the other settings specs aren't failed for an unmocked call.
    - **WK-16 shows `start_failure` on the Workers screen later,** as one `role=status` line. The runner adds that note to WK-16's entry.
  - **`cluster_start_failing`** is a new gauge: 1 while workers are allowed and the port is being tried again.
  - **Both clear** when a start succeeds or workers are turned off.
- **The log:** `cluster.start_failed` is logged at the first failure of a run, and again only when the step (`bind`, `store`, `secrets`, `pki`, `pauses`, `server`) or the error kind changes. The tries in between aren't logged, so a port held for a day doesn't log 300 errors. `cluster.listening` marks the recovery. The event's doc and its English and Chinese help say so.
- **Metrics:** `cluster_start_failures_total` counts every try, retries included. `cluster_listening 0` with `cluster_allowed 1` is the failing state, as `docs/metrics.md` already said; its row now says the port is tried again.

## Typed labels

- **`StartError` carries the API's enums** (`ClusterStartStep`, `ClusterStartKind`) instead of strings. So the log, the API and the error can't drift apart. A test checks that each enum's JSON label equals its `as_str()`.

## Tests

- `a_taken_port_fails_the_start_and_creates_nothing` now holds the port through a second try, with nothing toggled, then frees it and waits for the port to open. Without the retry it fails at "a second try".
- `a_failed_start_backs_off_and_logs_a_new_cause_only` checks the waits (2, 4, 8 … 256, then 300 s) and which failures are logged.
- **`a_taken_port_fails_the_start_and_creates_nothing` also checks what's reported while failing:** the gauge, and `start_failure` with `bind` / `addr_in_use` and a retry time within 5 minutes. After recovery it checks that both clear.
- **`turning_workers_off_stops_the_retry`:** turning workers off clears both, and turning them back on tries again at once.
- **`start_failure_labels_match_the_log_and_are_null_when_fine`** in the API tests checks the JSON shape, null included.
- **Overview, in `logic.test.ts`:** the row when failing, none once recovered or with workers off, its order among the other rows, the plain-words reasons and `retryIn`.
- **Overview, in `settings-overview.keyboard.test.ts`:** the row's text with no link, axe passing, and the row gone after a reload once the port is open.
