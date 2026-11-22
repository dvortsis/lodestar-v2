# X-worker-oversize-nits decisions

The optional findings from the runner's delta security review of #939 (X-worker-oversize-handback, `2026-11-15-X-worker-oversize-handback.md`). The runner set the scope (§2.19): items 2 to 4 of its brief, and no change to the wall-clock window. This file says only what the code now does.

## Calls

- **The hand-back allowance map stays small** (`crates/cluster/src/leases.rs`, `hand_backs`). The runner gave the choice of pruning or a comment saying it's bounded by enrolment; this takes pruning.
  - When a worker starts a new window, every entry whose window has lapsed is dropped. A lapsed window counts from zero anyway, so nothing changes for any worker.
  - The map then holds only workers that handed back within the last `HAND_BACK_WINDOW_SECS`, whether they were forgotten or not. It no longer keeps one entry for every worker that ever handed back.
  - **Why pruning rather than a comment:** the enrolment bound grows with every worker enrolled over the coordinator's life, revoked ones included. The window bound doesn't. The cost is one pass over a map of recent hand-backers, once per worker per window.
- **The allowance counts grants, not keys the pool took** (a sentence added to the X-worker-oversize-handback decisions file). A key granted to the built-in worker uses one of the worker's 120 an hour, even when the pool then turns it away (`Full`, or `Refused` in the race window). That's the safe direction. The code is unchanged; only the wording was misleading.
- **A refusal in the race window is remembered as blocked** (`crates/pipeline/src/remote.rs`, `start_local`).
  - **The case:** a key passes the server's check before the hand-back, then its hold or filter verdict changes before the intake takes it, so the intake refuses it.
  - **Before:** the intake ended the lease with `LeaseAuthority::lost(Blocked)`. That didn't remember the key with the recent commits, while the server's own check uses `report(…, LOSS_BLOCKED)`, which does.
  - **Now:** it uses `report(WorkerId::LOCAL, …, LOSS_BLOCKED)`. A remote claim whose store check ran before the refusal is answered as committed, never granted, as after the server's own refusal.
  - **Parked keys:** they share `start_local`, but they're handed to the built-in worker only while remote workers aren't served. The recent-commit memory exists only while they are, so nothing changes for them.
- **No change** to the wall-clock window (runner).

## Tests

- **`leases.rs`:** `hand_backs_keep_to_a_per_worker_allowance` now also checks that a worker whose window lapsed with no hand-back since is dropped from the map when another worker starts a new window.
- **`tests/remote_intake.rs`:** `a_refusal_in_the_race_window_is_remembered_as_blocked`. The hold changes after the server's check, the intake answers `Refused`, the key is never fetched, and a remote claim whose store check ran before the refusal is answered `Committed`. With `lost(Blocked)` put back, it fails at that last assertion.
