# X-win-restart-followups decisions

Follow-ups to PK-win-restart (#605) from its review: the portable successor starts before the 202, the portable launcher's message, a fixed-port relaunch test, and the relaunch flake. Brief: runner, 2026-11-09T02:54Z.

## The portable successor starts before the 202
- **`SystemApi::prepare_restart`** (`crates/api/src/system.rs`) runs before `POST /system/restart` answers. An error is `409 restart_failed`, and nothing stops. By default there's nothing to ready, so other implementations are unchanged.
- **The message is fixed text:** "Lodestar couldn't start the new copy that takes over, so it's still running. Close Lodestar and start it again to restart it." The error kind is in the log (`app.restart_failed`), never in the answer.
- **`Restart::prepare`** (`crates/lodestar/src/system/restart.rs`) runs the portable build's step (start the new copy) for `relaunch` only, once. Later calls, and the `Restart::request` that the answer's drop makes, don't run it again.
  - The service and Unix have nothing to ready, so they behave as before. A paused service is still refused by `restart_unavailable` first.
- **The address timer's revert-and-restart** (`POST /system/address/revert`) still starts the copy in `Restart::request`, after its answer, as before. Its answer reports a saved address change, which a failed copy doesn't undo, so a 409 there would be wrong. A copy that can't start there is logged (`app.restart_failed`), and the old process keeps serving.
- **Documented in the API.** `openapi.json` and `schema.d.ts` were regenerated with `cargo run -p lodestar --bin openapi` and `npm run api:types`.
- **Tests:**
  - `restart_that_cant_get_ready_is_refused` (api; golden `system_restart_failed.txt`): 409, readied once, never restarted.
  - `restart_answers_then_restarts`: readied before the answer, restarted after it.
  - `a_relaunch_starts_its_copy_before_the_answer` (`restart.rs`): a failed copy requests nothing; one copy however often it's readied; other methods have nothing to ready.
  - `a_portable_restart_starts_a_new_copy` (Windows): the 202 and the hand-over.
  - **A real failed spawn isn't simulated on Windows CI.** The only way would be renaming the running `.exe` out from under `current_exe()`, and what Windows reports then couldn't be checked from this session. The fake and the unit test cover that path.

## `Start Lodestar.cmd`
- **The portable build now exits with 80 after a hand-over** (`EXIT_RELAUNCHED` in `restart.rs`), not 0. 80 is the Unix supervisor's `EXIT_RESTART` and the service's code, so "80" means a restart on every platform.
- **The launcher checks it.** On 80 it says "Lodestar restarted and keeps running in this window." and how to stop it, and ends without `pause`. On anything else it says "Lodestar stopped." and pauses, as before.
- **No labels** (an `if`/`else` block instead): `cmd` can miss a `goto` label in a file with Unix line endings, which this file has in the repository.
- **Ctrl+C is unchanged.** The new copy shares the console, so Ctrl+C there still stops it gracefully. The launcher has already ended, so there's no "Terminate batch job?" prompt in the way.
- **Not verified on a Windows desktop here:**
  - that the window stays open while the new copy runs once the launcher has ended (Windows keeps a console while any process is attached);
  - that it closes as soon as the new copy stops, without a pause.
  - Closing the window with its X still kills the new copy without a graceful shutdown, as PK-win-restart recorded. PK-guide-3 should say to use Ctrl+C.

## A fixed-port relaunch test
- **`a_portable_restart_serves_again_on_the_same_port`** (`crates/lodestar/tests/windows_service.rs`) runs the same relaunch with HTTP on a fixed port (a free one picked by the test). The new copy must serve on that same port, so the wait for the old process's exit really releases it.
- **Both tests share `portable_restart`.** They also check the old process's exit code is 80.

## The relaunch flake (`a_portable_restart_starts_a_new_copy`, "no new copy served")
- **Diagnostics first, as asked.** On any failure after the start, the test now prints the console (stderr, which the relaunched copy shares, so its start error shows) and `lodestar.jsonl`.
- **The likely cause.** It couldn't be reproduced here: this session has no Windows, and the failure logs from #636 and #637 have only the assertion. The evidence:
  - The search index takes its root lock non-blocking (`crates/search/src/rebuild.rs` `lock_root`). A busy lock is `Error::Locked`, and the start fails at once. The store doesn't fail that way: SQLite has a busy timeout.
  - tantivy 0.27 releases that lock only by closing the file (`ReleaseLockFile`'s drop just logs). It never unlocks it explicitly.
  - Microsoft's `LockFileEx` documentation says a lock left on a file that's closed, or held by a process that ends, is released by the OS "depending upon available system resources", not at once.
  - The new copy starts when its stdin pipe ends, that is when the old process has exited. On a loaded runner, the index lock can still be held then.
  - **The timing fits.** On #637 the test failed 97 s after it began, so the 90 s wait for the second `app.serving` ran out. A new copy that waited out the 60 s end-of-file deadline would still have served; one that died at start never does.
- **The fix (runner's decision, 2026-11-09T03:00Z, a shared edit to `crates/search/src/rebuild.rs`).** On Windows, `lock_root` tries a busy lock again every 100 ms for up to 10 s (`LOCK_WAIT`, `LOCK_POLL`). After that it's `Error::Locked(root)`, with the same message as before. An I/O error is never retried.
  - **Unix is unchanged:** a closed file's lock is gone at once there, so a busy lock is another running instance, refused at once.
  - **The loop is `retry_busy`.** It takes the attempt, the wait, the clock and the sleep, so it's tested on every platform on a fake clock, with no waiting:
    - busy then free: taken;
    - busy past the wait: refused;
    - no wait: one attempt;
    - an I/O error: returned at once.
  - `the_index_root_is_locked_once` checks the real lock.
  - Mutation-checked: with the retry disabled, the first two fail.
- **What it costs:** a genuine second instance on the same data folder on Windows is now refused after up to 10 s instead of at once.
- **What it covers:** any quick restart on Windows, not only the portable relaunch: a service restart, or a user who closes Lodestar and starts it again at once.
- **Every caller can wait 10 s.** `lock_root` has one caller, `Serving::open_or_rebuild`, which takes the lock once when the index is opened. A rebuild swaps versions under that same lock and never takes it again.
  - In production, that's `Services::start` (`crates/lodestar/src/services.rs`), on a `spawn_blocking` thread during start, before anything serves.
  - The other callers are tests (`percolator.rs`, `services.rs`, and the search crate's own).
- **The failure dump stays in the test,** so a future failure still shows its cause.

## For PK-win-portable-upd
- **Children inheriting `LODESTAR_RELAUNCH_TOKEN` is harmless.** `icacls` and other children of a relaunched copy inherit it unless it's removed, but the variable does nothing without a pipe that delivers the same token, and a console stdin skips the wait.
- **Any code that starts `lodestar.exe` itself must remove the marker** (`env_remove(windows::relaunch::MARKER)`) or go through `relaunch::command`. Otherwise the child waits for a hand-over that never comes, and ends.
- **The relaunch now exits with 80, and `POST /system/restart` starts the copy before answering.** An updater calling `Restart::request` directly still starts it there, as before.
