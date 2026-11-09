# X-win-restart-followups-2 decisions

The follow-ups from #649's review (`reviews/rv-649-8d4ac5f6.md`: findings 1, 2, 4 and 5). Brief: runner, 2026-11-09T04:58Z.

## A relaunched copy that fails at start stays readable (finding 1)
- **The case:** after the hand-over, the old process exits 80 and `Start Lodestar.cmd` ends without `pause`. If the relaunched copy then fails at start (a held port, an unreadable config, a lock still held past its wait), it prints its error and exits. With nothing left attached to the console, the window closes before anyone reads it.
- **`restart::hold_if_relaunch_failed`** wraps everything `main` runs. A copy started by Restart now that ends with anything but success, with a console attached:
  - prints "Lodestar couldn't start again after Restart now. The error is above.";
  - prints the log folder (`<data>\logs`, from the same `--config` the copy was started with), if that config loads;
  - prints "Press Enter to close this window.";
  - then reads one line from `CONIN$`. An end of file or an error ends the wait at once.
- **"Relaunched" is read once, first thing in `main`** (`Restart::global().relaunched()`, before `run`, so before the Windows hook and any config load), and passed to the wrapper. `Restart` reads the relaunch token when it's created, which is that call. So a config that fails early still counts, and a later change to the environment can't hide it (#651's review). The token is never removed anyway: edition 2024 makes that `unsafe`.
- **Who never waits:**
  - With no console (the service, a detached run), `CONIN$` doesn't open, so nothing waits. The service never has the token anyway.
  - A success, or a run Restart now didn't start, returns at once.
- **Off Windows** the wrapper returns the code unchanged.
- **The call site wraps `main` as a whole, not the serve arm's result** (the runner approved "a one-line shared edit wrapping the serve result"). The serve arm returns early on several failures: a config that doesn't load, the rollback journal, the runtime. A wrap of its result would miss exactly the "bad config" case. So `fn main` is renamed `run`, and a three-line `main` calls `hold_if_relaunch_failed(run())`. A relaunch only ever runs `serve` (same arguments), so for other commands it's the identity.
- **Tests:** `only_a_failed_relaunch_holds_the_window` checks the decision and the wait on every OS, with the console and the output injected:
  - relaunched and failed: the message, the log folder, then one read;
  - an end of file: the wait ends;
  - a success, not relaunched, or no console: no wait, nothing printed;
  - off Windows: the identity.
  - On Windows (`crates/lodestar/tests/windows_service.rs`), a run with a config that doesn't load, started in a console of its own (`CREATE_NEW_CONSOLE`, so `CONIN$` opens) with stderr kept:
    - `a_relaunched_copy_that_fails_at_start_waits_for_enter`: with the token and the hand-over on stdin, it prints the config error and the prompt, and is still running.
    - `an_ordinary_run_that_fails_at_start_exits`: without them it exits with a failure and no prompt.
  - The unit test calls the wrapper itself only off Windows, so a developer's console never waits.

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
- `crates/lodestar/src/main.rs`: `main` becomes `run`, and the new `main` calls `hold_if_relaunch_failed(run())` (runner-approved, 2026-11-09T05:01Z, as a wrap of the serve result; widened to `main` for the early returns, as above).
