# X-win-upd-window decisions: Windows update follow-ups after #659 and #677

Follow-ups from the reviews of #659 (PK-win-portable-upd, `reviews/rv-659-c48421bb.md`) and #677 (X-win-upd-1920, `reviews/rv-677-f4243e6c.md`). The code is in `crates/lodestar/src/update/windows/check.rs`, `portable.rs` and `main.rs`, and the notice's wording is in `web/src/lib/features/settings/updates/`. Calls marked "runner" are the runner's under §2.19; the owner can veto them.

## Install now respects Off and a pause (#677's flag, sent to the owner under §2.19)
- **When updates are switched Off, or a pause starts, the request waiting for the update helper goes** (`<data>\updates\request.json`). The helper's boot retry needs that file, so it can't replay Install now afterwards (owner §2.13: updates wait for their window). The helper isn't changed; X-win-upd-reoffer owns it.
  - **Switched Off:** a watcher task of its own sees the change at once (self-review I1). The check loop can't, because it can wait for the check lock for a whole download. Only the switch counts: a change to another setting, or Off staying Off, leaves the request. The watcher reads the starting mode when it's created, not when it first runs, so a switch made in between isn't mistaken for the start. A test runs the real watcher through a change while Off and waits for it to be handled (runner review B1).
  - **At a stop** (runner review I2): a change waiting is handled before the stop is, and the watcher looks at the settings once more as it stops. Since X-win-upd-stop-wait, the service's shutdown waits up to 2 s for the updates task before aborting it, so that last look finishes unless it overruns the wait (see `2026-11-11-X-win-upd-stop-wait.md`).
  - **A pause starting:** after it's saved. A resume (0 days) leaves the request.
  - **Not at the notifier's start** (self-review I2): a request waiting then stays, whether updates are Off or paused. Otherwise an Install now made while Off would lose its retry at the first restart, and the notice's "tries again" would be wrong. Settings change while the service is stopped only when someone edits the file by hand.
  - **Off and then On again before the watcher runs** can coalesce into one change and withdraw nothing (self-review P6). Updates are On again by then, so the request stands, as it would have with no switch.
  - **The removal** is retried for about a second, as the renames are, for antivirus or the indexer holding the file. On Unix the folder is then flushed; on Windows there's no folder flush, and the removal relies on NTFS's own journal, as the renames do. The removal runs off the async runtime (self-review P2, P3). If it still fails, that's logged (`update.state_unsaved`) and the request stays.
- **Residuals** (runner review I2 and nit):
  - **A crash between saving Off and the removal** leaves the request, and the helper retries it at the next boot. Nothing re-checks at the next start (see "Not at the notifier's start"). While updates are Off, the notice then says how to cancel it (below).
  - **A last look that overruns the stop's 2 s wait** (the removal's retries, about a second, plus a slow disk) is aborted with the task. The request then stays, and the helper retries it at the next boot, as after a crash. Before X-win-upd-stop-wait, a stop right after a switch to Off could abort the task before the watcher had run at all; the wait closes that.
  - **A withdrawal between the install's recheck and the helper's start** is answered as a started install, and the screen shows Installing. The request is gone by then, so the helper finds nothing to install and the screen clears at the next status. A second recheck after the start can't fix the wording: by then the helper may already have taken the request, and that install is real.
- **Install now asked for while updates are already Off or paused still goes ahead** (it's the user's explicit action). Only a switch to Off, or a pause starting, takes a waiting request back.
- **The ordering against an install under way:** a withdrawal bumps an epoch, then removes the file. The install reads the epoch when Install now is accepted (self-review P1) and again after it writes its request. If the epoch moved, the install takes its own request back and answers `withdrawn`, and the helper isn't started. With both sides sequentially consistent, either the install sees the bump, or the removal comes after its write. Removing one file is atomic. A request the helper has already taken is past recall, as before.

## The install_blocked notice (runner, approving the web files)
- **`install_blocked` gets its own notice** in place of `install_failed`'s, which says the installer's log is worth reading. The helper answers `install_blocked` only while a retry remains: once retries are spent, it skips the version and answers `install_failed`, which keeps that notice.
- **Only while the retry's request is still there,** with the same id as the answer (self-review I2). The retry needs that request. Once Off or a pause has taken it back, or another request has replaced it, the notice is `install_failed`'s instead, so "tries again" is never shown when no retry is coming.
- **The wording**, checked against the helper: "The update is waiting. Windows wouldn't start the Lodestar service at the end of the install, so the update wasn't installed. Nothing changed. Lodestar tries again the next time Windows starts, unless you switch updates off or pause them first." The last clause is true because of the section above: the retry needs the request, and switching Off or pausing removes it. zh says the same.
- **While updates are already Off, the notice is `install_blocked_off`** (runner review I1): "switch updates off or pause them" would be no help there. Its body: "… Lodestar tries again the next time Windows starts, even with updates Off. To cancel that, set updates to Check and notify, then back to Off." That switch to Off is what takes the request back. zh says the same, with the mode names as the screen shows them.
- The API's notice list and the generated `openapi.json` and `schema.d.ts` name the new code.

## The self-test's verdict and a graceful stop (#659 review, N4)
- **They never both run, and never at once.** Before, a stop could read the journal at `Swapped`; meanwhile `passed` removed `previous.exe` and the journal, and the stop then wrote `Swapped` back, with no old program behind it.
- Both now go through `portable::Verdict`, one lock and a state: whichever comes first runs whole, and the other then does nothing.
  - **A stop that comes first** gives its attempt back. The self-test's later result is ignored, because a stop isn't a failure.
  - **A verdict that comes first** stands. A later stop gives nothing back.
  - The verdict is marked decided before it runs (self-review P4), so one that panics part way never lets a stop give an attempt back over it.
  - The stop runs off the async runtime, since it may wait for a rollback running on the blocking pool (P3).
  - A test pins that the lock is held for the whole verdict (`try_lock` fails inside it), and that a stop started from another thread meanwhile does nothing, whenever it gets to run (P5's wording). No sleeps.

## #659 nits
- N1: `at_start` is test-only (`#[cfg(test)]`); `serve` uses `begin`. So the Windows build has no unused function.
- N2: the decisions bullet on what the launcher fallback can't help now says why. On Windows a rename replaces an existing `lodestar.exe` unless it's running, so a launcher run inside the rename gap can lose the old program to the second rename.
- N3: a crash test again covers "Swapping, program missing, `previous.exe` present" (a start made some way other than the launcher), which puts the old program back. It sits beside the case the launcher leaves.

## Checks
- The Windows target can't be checked here: there's no MSVC or mingw toolchain for `ring`. The Windows CI job compiles it.
- Mutation-checked, all killed:
  - the pause's withdrawal;
  - the switch-to-Off condition, both halves;
  - the install's epoch recheck;
  - the epoch bump;
  - the watcher's withdrawal;
  - the watcher's starting mode (starting it as not Off fails the real-watcher test);
  - `install_blocked_off` only while Off;
  - `install_blocked` only while its request is pending;
  - `Verdict`'s two phase checks.
- Not separable by a test, kept as defence in depth: the watcher's last look at a stop, and the check task waiting for the watcher rather than aborting it. With the select biased to a waiting change, a change sent before the stop is always handled first. And work already on the blocking pool runs to the end even when its task is aborted. So removing either one leaves every test green; forcing the order they guard would need a test-only gate in the removal.
