# rv-663-cb9fdeb6: MERGE (fresh Opus review plus /security-review). Merged as a2450849.

## Non-blocking, carried into X-win-upd-1920
1. **The share-mode-0 window is understated.** IMAGE_WAIT never refuses, so a hold taken any time before the copy works, as long as it lasts through StartServices. Reword the decisions file.
2. **A rare edge in the forward 3010 bound.** Two clock corrections over 60 s within one boot end the wait early, and `to` then misses probation. This is accepted.
   - Observation: re-offering `to` after a failed 3010 replacement may give a maintenance-mode exit 0, and probation could then loop. This predates the PR.
3. **The recovery text at `rollback.rs:648-650` misattributes halts that come from journal-write failures.** The branch is untested; mutation M10 survived.
4. **`remove_marker`'s early return when there's no marker and no lock is untested.** Mutation M8 survived.
5. **The Windows-only wiring has no test:** `hold_parent`, the `copy()` wait, `boot_time`, `installer_idle` and the spawn mapping.
6. **`boot_time`'s PowerShell call has no timeout.** It's bounded only by PT2H. Add `-OperationTimeoutSec`. Also, `Windows::powershell` duplicates `authenticode()`'s command setup.
7. **`the_wait_for_the_parent_is_bounded` waits about 9.4 s of real time.** Raise it to 11 s or more.

## Verified
- Every bound is a named constant.
- Paths are confined to SYSTEM-only staging.
- The sweep races are handled.
- It fails closed, with no secrets in logs and no new privilege.
- 13 of 15 mutations were caught.
