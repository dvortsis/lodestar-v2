# X-win-restart-followups-2 decisions

The follow-ups from #649's review (`reviews/rv-649-8d4ac5f6.md`: findings 1, 2, 4 and 5). Brief: runner, 2026-11-09T04:58Z.

## A relaunched copy that fails at start stays readable (finding 1)
- **The case:** after the hand-over, the old process exits 80 and `Start Lodestar.cmd` ends without `pause`. If the relaunched copy then fails at start (a held port, an unreadable config, a lock still held past its wait), it prints its error and exits. With nothing left attached to the console, the window closes before anyone reads it.
- **`restart::hold_if_relaunch_failed`:** `main` passes it the serve command's exit code. A copy started by Restart now (its environment has `LODESTAR_RELAUNCH_TOKEN`) that ends with anything but success:
  - prints "Lodestar couldn't start again after Restart now. The error is above.", the log folder if the config loaded, and "Press Enter to close this window.";
  - then reads a line from `CONIN$`.
- **Who never waits:**
  - With no console (the service, a detached run), `CONIN$` doesn't open, so nothing waits. The service never has the marker anyway.
  - A success, or a run Restart now didn't start, returns at once.
- **Off Windows** it returns the code unchanged.
- **The call site** is one line in `crates/lodestar/src/main.rs`, a shared edit (see "Shared edits").
- **Tests:**
  - `only_a_failed_relaunch_holds_the_window` (the rule, on every OS).
  - The key wait itself isn't tested on Windows CI. A test would need a console attached to the relaunched copy, and CI runs without one: `CONIN$` doesn't open there, which is the "nothing waits" path.

## The companions lock (finding 2)
- **The problem:** `Manager::open` (`crates/companions/src/manager.rs`) answering `Busy` after a quick restart on Windows turned companions off for the whole run (`install_failed step=open reason=busy`).
- **The fix:** on Windows it now tries a busy lock again every 100 ms for up to 10 s, as `lodestar-search`'s index lock does (X-win-restart-followups). Elsewhere it's one attempt. A lock error other than busy is never retried.
- **A local helper, `retry_busy`,** over `std::fs::TryLockError`. companions doesn't depend on search, and no existing common crate fits without a new dependency edge.
  - It's tested on a fake clock, with no waiting: busy then free, busy past the wait, no wait, and an error.
- **Callers:**
  - Serve opens the manager in `spawn_blocking` at start (`crates/lodestar/src/companions.rs`), where 10 s is acceptable.
  - The `lodestar companion` command opens it too. On Windows, while serve is running, it now says another Lodestar process is managing companions after up to 10 s instead of at once.

## Nits (findings 4 and 5)
- **`EXIT_RELAUNCHED`:** `update::control` is Unix-only and the relaunch is Windows-only, so neither can name the other.
  - `EXIT_RELAUNCHED` is now defined on every platform.
  - `the_relaunch_exit_code_is_the_restart_code` (a Unix test) pins it to `EXIT_RESTART`.
- **`rebuild.rs`'s `lock_root` doc now says that only opening the lock file is never retried.** tantivy 0.27 reports every failed try-lock as busy, so a filesystem that refuses locks is tried for the whole wait on Windows, then reported as in use.

## Left for a later slice
- **`crates/lodestar/src/update/layout.rs` (~239), the update `bin` folder's `.lock`,** has the same non-blocking `LockFileEx` pattern. PK-win-upd-3 (#644) is open on `update/**`, so it goes in its own slice once #644 merges.

## Shared edits
- `crates/lodestar/src/main.rs`: the one-line call (runner-approved; see the PR).
