# X-win-upd-reoffer decisions

The follow-ups from #677's review (`reviews/rv-677-f4243e6c.md`). The brief is from the runner, dated 2026-11-11. This file supersedes item 2's residual in `2026-11-11-X-win-upd-1920.md` (the 3010 re-offer loop), which is now fixed.

No new privilege, trigger or principal is added beyond §2.47's Q13 row.

## A 0 exit that left the old program on disk is skipped (#663 item 2)
- **The case:** a forward install's restart never put `to` in place, and the 3010 bound ended it as `install_interrupted`. The service then offers `to` again.
  - Windows Installer already counts `to` as installed, so the next `msiexec /i` is a maintenance run that exits 0 and replaces nothing.
  - The helper used to report `installed` for `to` while `from` was serving, and the next check offered `to` again.
- **Now:** exit 0 while the program on disk is still `from` is skipped with a plain reason (`install_failed`, with `result.json`'s `detail` and the Event Log).
  - The reason says Windows Installer reported the version installed, but the program in Program Files is still `from`. To install it, an administrator repairs or removes Lodestar in Settings > Apps, then installs the MSI by hand.
  - The journal goes, the start type goes back, and the old service is started.
  - The MSI isn't kept as `current\`, since it isn't the installed program.
- **"The program on disk":** `System::on_disk`.
  - On Windows it's this process's own version. The run that judges an install's answer is a run of `%ProgramFiles%\Lodestar\lodestar.exe` that reads the answer after `msiexec` ended: the copy's, the boot run, or one the service starts. So it's the program as `msiexec` left it, and a SYSTEM-owned fact. A program still loaded while `msiexec` ran makes the replacement wait for a restart (3010, with `/norestart` and the Restart Manager off), never an exit 0, so the check can't misfire on a legitimate install.
  - The 3010 decision ("wait for the restart only while `from` is on disk") now uses the same call.
- **The tests model it:**
  - The Fake's `msiexec` puts its MSI's version on disk on exit 0 (`lodestar-<v>.msi`), unless `stays`, which models a maintenance run. Any other exit leaves it.
  - A hand-off's next run uses the program the test puts on disk (`env.ctx.running`).
  - "Installed" now means the new version is the one on disk. Three hand-off tests judged `to` while `from` was still on disk. One ends `installed`, and the other two go through probation into a rollback. They now put `to` there first, as the real copy does, and every assertion is kept.
- **Why skip rather than retry:** the state can't fix itself. Windows Installer's record and the files disagree, and only a repair or a manual install reconciles them. Skipping ends the loop, and the reason says what to do.

## A linear 1920 matcher (#677 review, important item 1)
- `error_1920` found each ` 1920. ` and then searched the rest of the line for the service, which is quadratic. #677's review measured a 1 MiB line of `a 1920. ` at about 222 s in debug. Here it took about 6 s on a faster machine, and about 100 s at 4 MiB.
- Now the service's last whole-word place is found in one pass. Each ` 1920. ` is then a constant-time check against it (`last >= at + len`), so the answer is the same and the cost is linear.
- Tested: an adversarial line of `a 1920. ` with no name, with the name at the end and with the name at the start gives the old matcher's answers. The line is 4 MiB with a 10 s deadline, so the test tells the two matchers apart: the quadratic one took about 6 s at 1 MiB in debug, about 100 s at 4 MiB, while the linear one stays well under a second.

## The service's name is matched as a whole word (review nit)
- `contains` matched `Lodestar2` or `MyLodestar` too.
- Now the characters on either side of the name must not be a letter, digit, `_` or `-`. So `'Lodestar'`, `"Lodestar"`, `(Lodestar)` and `Lodestar.` match, but `Lodestar2`, `Lodestar-old` and `old-Lodestar` don't. Tested.
- **When the name is stuck to text in a language without spaces:** it isn't matched, and the failure is treated as an ordinary one, as before #677. That fails safe.

## Saturating counters (review nit)
- The forward install's `attempts + 1` and the rollback's `j.blocked + 1` and `+= 1` now use `saturating_add`.
- `blocked.json` and the journal are SYSTEM-only, so a huge value means damage, never an attack. A count of `u32::MAX` is now an ordinary failure, not an overflow (a panic in debug). Tested for both.

## What injected log text could do (review nit)
- The `msiexec` logs are in staging, which only SYSTEM and Administrators can write. Windows Installer writes the text, including property values and paths.
- Suppose text that matches the pattern reached a log line somehow, such as a value the MSI logs. The most it can do:
  - **Forward install:** turn a genuine 1603 into at most 2 more attempts (`MAX_BLOCKED` − 1). Each is a whole install, re-verified from the signed envelope: trust floor, pins, Authenticode, the stop and the lock. Then the version is skipped as before.
  - **Rollback:** turn a halt into at most 2 more reinstalls of the pinned previous MSI, with the service disabled and stopped and the marker in place throughout. Then it halts as before.
- **Why that's safe:** the match only ever makes the outcome a bounded retry of something the helper was already about to do. It never skips a check, never runs anything new, and never starts the service during a rollback. A false negative is the old behaviour.
- **"Windows Installer writes UTF-16 logs" is unverified** (review nit). The decoder handles UTF-16 (with or without the byte-order mark) and UTF-8, and a log it can't read fails safe as an ordinary failure.

## Left for later (self-review polish)
- **Step 5 of the rollback** still reads `ctx.running` directly, not `System::on_disk`. That's the same value in production. Moving it over belongs in a later rollback slice.
- **`result.json` shows `code: "0"` for this skip,** with `install_failed`. The `detail` text explains it. Any UI or `check.rs` owner that shows the exit code should know an exit 0 can be a failure here.
