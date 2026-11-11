# X-win-upd-1920: the MSI updater handles 1920 from a held file, plus the #663 review follow-ups (Opus, core, /security-review)

**Why:** #663 (X-win-upd-bounds) merged with a residual. A local user who holds `lodestar.exe` open with share mode 0 through StartServices gets error 1920 and then 1603. The forward install then skips the version for good, and a rollback reinstall halts. IMAGE_WAIT never refuses on a held image, so the hold can start at any time before the copy; no race is needed. The review's notes are in `reviews/rv-663-cb9fdeb6.md`.

## Build
1. **Tell 1920 apart from a generic 1603,** from the msiexec log the helper already keeps. A held-file failure is reported as a retryable `install_blocked` (or the existing nearest code; say which), not as `install_failed`.
   - **It must not skip the version.** Retry it on the next helper run, with a bounded count from a named constant. After that, record it as failed with a plain reason.
   - **For rollback:** retry the reinstall a bounded number of times before halting.
   - **The service stays in a safe state** at every step, as #663 does now.
   - **Never parse more of the log than needed.** Treat it as untrusted input: cap the bytes read and match a fixed pattern.
2. **#663 review items:**
   - (3) the recovery text at `rollback.rs:~648-650` names the real cause when a journal write failed. Test that branch.
   - (4) a test for `remove_marker`'s no-marker, no-lock early return: no lock wait.
   - (6) `-OperationTimeoutSec` on `boot_time`'s `Get-CimInstance`, and fold `Windows::powershell` into one shared command setup with `authenticode()`.
   - (7) the parent-wait test budget at 11 s or more.
   - (1) reword the decisions file for the window.
   - (2) investigate the 3010 re-offer loop and record the finding. Fix it only if it's small and in scope; otherwise state it as a residual.
3. **Tests:** the platform-neutral parse and decision logic with fixtures, and the Windows paths in `tests/update_windows_msi.rs` where feasible. No fixed sleeps; use condition waits of at least 10 s or injected clocks. Show that each guard's test fails when the guard is removed.
4. **Decisions file:** `docs/decisions/2026-11-11-X-win-upd-1920.md`.

## Owns
- `crates/lodestar/src/update/windows/msi.rs`
- `crates/lodestar/src/update/windows/rollback.rs`
- `crates/lodestar/src/update/windows/helper.rs`
- `crates/lodestar/src/cli/update_helper.rs`
- `crates/lodestar/tests/update_windows_msi.rs`
- its decisions file

## Not owned
- `update/windows/portable.rs`, `check.rs`, `windows/mod.rs`, `services.rs`, `main.rs` and `cli/util.rs`: PK-win-portable-upd, in review as #659.
- `system/restart.rs`: X-flake-portable-restart-2.

No new privilege beyond §2.47 Q13.

Branch `slice/X-win-upd-1920`; PR title "X-win-upd-1920: <title>". Follow the steward skill.
