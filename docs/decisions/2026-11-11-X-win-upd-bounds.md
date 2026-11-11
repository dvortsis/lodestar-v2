# X-win-upd-bounds decisions

This slice fixes the bounds and test gaps left by the Windows MSI updater: the residuals #644 accepted (`2026-11-09-PK-win-upd-3.md` §"Accepted residuals") and the notes from its round 5 review. The brief is from the runner, dated 2026-11-11. The reviews are `reviews/rv-644-70b3f9a5.md` (P-3, P-4, P-8), `reviews/sec-644-70b3f9a5.md` (LOW-B, LOW-C) and `reviews/rv-644-e8dd3ccb.md` (R5-1 to R5-3). This file supersedes the "Accepted residuals" list in `2026-11-09-PK-win-upd-3.md` for the items below.

No new privilege, trigger or principal is added, so the Q13 row of §2.47 holds as approved. Every new Windows call goes through programs the helper already runs as SYSTEM, from `System32` by absolute path, with a fixed script and no input: PowerShell and its CIM cmdlets. The workspace still forbids `unsafe`, and `Cargo.toml` is unchanged.

## LOW-B / P-8: a 3010's wait for a restart is bounded by boot identity
- **The case:** a rollback's reinstall answered 3010 or 1641, and step 5 waits until the program on disk is `from`. If the pending replacement was cancelled or failed, every boot run saw `to` still on disk and retried. The service stayed disabled, with no halt and no recovery text. A forward install's 3010 had the same gap (the self-review's finding): with `from` still on disk at every boot it answered `installed` (restart pending) for ever, and its journal held back every later update.
- **The bound (`boot_seen`):**
  - The run that takes the 3010 records Windows' boot time (`Win32_OperatingSystem.LastBootUpTime`, as Unix seconds) in the journal, as `restart_boot`. For the install that's `probation`; for the rollback, step 5's first waiting pass in the same run.
  - Each waiting run reads the boot time again. One more than `BOOT_SLACK` (60 s) from the recorded one is a later boot that still sees the old program. It's counted in `restarts`, and the new boot is saved.
  - At `MAX_RESTARTS` (2):
    - **The rollback halts.** The service stays disabled and stopped with the marker in place, and the Event Log and the recovery text say step 4 of 5 stopped because the replacement didn't take effect.
    - **The install ends as `install_interrupted`.** The journal goes, the start type goes back, the service is started (it's `from`, still running) and the Event Log says why. Nothing is skipped, so the next check offers the version again.
- **Why two restarts, not one:** the boot time comes from the clock. A large clock change between two runs in one boot could make one boot look like two. Two restarts mean such a misreading never ends a wait by itself, and a failed replacement still ends it on the second boot.
- **A boot time that can't be read** counts nothing, so the wait goes on as it did before the bound. A journal with no `restart_boot`, written by an earlier build or when the boot time couldn't be read, takes it from the next run that can read it.
- **Across versions:** both fields are `#[serde(default)]`. An older helper ignores them and waits as before, which is right once the program it is on disk.

## LOW-C: the copy waits only for its own parent
- **The case:** the copy waited, then refused with `program_files_in_use`, until no process at all held `lodestar.exe`. Any local user can run that program. So a user could block every update indefinitely and keep a rollback pending.
- **The lock:**
  - Before the helper run that hands off starts the copy, it takes a lock on `run-<hex>.parent` beside the copy, in staging, which only SYSTEM and Administrators can open.
  - It never closes that handle (`mem::forget`), so Windows releases the lock when the process ends.
  - The copy waits for that lock alone, up to `FREE_WAIT` (120 s, as before). If the parent is still running past that, it refuses with `helper_running`.
- **Then a best-effort wait:** the copy waits up to `IMAGE_WAIT` (30 s) for the program in Program Files to be free, and never refuses on it. If the parent hasn't ended within `FREE_WAIT`, the copy fails closed (`helper_running`, nothing run); that path and the normal one are tested.
- **What's left to a holder of `lodestar.exe` (residuals; the runner flags them to the owner):**
  - **Holding it with read sharing** (running it, or opening it to read): Windows Installer replaces it at the next restart (3010), a path the helper handles. A forward install waits for the restart and judges `to` at the boot run. A rollback's reinstall waits at step 5 with the service disabled, until the restart (bounded as above). So a local user can turn an update into a 3010, and delay a rollback's end until the next reboot, but no longer block either for as long as they stay logged on.
  - **Holding it with no sharing** (`CreateFile` with share mode 0, in the moment between the copy's checks and `StartServices`): the MSI's `ServiceControl Start="install" Wait="yes"` can't start the service from the held file. That's error 1920, so msiexec exits 1603. A forward install is then `install_failed` and its version is skipped for good, and a rollback's reinstall halts. This predates the slice: the old `wait_free` check was just as racy, since a user could open the file right after it passed. So "a local user then gets nothing from holding the file" (sec-644 LOW-C) holds only for read-shared holders. A fix would tell a 1920 from a real failure (`msiexec`'s log) or retry a 1603 a bounded number of times before skipping. That needs a follow-up slice and was raised with the runner.
- **Not a process handle:** sec-644 suggested waiting on the parent's process handle with `SYNCHRONIZE`. That needs `OpenProcess`, which is `unsafe`. A lock that the parent's exit releases gives the same "own parent only" property with std's safe calls.
- **Swept:** `clean_copies` removes stale `run-*.parent` files with the copies, and the copy removes its own once it's free.
- **Outside the brief's owns:** `cli/update_helper.rs`'s `copy()` builds the wait closure. The edit is about ten lines there; the runner approved it (2026-11-11) and is adding the file to this slice's owns.
- **Cleaned up:** each helper run sweeps `run-*.parent` with the old copies whenever no copy is running, the drop path included (tested). A copy also removes its own once the parent has let go.

## P-3: after a deadline kill, wait for Windows Installer to be idle
- **The case:** after the deadline kill, `run_copy` writes no answer and lets go of its lock. The next run then judged the program on disk while Windows Installer's server-side transaction might still finish. It could report `install_interrupted` and start the service, and `to` could then land with no journal and no probation.
- **The fix:** when `msiexec` started and didn't finish with a code, the copy waits for Windows Installer to be idle before it drops `msiexec.lock`. The cases are the deadline kill and an exit with no code. "Idle" means no `Global\_MSIExecute` mutex, checked every 5 s through PowerShell's `Mutex.TryOpenExisting`; a mutex that can't be opened counts as busy.
- **The wait** is bounded at `INSTALLER_IDLE_WAIT` (30 min). While the copy waits, every helper run sees the lock held and returns `handed_off`. Past the bound it logs and lets go as before, which is now a residual only after a 90-minute install and a further 30 minutes busy.
- **Any install counts:** the mutex is Windows Installer's own, so an unrelated install (Windows Update among them) holds the copy too, for at most the same 30 minutes. That's bounded, and it's only after an interrupted run.

## P-4: `try_wait` classification, and the HandedOff race
- **Classification, by position:** `Windows::msiexec` now waits through `supervise`. Once `msiexec` is spawned, every error is `Interrupted`, the deadline is `TimedOut`, and neither is ever answered as a refusal. That includes an error from `try_wait`, which now also ends the child (reaped only once the kill succeeds, so a failed kill can't leave it waiting). A failed spawn is mapped to a plain error, so it's "not started" whatever its own kind. `supervise` takes the child, clock and sleep as parameters, so it's tested on every OS. The spawn mapping is Windows-only code.
- **The race:** the copy starts the task right before it ends. A run that comes quickly can find the copy's lock free but its file not removable, because the image is still mapped. That run returned `handed_off`, and nothing ran again until the next boot.
  - `copy_running` now tries the removal again every 200 ms for up to `COPY_EXIT_WAIT` (15 s).
  - It looks at the lock only once, first. Probing the lock again while a copy starts could take it from under that copy.
  - A copy that is really starting still can't be removed, so the result after the wait is the same as before.

## The busy-lock retry for `.lock` (brief item 5; rv-649 finding 2)
- **Where it was missing:** `Bin::lock` (`update/layout.rs:~239`) is one try, which is right for its other callers. The helper's `remove_marker` and `mark_failed` called it once, right after the service stopped. On Windows a stopped process's lock can linger for a moment, so a busy one meant "the rollback marker couldn't be removed": a retry left for the next boot, with the service disabled meanwhile. A busy one also meant a failed version not skipped.
- **The fix uses the helper's existing bounded wait,** `take_lock` (up to `lock_wait`, 30 s, every 100 ms), which step 1 and the install already use. It's not a second retry in `layout.rs`, so `layout.rs` is unchanged. Its wait comes from the context, so the wiring is tested: a lock held from msiexec on, and let go at `take_lock`'s first busy pass (a `System::lock_busy` test hook), still gets the skip written and the marker removed.
- **`remove_marker` takes no lock when there's no marker.** On the drop path a version installed by hand may be serving and holding `.lock` for good.
- **The service's own lock:** `check::Notifier::bin()` is called from async code, so it gets no blocking wait. It already opens the folder again on its next call when the lock was busy at start, so one busy moment doesn't turn updates off. `take_lock` runs only in the helper's own process, which has no async runtime.

## Tests that now catch what they claimed (R5-1, R5-2)
- **R5-1:** the rollback Fake's service now behaves like the real idle service.
  - A start while the marker exists idles.
  - A start of a running service changes nothing (the SCM's 1056).
  - Once the marker goes, the idle service ends or carries on as `Idle::MarkerGone.exit_code()` says, noticed at the next health look (its poll comes after the helper's start).
  - So `a_rollback_leaves_the_service_running_when_the_msi_started_it` fails on 93a26643's behaviour (`MarkerGone => Some(0)`): checked by mutation.
- **R5-2:** `a_manual_install_drops_the_journal` plants the marker and checks that the drop path removes it.
- **R5-3:** the reflowed doc line in `windows/service.rs` is wrapped.

## Not verified here
- The Windows-only code isn't built off Windows: `boot_time`, `installer_idle`, the parent lock in `run_msiexec`, the spawn mapping and `wait_free`. It rests on Windows CI and on the parse functions, which are tested on every OS.
