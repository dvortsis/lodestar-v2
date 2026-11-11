# X-win-upd-bounds decisions

This slice fixes the bounds and test gaps left by the Windows MSI updater: the residuals #644 accepted (`2026-11-09-PK-win-upd-3.md` §"Accepted residuals") and the notes from its round 5 review. The brief is from the runner, dated 2026-11-11. The reviews are `reviews/rv-644-70b3f9a5.md` (P-3, P-4, P-8), `reviews/sec-644-70b3f9a5.md` (LOW-B, LOW-C) and `reviews/rv-644-e8dd3ccb.md` (R5-1 to R5-3). This file supersedes the "Accepted residuals" list in `2026-11-09-PK-win-upd-3.md` for the items below.

No new privilege, trigger or principal is added, so the Q13 row of §2.47 holds as approved. Every new Windows call goes through programs the helper already runs as SYSTEM, from `System32` by absolute path, with a fixed script and no input: PowerShell and its CIM cmdlets. The workspace still forbids `unsafe`, and `Cargo.toml` is unchanged.

## LOW-B / P-8: the reinstall's wait for a restart is bounded by boot identity
- **The case:** a rollback's reinstall answered 3010 or 1641, and step 5 waits until the program on disk is `from`. If the pending replacement was cancelled or failed, every boot run saw `to` still on disk and retried. The service stayed disabled, with no halt and no recovery text.
- **The bound:**
  - The first waiting pass, in the run that takes the 3010, records Windows' boot time (`Win32_OperatingSystem.LastBootUpTime`, as Unix seconds) in the journal, as `reinstall_boot`.
  - Each waiting pass reads the boot time again. A boot time more than `BOOT_SLACK` (60 s) from the recorded one is a later boot that still sees `to`. It's counted in `restarts`, and the new boot is saved.
  - At `MAX_RESTARTS` (2) the rollback halts. Halting keeps the service disabled and stopped with the marker in place, and writes the Event Log entry and the recovery text ("stopped after step 4 of 5").
- **Why two restarts, not one:** the boot time comes from the clock. A large clock change between two runs in one boot could make one boot look like two. Two restarts mean such a misreading never halts by itself, and a failed replacement still halts on the second boot.
- **A boot time that can't be read** counts nothing, so the wait goes on as it did before the bound. A journal with no `reinstall_boot`, written by an earlier build or when the boot time couldn't be read, takes it from the next run that can read it.
- **Across versions:** both fields are `#[serde(default)]`. An older helper (`from`, after the restart) ignores them and finishes step 5 as before, which is the right behaviour once `from` is on disk.

## LOW-C: the copy waits only for its own parent
- **The case:** the copy waited, then refused with `program_files_in_use`, until no process at all held `lodestar.exe`. Any local user can run that program. So a user could block every update indefinitely and keep a rollback pending.
- **The lock:**
  - Before the helper run that hands off starts the copy, it takes a lock on `run-<hex>.parent` beside the copy, in staging, which only SYSTEM and Administrators can open.
  - It never closes that handle (`mem::forget`), so Windows releases the lock when the process ends.
  - The copy waits for that lock alone, up to `FREE_WAIT` (120 s, as before). If the parent is still running past that, it refuses with `helper_running`.
- **Then a best-effort wait:** the copy waits up to `IMAGE_WAIT` (30 s) for the program in Program Files to be free, and never refuses on it.
  - A file held by anyone else is left to Windows Installer, which replaces it at the next restart (3010).
  - The helper already handles 3010: a forward install waits for the restart and judges `to` at the boot run, and a rollback's reinstall waits at step 5, bounded as above.
- **Not a process handle:** sec-644 suggested waiting on the parent's process handle with `SYNCHRONIZE`. That needs `OpenProcess`, which is `unsafe`. A lock that the parent's exit releases gives the same "own parent only" property with std's safe calls.
- **Swept:** `clean_copies` removes stale `run-*.parent` files with the copies, and the copy removes its own once it's free.
- **Outside the brief's owns:** `cli/update_helper.rs`'s `copy()` builds the wait closure. The edit is about ten lines there, and the runner was told before it was made.

