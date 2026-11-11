# X-win-upd-window decisions: Windows update follow-ups after #659 and #677

Follow-ups from the reviews of #659 (PK-win-portable-upd, `reviews/rv-659-c48421bb.md`) and #677 (X-win-upd-1920, `reviews/rv-677-f4243e6c.md`). The code is in `crates/lodestar/src/update/windows/check.rs`, `portable.rs` and `main.rs`, and the notice's wording is in `web/src/lib/features/settings/updates/`. Calls marked "runner" are the runner's under §2.19; the owner can veto them.

## Install now respects Off and a pause (#677's flag, sent to the owner under §2.19)
- **When updates are switched Off, or a pause starts, the request waiting for the update helper goes** (`<data>\updates\request.json`). The helper's boot retry needs that file, so it can't replay Install now afterwards (owner §2.13: updates wait for their window). The helper isn't changed; X-win-upd-reoffer owns it.
  - **Switched Off:** the notifier's settings loop sees the change. Only the switch counts: a change to another setting, or Off staying Off, leaves the request.
  - **A pause starting:** after it's saved. A resume (0 days) leaves the request.
  - **At the notifier's start, with updates Off or paused:** a request left from when the service wasn't running goes too.
- **Install now asked for while updates are already Off or paused still goes ahead** (it's the user's explicit action). Only a switch to Off, or a pause starting, takes a waiting request back.
- **The ordering against an install under way:** a withdrawal bumps an epoch, then removes the file. The install reads the epoch when it starts and again after it writes its request. If the epoch moved, the install takes its own request back and answers `withdrawn`, and the helper isn't started. With both sides sequentially consistent, either the install sees the bump, or the removal comes after its write. Removing one file is atomic. A request the helper has already taken is past recall, as before.

## The install_blocked notice (runner, approving the web files)
- **`install_blocked` gets its own notice** in place of `install_failed`'s, which says the installer's log is worth reading. The helper answers `install_blocked` only while a retry remains: once retries are spent, it skips the version and answers `install_failed`, which keeps that notice.
- **The wording**, checked against the helper: "The update is waiting. Windows wouldn't start the Lodestar service at the end of the install, so the update wasn't installed. Nothing changed. Lodestar tries again the next time Windows starts, unless you switch updates off or pause them first." The last clause is true because of the section above: the retry needs the request, and switching Off or pausing removes it. zh says the same.
- The API's notice list and the generated `openapi.json` and `schema.d.ts` name the new code.

## The self-test's verdict and a graceful stop (#659 review, N4)
- **They never both run, and never at once.** Before, a stop could read the journal at `Swapped`; meanwhile `passed` removed `previous.exe` and the journal, and the stop then wrote `Swapped` back, with no old program behind it.
- Both now go through `portable::Verdict`, one lock and a state: whichever comes first runs whole, and the other then does nothing.
  - **A stop that comes first** gives its attempt back. The self-test's later result is ignored, because a stop isn't a failure.
  - **A verdict that comes first** stands. A later stop gives nothing back.
  - A test forces the interleave: inside the verdict, the lock is held and a stop started from another thread waits for it, then does nothing. No sleeps.

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
  - the withdrawal at start;
  - `Verdict`'s two phase checks.
