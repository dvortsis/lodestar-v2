# X-flake-portable-restart-3 decisions

`test (windows-latest)` failed on #717 (the Windows test job, 2026-11-12T04:57Z) in `a_portable_restart_serves_again_on_the_same_port`. The copy started by Restart now printed `WATCH_TIMED_OUT` and the refusal, so it never served. This slice hardens the copy's wait on the process it replaces (`crates/lodestar/src/system/restart.rs`) and makes a repeat failure explain itself. Nothing here loosens an owner or security decision. The changes to user-visible wording and to the wait's upper bound are the runner's call (`docs/owner-decisions.md` §2.26); they're listed under "For the runner".

## What the log shows, and what it can't
- **The old process didn't linger.** The test's `child.try_wait()` saw it exit (code 80), and the failing `nth_port` wait of 90 s ended at 04:57:40.92. So the test saw the exit at about 04:56:10.6–10.9Z, under a second after `app.relaunch` (04:56:10.213Z).
  - That rules out the brief's third candidate: no log flush, stderr pipe or inherited handle kept it alive past the hand-over.
- **The copy's watch didn't report that exit.** Rust refuses only when:
  - the watch says `timeout`, or exits with code 3;
  - or its own clock runs out: 30 s from `started`, or 60 s from the hand-over.

  So one of these happened:
  1. **A stall before the wait:** the PowerShell started but took 30 s or more to load `Get-Process` and `Select-Object` on a loaded runner. Four service tests were running `sc.exe` and PowerShell at the same moment.
  2. **An early `Wait-Process`:** it came back (an error, which `SilentlyContinue` hides, or a missed exit event) while the old process was still in its graceful shutdown. The script then said `timeout` at once, and the copy refused the moment the hand-over came.
  3. **A slow PowerShell exit:** the script said `exited`, but its own process didn't end for 30 s. The copy needed both.
- **The console lines carried no times,** so the log can't tell these apart. The sibling test passed in the same binary at the same moment, which points to a per-process race or stall rather than a broken script.
- **Contention between the tests in `windows_service.rs`** is the background to case 1, not a cause on its own. Making the tests serial would only hide it.

## The fix: one change for each case
- **Case 1: the 30 s runs from a new `ready` word.**
  - **The script now prints three words:**
    - `started` first, as a bare string, so not even `Write-Output`'s module loads before it;
    - `ready` once the first `Get-Process` and `Select-Object` have run, so its cmdlets have loaded, just before the wait;
    - `exited` or `timeout`, as before.
  - **The clock:** the copy measures `PREDECESSOR_WAIT` from the later of `ready` and the hand-over.
    - Until `ready` arrives, the copy waits at most twice `PREDECESSOR_WAIT` (60 s) from the hand-over.
    - Once it has arrived, the copy waits 30 s from it.
    - So the most the copy can wait is 90 s, three times `PREDECESSOR_WAIT`. Before, it was 60 s.
  - **The brief's suggestion, not a larger `PREDECESSOR_WAIT`:** the 30 s is unchanged. It's measured from when the watch can actually see the exit.
  - **A `ready` before `started` is ignored,** as any word before `started` already was.
- **Case 2: the wait is a loop on the script's own clock.**
  - `$t0 = Get-Date` is taken when the script is ready. Then, while the old process is still there, it repeats:
    1. work out the time left with `New-TimeSpan -Start $t0`;
    2. say `timeout` and exit with code 3 if under a second is left;
    3. otherwise run `Wait-Process -Id <old> -Timeout <left>`;
    4. check again, and pause 250 ms before the next round, so an instantly failing `Wait-Process` can't spin.
  - **Only the clock says `timeout` now.** A `Wait-Process` that returns early no longer reads as "still running".
  - **The reused-id check is unchanged,** now in one script block (`$gone`) used before and after each wait. A process with the old id that started after this copy isn't the old process.
- **Case 3: the copy believes `exited`.**
  - The word comes only after the wait has seen the exit, and it's the script's last statement before `exit 0`.
  - The copy now gives the watch `WATCH_DRAIN` (2 s) to end, and goes ahead even if it hasn't.
  - A watch that said `exited` but then ended with an error is still a skipped check (`WATCH_SKIPPED`), as before.
  - Exit code 3 (`WATCH_EXIT_TIMED_OUT`) still wins over any line, an `exited` one included: the copy refuses. The real script can't produce both; a unit test checks the precedence.
  - **This refines X-flake-portable-restart-2's rule** that the watch must both say `exited` and end well. "Exited only after the process object signals" still holds: the word follows `Wait-Process` or a `Get-Process` that no longer finds the process, exactly as before.
