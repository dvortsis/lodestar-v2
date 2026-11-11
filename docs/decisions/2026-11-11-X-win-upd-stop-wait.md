# X-win-upd-stop-wait decisions: the service's stop waits for the updates task

Follow-up to #693 (X-win-upd-window), from the runner's delta review at 5e260f33. The code is `stop_updates` in `crates/lodestar/src/services.rs`, called from `Handles::shutdown`. Calls here are the runner's under §2.19; the owner can veto them.

## The wait (Windows)
- **The stop is sent, then the updates task gets up to 2 s (`UPDATES_STOP_WAIT`) to end by itself, and only then is it aborted.** Before, it was aborted straight away. In the reviewer's experiment, that lost a switch to Off made just before the stop in 80 of 120 runs: the watcher hadn't run yet.
- **The abort stays,** for a scheduled check mid-download that never sees the stop. It's abandoned as before, which is safe: it writes only after it's verified, by rename.
- **The budget:**
  - `SHUTDOWN_GRACE` (10 s, open connections) + `UPDATES_STOP_WAIT` (2 s) + `RUNTIME_GRACE` (5 s, blocking work at exit) is 17 s, plus the pipeline's and indexer's saves.
  - The service tells the SCM `STOP_WAIT_HINT` (30 s), and reports progress every 5 s, so the SCM never gives up on a stop inside that.
  - That's measured against a Stop. At system shutdown (Preshutdown and Shutdown, mapped to the same stop), Windows has a shorter timeout of its own; 17 s was already over it before this change. The 2 s is only spent when a check is mid-download.
  - The watcher's last look is one file removal, retried for about a second. 2 s covers it with room, and costs a stop nothing when the task ends at once, as it does when no check is under way.

## Not on Unix
- **On Unix the wait is zero, and the task is aborted at once,** as before. The Unix engine's task runs installs itself. One allowed to run on at a stop could reach its switch: it would set the Switch intent and ask for a restart, so a `docker stop` would come back up on the new version. Unix has no last work at a stop that needs the wait.
- A test pins that with no wait, a task not yet polled never is.
- **Residual, as before this change:** the production runtime is multi-threaded, and an abort doesn't stop a poll already running on another worker. If the stop lands just as an install's last verification finishes, that poll can run on through `switch`. The window is microseconds. A real fix has the engine check the stop just before `switch`, in the engine (not this slice's).

## Only the updates task
- **The updates task was the only one aborted straight after its stop with last work to do.** The storage sampler, the subscriptions refresher, the torrent cache and the pipeline are waited for. The companions' tasks and the cluster's ticker are aborted by design, inside their own bounded stops, and have no last work at a stop. None needs the pattern.
- **Residual:** the Windows check loop's selects aren't biased, so a stop arriving just as a scheduled check falls due can start that check. It's aborted at the 2 s wait, and harmless, since it writes by rename, but it costs the stop the full 2 s. Biasing those selects to the stop is outside this slice's owns, so it's left to the runner.

## Supersedes
- In `2026-11-11-X-win-upd-window.md`, the runner had this slice update the "At a stop" bullet and the not-yet-polled residual. They now point here: the watcher's last look finishes unless it overruns the wait.

## Checks
- **`the_service_stop_waits_for_the_last_look`** (check.rs) goes through `stop_updates`, the stop's own code: Off, then the stop at once, then the request must be gone. It runs 20 times. With the old abort-first order it failed 5 runs out of 5.
- **Mutation-checked, all killed:**
  - dropping the zero-wait guard (the Unix test fails);
  - aborting at once whatever the wait (the stop-waits tests fail);
  - never aborting (the ignored-stop test fails at its 20 s deadline, and doesn't hang).
- No fixed sleeps: the tests wait on conditions or the task itself, with deadlines of at least 10 s. The ignored-stop test's 50 ms is the bound under test, not a wait for a condition.
