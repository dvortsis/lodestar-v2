# X-win-restart-followups-2 decisions

The follow-ups from #649's review (`reviews/rv-649-8d4ac5f6.md`: findings 1, 2, 4 and 5). Brief: runner, 2026-11-09T04:58Z.

## A relaunched copy that fails at start stays readable (finding 1)
- **The case:** after the hand-over, the old process exits 80 and `Start Lodestar.cmd` ends without `pause`. If the relaunched copy then fails at start (a held port, an unreadable config, a lock still held past its wait), it prints its error and exits. With nothing left attached to the console, the window closes before anyone reads it.
- **`restart::hold_if_relaunch_failed`** wraps everything `main` runs. It waits only when all of these hold:
  - the copy was started by Restart now;
  - it failed at start, before it was listening (`Restart::has_listened`). A copy that served and then failed doesn't wait (#651's review, LOW 2);
  - it's in `Start Lodestar.cmd`'s window: the launcher sets `LODESTAR_PORTABLE_WINDOW=1`, and the copy inherits it;
  - `CONIN$` opens.
- **What it does then:**
  - prints "Lodestar couldn't start again after Restart now. The error is above.";
  - prints the log folder: the folder the log file opened in this run (a folder moved in the app is honoured), or else the config's default `<data>\logs` from the same `--config`;
  - prints "Press Enter to close this window.";
  - then waits for one line from `CONIN$`, **for at most 10 minutes** (`HOLD_LIMIT`). An end of file or an error ends it at once.
- **Why the launcher opts in, not a visible-window check** (#651's review, LOW 1):
  - A console can exist with no window anyone sees: a `CREATE_NO_WINDOW` start, Task Scheduler, a service wrapper such as NSSM. `CONIN$` opens there too. The first version would have waited for ever.
  - Asking Windows whether the window is visible (`GetConsoleWindow`, `IsWindowVisible`) needs `unsafe` FFI, which the workspace forbids (`unsafe_code = "forbid"`), and no safe crate in the lock wraps it.
  - The launcher, which does run in a visible window, sets `LODESTAR_PORTABLE_WINDOW=1` instead. A run started any other way doesn't have it, so it never waits. The 10-minute deadline bounds any case left over.
- **"Relaunched" is read once, first thing in `main`** (`Restart::global().relaunched()`, before `run`, so before the Windows hook and any config load), and passed to the wrapper. `Restart` reads the relaunch token when it's created, which is that call. So a config that fails early still counts, and a later change to the environment can't hide it (#651's review). An empty token isn't one, as in `wait_if_successor`. The token is never removed anyway: edition 2024 makes that `unsafe`.
- **Off Windows** the wrapper returns the code unchanged.
- **The call site wraps `main` as a whole, not the serve arm's result** (the runner approved "a one-line shared edit wrapping the serve result"). The serve arm returns early on several failures: a config that doesn't load, the rollback journal, the runtime. A wrap of its result would miss exactly the "bad config" case. So `fn main` is renamed `run`, and a short `main` calls `hold_if_relaunch_failed(relaunched, run())`. A relaunch only ever runs `serve` (same arguments), so for other commands it's the identity.
- **Tests on every OS:**
  - `only_a_failed_relaunch_at_start_in_the_window_holds`: the rule (relaunched, failed, not yet listening, in the window), the message, and the identity off Windows.
  - `the_hold_has_a_deadline`: a line or an end of file ends the wait; a pipe that never answers ends it at the deadline.
  - `listening_marks_the_start_done`.
  - The unit tests call the wrapper itself only off Windows, so a developer's console never waits.
- **Tests on Windows** (`crates/lodestar/tests/windows_service.rs`): a run with a config that doesn't load, started in a console of its own (`CREATE_NEW_CONSOLE`, so `CONIN$` opens) with stderr kept.
  - `a_relaunched_copy_that_fails_at_start_waits_for_enter`: with the token, the hand-over on stdin and the window variable, it prints the config error and the prompt, and is still running.
  - `an_ordinary_run_that_fails_at_start_exits`: without the token it exits with a failure and no prompt.
  - `a_relaunch_outside_the_launchers_window_exits`: with the token but no window variable (the hidden-console case), it exits with a failure and no prompt.

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
- **Unix never waits for either lock:** `unix_never_waits_for_the_lock` in `rebuild.rs` and in `manager.rs` pins `LOCK_WAIT` to zero off Windows.
- **`rebuild.rs`'s `lock_root` doc now says that only opening the lock file is never retried.** tantivy 0.27 reports every failed try-lock as busy, so a filesystem that refuses locks is tried for the whole wait on Windows, then reported as in use.

## Left for a later slice
- **`crates/lodestar/src/update/layout.rs` (~239), the update `bin` folder's `.lock`,** has the same non-blocking `LockFileEx` pattern. PK-win-upd-3 (#644) is open on `update/**`, so it goes in its own slice once #644 merges.

## Shared edits
- `crates/lodestar/src/main.rs`: `main` becomes `run`, and the new `main` calls `hold_if_relaunch_failed(run())` (runner-approved, 2026-11-09T05:01Z, as a wrap of the serve result; widened to `main` for the early returns, as above).
