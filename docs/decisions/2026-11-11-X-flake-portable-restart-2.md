# X-flake-portable-restart-2 decisions

These are follow-ups from the #662 review (X-flake-portable-restart) to the portable build's Restart now. A copy started by Restart now waits for the process it replaces to exit before it opens the data folder. It does that through a Windows PowerShell watch (`crates/lodestar/src/system/restart.rs`). Nothing here loosens an owner or security decision or changes an owner-set number.

- **The watch is safe under Constrained Language Mode.**
  - **Before:** the script waited with `$old.WaitForExit()`. Under Constrained Language Mode, which AppLocker or WDAC policies impose, that .NET method call is blocked. The script then failed, and the copy went ahead without waiting, which is the flake #662 fixed.
  - **Now:** the script uses only cmdlets and operators:
    - it reads start times through `Get-Process | Select-Object -ExpandProperty StartTime`;
    - it waits with `Wait-Process -Id <old> -Timeout <n>`, so the wait is bounded.
  - **The script's bound isn't 30 s,** although the brief asked for that; it's `WATCH_SCRIPT_LIMIT`, 720 s (`GO_LIMIT` + `EXIT_LIMIT` + twice `PREDECESSOR_WAIT`).
    - **Why:** the watch starts before the old process's graceful shutdown, which may take up to `GO_LIMIT` (600 s). A 30 s bound inside the script could run out during a slow shutdown. The copy would then refuse the restart the moment the hand-over came, with no wait at all. The fresh review before the PR found this.
    - **The 30 s is unchanged:** Rust still measures `PREDECESSOR_WAIT` from the hand-over (`await_watch`), refuses after it, and ends the watch. The script's bound only keeps a forgotten watch from running forever.
    - A unit test checks that the bound is at least that sum.
  - **A reused id is checked after the wait too.** If the old process exits just as the wait ends and its id is taken at once, the new holder started after this copy, so it doesn't count as the old process still running.
  - A unit test checks that the script has no `.WaitForExit(`, no `::` static call and no member access on `$old`.
  - A Windows-only test runs the real script under Constrained Language Mode, after first checking that the mode is in force (a .NET static call must fail, with `$ErrorActionPreference = 'Stop'` so the error ends the probe). Its old process runs until the test kills it. The test reads `started` from the pipe (within 60 s) and checks that the script is still waiting. Then it kills the old process and expects `exited` and exit code 0. So it also shows that `started` arrives before the wait ends.
- **The 30 s runs from the later of the watch's `started` marker and the hand-over.**
  - The script prints `started` first, before anything that can be slow.
  - The copy reads the watch's status lines on a thread (`read_watch`) and measures `PREDECESSOR_WAIT` from that marker, or from the hand-over if the marker came earlier (`await_watch`).
  - So a PowerShell that's slow to start no longer eats into the wait. The wait is never longer than twice `PREDECESSOR_WAIT` from the hand-over.
  - Unit tests on a fake clock check 30, 40 and 60 s for no marker, an early marker, a late one and a very late one.
- **One stderr line when the wait is skipped or times out,** then it carries on as before:
  - **Skipped** (no watch, or it failed or said nothing): "Lodestar couldn't check that the copy it replaces had exited, so it's starting without that check." The copy goes ahead.
  - **Timed out:** "Lodestar's wait for the copy it replaces to exit timed out." The refusal follows, as before.
  - **The wording lives in Rust, not in the script,** so it's fixed and can never carry the hand-over token.
  - **The watch never receives the token.** On `main`, `system32_command` (which `powershell_command` builds on) already removes `LODESTAR_RELAUNCH_TOKEN`. `watch_command` now removes it as well, a second guard next to the script, and a unit test checks it.
- **Only the status words count.** Anything before `started`, an unknown word, a line past 64 bytes or past 16 lines is ignored, so a misbehaving shell can't grow memory or fake an exit. The copy counts the old process as gone only if the watch said `exited` and also ended well. A byte-order mark before a word is ignored. Exit code 3 (`WATCH_EXIT_TIMED_OUT`) counts as still running even if the `timeout` line was lost.
- **A runner-approved shared edit:** `crates/lodestar/src/windows/mod.rs`, in `relaunch::os::watch_predecessor` and `relaunch::os::wait_if_successor` only. They now call `restart::watch_command`, `read_watch` and `await_watch`, and write the note to stderr. `await_predecessor` and `watch_says` are gone, replaced by `await_watch`. PK-win-portable-upd (#659) edits other parts of that file. Whichever PR merges second brings `main` in.
- **The script's bound stays within `Wait-Process`'s limit:** 720 s, under its maximum of 32,767 s; a unit test checks both.
- **Residuals, the runner's call (2026-11-11):**
  - **A failed or skipped wait lets the copy go ahead**, as the brief says, with the skip line. On a machine where WDAC or AppLocker blocks `powershell.exe` itself, every Restart now gets the skip line and no wait. Refusing instead would make Restart now impossible there.
  - **A reused id can cause a needless refusal.** If the old process exits between `Get-Process` and `Wait-Process`, and its id is taken at once, the wait can run on the new holder. It fails closed and is very unlikely.
- **Only the windows-latest CI job can prove the PowerShell behaviour** (the runner, 2026-11-11). This session can't run Windows: the Constrained Language Mode test and the two `windows_service.rs` tests run there. The logic around them (the status parsing, the clock, the token removal) is tested on every platform.
- **The Windows CI job takes a few seconds longer, not the 30 s the brief expected.** No new test waits the full 30 s:
  - the Constrained Language Mode test kills its old process as soon as the script has said `started`;
  - the skip-line test is a quick start-and-fail run;
  - the existing 30 s wait test only gains an assertion.
- **Mutation-checked:** 18 mutations of `restart.rs`, each made to fail a test:
  - the `started` marker, `WaitForExit` restored, and `-Timeout` removed;
  - the token kept in the watch's environment;
  - a status taken before `started`, a later `started` moving the clock, and the 16-line cap;
  - each skip line and the timeout line dropped;
  - the exit status ignored;
  - the marker ignored, the 2× cap, and the drain for a last line still in the pipe;
  - after the review: the script's bound back to `PREDECESSOR_WAIT`, and the reused-id check after the wait removed;
  - after the runner's review: exit code 3 not read as still running, and the byte-order mark not stripped.

  The first run left two survivors, a second `started` and the drain; a test was added for each.