## P-3: after a deadline kill, wait for Windows Installer to be idle
- **The case:** after the deadline kill, `run_copy` writes no answer and lets go of its lock. The next run then judged the program on disk while Windows Installer's server-side transaction might still finish. It could report `install_interrupted` and start the service, and `to` could then land with no journal and no probation.
- **The fix:** when `msiexec` started and didn't finish with a code, the copy waits for Windows Installer to be idle before it drops `msiexec.lock`. The cases are the deadline kill and an exit with no code. "Idle" means no `Global\_MSIExecute` mutex, checked every 5 s through PowerShell's `Mutex.TryOpenExisting`; a mutex that can't be opened counts as busy.
- **The wait** is bounded at `INSTALLER_IDLE_WAIT` (30 min). While the copy waits, every helper run sees the lock held and returns `handed_off`. Past the bound it logs and lets go as before, which is now a residual only after a 90-minute install and a further 30 minutes busy.

## P-4: `try_wait` classification, and the HandedOff race
- **Classification:** `Windows::msiexec` now waits through `supervise`. Once `msiexec` is spawned, every error is `Interrupted`, the deadline is `TimedOut`, and neither is ever answered as a refusal. That includes an error from `try_wait`, which now also ends the child. Only a failed spawn is "not started". `supervise` takes the child, clock and sleep as parameters, so it's tested on every OS.
- **The race:** the copy starts the task right before it ends. A run that comes quickly can find the copy's lock free but its file not removable, because the image is still mapped. That run returned `handed_off`, and nothing ran again until the next boot.
  - `copy_running` now tries the removal again every 200 ms for up to `COPY_EXIT_WAIT` (15 s).
  - It looks at the lock only once, first. Probing the lock again while a copy starts could take it from under that copy.
  - A copy that is really starting still can't be removed, so the result after the wait is the same as before.

## The busy-lock retry (`update/layout.rs`, rv-649 finding 2)
- **`Bin::lock_retrying`:** it's `Bin::lock` with a busy lock tried again every 100 ms for up to 10 s on Windows, and one attempt elsewhere. That matches `lodestar-search`'s index lock and the companion lock. An I/O error comes back at once. The loop (`retry_busy`) is tested on a fake clock.
- **Where it's used:** the helper's `remove_marker` and `mark_failed`, which took `.lock` once, right after the service stopped. A lock the stopped service hadn't released yet meant "the rollback marker couldn't be removed", a retry left for the next boot.
- **Not inside `Bin::lock` itself:** the service's `check::Notifier::bin()` calls it from async code, where a 10 s block would stall a worker. `bin()` already opens the folder again on its next call when the lock was busy at start, so one busy moment doesn't turn updates off. Its callers are unchanged.

## Tests that now catch what they claimed (R5-1, R5-2)
- **R5-1:** the rollback Fake's service now behaves like the real idle service.
  - A start while the marker exists idles.
  - A start of a running service changes nothing (the SCM's 1056).
  - Once the marker goes, the idle service ends or carries on as `Idle::MarkerGone.exit_code()` says, noticed at the next health look (its poll comes after the helper's start).
  - So `a_rollback_leaves_the_service_running_when_the_msi_started_it` fails on 93a26643's behaviour (`MarkerGone => Some(0)`): checked by mutation.
- **R5-2:** `a_manual_install_drops_the_journal` plants the marker and checks that the drop path removes it.
- **R5-3:** the reflowed doc line in `windows/service.rs` is wrapped.

## Not verified here
- The Windows-only calls (`boot_time`, `installer_idle`, the parent lock in `run_msiexec`, `wait_free`) aren't built off Windows. They rest on Windows CI and on their parse functions, which are tested on every OS.
- No test checks the wiring of `lock_retrying` at its two call sites. Off Windows the wait is zero, and on Windows a test would need a lock that frees at an unobservable moment. The retry itself is tested.