- **`WATCH_SCRIPT_LIMIT` grows to 750 s:** `GO_LIMIT` + `EXIT_LIMIT` + three times `PREDECESSOR_WAIT`, so the script still outlasts the copy's longest wait. It's still far under `Wait-Process`'s 32,767 s.

## Diagnostics, so a repeat failure names its case
- **The copy writes a second line after `WATCH_TIMED_OUT`,** before the refusal:

  `The check's timeline, from the hand-over: started -0.9 s, ready +0.4 s, no answer at +30.4 s.`

  - Its parts are when `started` and `ready` arrived (or `never`), and how the wait ended:
    - `no answer at …` when the copy's clock ran out;
    - `it said the copy was still running at …` when the watch said `timeout` or exited with code 3.
  - All times are in seconds from the hand-over.
  - Each of the three cases gives a different line:
    - case 1: `ready` late or never;
    - case 2: `it said … still running`;
    - case 3 can't time out any more.
  - Fixed words and numbers only: it can never carry the hand-over token.
  - It also helps a user's support request.
- **The test's failure dump now has times:**
  - every console line is stamped with the UTC time the test read it;
  - a "seen by the test" block lists when the test started the old process, asked for Restart now, saw the old process exit, and failed.

  With `app.relaunch` in `lodestar.jsonl`, that puts the copy's refusal against the old process's exit. The setup token was already on the console; the hand-over token never reaches the console or the dump.
- **`a_relaunched_copy_waits_for_the_process_it_replaces`** now also expects the timeline line, ending in `no answer at +`.

## Kept from X-flake-portable-restart and -2
- **Constrained Language Mode:** cmdlets, operators and a script block only, with no .NET method calls, no `::` and no member access. The Windows test still runs the real script under CLM, and now expects `started`, then `ready`, then `exited`.
- **The token never reaches the watch:** `watch_command` still removes it.
- **The bounded line reader:** 64-byte lines and 16 lines.
- **Exited only after the process object signals,** as above.

## For the runner (§2.26)
- **A new user-visible line** after "Lodestar's wait for the copy it replaces to exit timed out.": the timeline line quoted above. The first line, the skip line and the refusal are unchanged.
- **The copy's longest wait grows from 60 s to 90 s** from the hand-over. That's 30 s from `ready`, which may arrive up to 60 s after the hand-over. The refusal still says "30 s", which is the time it waited once the check was ready.
- **A watch that said `exited` but hasn't ended within 2 s counts as exited** (case 3 above).
- **The refusal keeps saying "still running 30 s after it shut down"** even when `ready` never arrived, so the copy gave up after 60 s without knowing. That was already so under -2; the timeline line (`ready never, no answer at +60.0 s`) now shows which it was.
- **Left as it was:** if the old process vanishes between `Get-Process` and reading its `StartTime`, the script errors and the copy skips the check, as under -2. Treating an unreadable start time as "still there" would turn a reused id held by another user's process into a refusal.

## Evidence
- **Linux:** unit tests on a fake clock cover each new behaviour:
  - the wait from `ready`: none, at 0 s, at 10 s, at 50 s, too late at 70 s, and before the hand-over;
  - a believed `exited`, and `exited` followed by a failure;
  - the timeline's words and signs;
  - the reader stamping `ready` and the last word;
  - the script's text: the loop, the clock, the order of the words, and CLM safety.
- **Mutation-checked:** 13 mutations of `restart.rs`, each made to fail a test:
  - the clock back on `started`, and the no-`ready` cap changed;
  - `exited` made to need the watch's end, and an error after `exited` ignored;
  - `ready` not parsed, accepted before `started`, or dropped from the script;
  - the loop collapsed to one `Wait-Process`, and `Write-Output` restored before `started`;
  - the timeline dropped, its end time taken from the wrong place, and the `timeout` word's time not stamped;
  - after the review: exit code 3 no longer winning over an `exited` line.
- **Windows:**
  - `cargo clippy --target x86_64-pc-windows-gnu -p lodestar --all-targets` is clean for the files this slice owns.
  - This session can't run Windows. CI's `test (windows-latest)` is the proof for the real script, the CLM test and the two portable-restart tests.
