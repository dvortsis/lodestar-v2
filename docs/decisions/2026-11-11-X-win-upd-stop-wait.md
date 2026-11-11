# X-win-upd-stop-wait decisions: the service's stop waits for the updates task

Follow-up to #693 (X-win-upd-window), from the runner's delta review at 5e260f33. The code is `stop_updates` in `crates/lodestar/src/services.rs`, called from `Handles::shutdown`. Calls here are the runner's under §2.19; the owner can veto them.

## The wait (Windows)
- **The stop is sent, then the updates task gets up to 2 s (`UPDATES_STOP_WAIT`) to end by itself, and only then is it aborted.** Before, it was aborted straight away. In the reviewer's experiment, that lost a switch to Off made just before the stop in 80 of 120 runs: the watcher hadn't run yet.
- **The abort stays,** for a scheduled check mid-download that never sees the stop. It's abandoned as before, which is safe: it writes only after it's verified, by rename.
- **The budget:**
  - `SHUTDOWN_GRACE` (10 s, open connections) + `UPDATES_STOP_WAIT` (2 s) + `RUNTIME_GRACE` (5 s, blocking work at exit) is 17 s, plus the pipeline's and indexer's saves.
  - The service tells the SCM `STOP_WAIT_HINT` (30 s), and reports progress every 5 s, so the SCM never gives up on a stop inside that.
  - The watcher's last look is one file removal, retried for about a second. 2 s covers it with room, and costs a stop nothing when the task ends at once, as it does when no check is under way.

## Not on Unix
- **On Unix the wait is zero, and the task is aborted without being run again,** as before. The Unix engine's task runs installs itself. One allowed to run on at a stop could reach its switch: it would set the Switch intent and ask for a restart, so a `docker stop` would come back up on the new version. Unix has no last work at a stop that needs the wait.
- A test pins this: with no wait, the task is never run again.

## Only the updates task
- **The other handles in `Handles::shutdown` already wait for their tasks to end, and none is aborted:** the storage sampler, the subscriptions refresher, the torrent cache, the pipeline and the companions (each companion's stop is bounded). None needs the pattern.

## Checks
- **`the_service_stop_waits_for_the_last_look`** (check.rs) goes through `stop_updates`, the stop's own code: Off, then the stop at once, then the request must be gone. It runs 20 times. With the old abort-first order it failed 5 runs out of 5.
- **Mutation-checked, all killed:**
  - dropping the zero-wait guard (the Unix test fails);
  - aborting at once whatever the wait (the stop-waits tests fail);
  - never aborting (the ignored-stop test fails at its 20 s deadline, and doesn't hang).
- No fixed sleeps: the tests wait on conditions or the task itself, with deadlines of at least 10 s. The ignored-stop test's 50 ms is the bound under test, not a wait for a condition.
