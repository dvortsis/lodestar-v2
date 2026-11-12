Verdict: MERGE

# Review: PR #792 X-win-soak-attach-proc-hash at 47eba3fe

Fresh Opus review against the brief (`runner-state:briefs/X-win-soak-attach-proc-hash.md`), rv-777's O1, `runner/standards.md` and CLAUDE.md. Base 2e83895b; merge-tree with origin/main is clean, and nothing on main since the base touches these files.

**Tests weren't run:** `pwsh` isn't installed in this container. I read every changed PowerShell line for syntax and behaviour instead (notes below). I didn't check CI status (no network calls); the runner should confirm `soak collector (Windows)` is green on 47eba3fe before merging.

## Blocking
None.

## Important

**I1. A gap between the check and the binding can let a restarted service through.** Files: `docs/soak/tools/cr0-soak.ps1:32` and `:145-148`.
- `Assert-SoakAttachBinding` checks one service process (pid and start time) and one ImagePath hash at line 32 and then discards both.
- The evidence is bound later, at lines 145-148. Those lines re-read `Get-SoakServiceIdentity` and `Get-SoakServiceImage`, after the disk checks and the copy of `-Exe` into `bin\`.
- Suppose the service restarts on a different file in that gap (an MSI finishing, a manual restart). Then:
  - `run.process_identity`, `run.service_image_sha256` and every sample all describe the new process;
  - `run.sha256` and `run.version` still describe the file that was hashed at line 30.
- `analyze_cr0.py` never compares `run.sha256` with `run.service_image_sha256` (its build_binding check only compares sample hashes with the attach hash, at lines 881-896). Nothing downstream would catch the mismatch, and it is the exact case the brief names.
- The gap is a few seconds and needs a coincident restart, so this isn't blocking. The fix is cheap and inside the owned attach block.

Fix:
1. Have the helper return what it checked: `@{ pid = $id.pid; start_ticks = $id.start_ticks; sha256 = $registered }`, with `$id = & $p.Identity $Name`.
2. Capture it at line 32: `$attach = if ($serviceMode) { Assert-SoakAttachBinding $ServiceName $Exe }`. The capture also stops the return value printing to the console.
3. At lines 145-148, throw unless all of these hold:
   - `$svcId.pid -eq $attach.pid`;
   - `$svcId.start_ticks -eq $attach.start_ticks`;
   - `$img.sha256 -eq $attach.sha256`;
   - `$img.sha256 -eq $originalExeHash`.

   Use a message like "The service changed while the soak was starting. Start the soak again."
4. Add one assert in `test_cr0_collector.ps1` that the returned hashtable carries the registered hash and the stubbed pid.

**I2. The Limit section names the wrong second line of defence.** File: `docs/decisions/2026-11-12-X-win-soak-attach-proc-hash.md:23`.
- The section says "the upgrade record's `image_matches_installed` stays the second line of defence". That record is computed the same way as this check (`Save-SoakUpgrade`, `cr0-common.ps1:277,283`: the hash at the process's reported path against the hash of `-Exe`), so it has the same blind spot for a renamed-and-replaced image.
- What actually catches an old process kept running is `service_restarted`: start ticks later than before the upgrade, which `analyze_cr0.py`'s upgrade check requires.
- The reboot-pending clause also implies a false pass that doesn't happen. If the replacement waits for a reboot:
  - the file and the process are both still the old binary, so the binding is accurate;
  - the run fails the upgrade check (`to_image_sha256` equals `from_image_sha256`).

Fix: rewrite the last two sentences along these lines. "If Windows reports the original path for a process whose file was renamed and replaced, the hashes match although the process runs the old binary. The upgrade record's `service_restarted` (the service must have started after the upgrade) is what catches that. If the new file waits for a reboot, the file and process are both the old binary, so the evidence stays consistent and the upgrade check fails because the hash didn't change."

## Optional

**O1. Some unreadable-image failures don't get the clear message.** File: `docs/soak/tools/cr0-common.ps1:220`. The default `ProcessImage` probe is `(Get-Process -Id $procId).Path`:
- If the service exits between `Identity` and `ProcessImage`, `Get-Process` throws its own error, which includes the pid.
- If `.Path` throws instead of returning `$null`, the raw access error comes through.

Both fail closed (non-zero exit, no run folder), but the message isn't the clear one. Fix: `try { (Get-Process -Id $procId -ErrorAction Stop).Path } catch { $null }`, and word the null-case message "Cannot read the running service program file (it stopped, or this shell isn't elevated)."

**O2. The runbook line doesn't say how to restart the service.** File: `docs/soak/v1-windows.md:72`. It says "restart the service" but gives no command. Give it inline: `Restart-Service Lodestar`, from the elevated PowerShell.

The last sentence ("The check compares the file's hash at the running program's path; …") is implementation detail for this reader. Shorten it to "Restart after every upgrade even if the check passes."

**O3. The tests don't prove the stubbed pid reaches the process probe.** File: `docs/soak/tools/test_cr0_collector.ps1:126`. The `ProcessImage` stub ignores its pid, so the tests would pass even if the helper never passed the `Identity` pid through to `ProcessImage`. Make the stub return `$Running` only when `$procId -eq 7`, and `$null` otherwise.

**O4. Follow-up.** The decisions file defers a start-time or pending-rename check to a follow-up. The runner should queue it so it isn't lost. A file-time check would need care: NTFS file-name tunnelling carries the old file's creation time over to a replacement created within about 15 s.

## Checks

1. **Refusal correctness:**
   - **Order:** the helper reads the ImagePath hash, then hashes the running process's reported path, then hashes `-Exe`. Any differing hash throws, so it fails closed. The call is at `cr0-soak.ps1:32`, before `New-Item $Root`, so a refusal writes nothing.
   - **ImagePath quoting and arguments:** handled by the existing `Get-SoakImagePath`: the quoted form is taken first, the unquoted form is cut at the first `.exe`.
   - **8.3, relative or case-variant paths:** irrelevant to correctness, because only hashes are compared. A relative ImagePath that doesn't resolve throws, so it fails closed.
   - **Hash comparison:** `-ne` between `Get-FileHash` hashes, which are uppercase and case-insensitive. This is right.
   - **Unreadable image:** an empty or null path gives the "elevated" refusal. The other unreadable cases are in O1.
   - **TOCTOU:** the window inside the helper only ever causes a refusal. The window after the helper is I1.
   - **Known limit:** a renamed-and-replaced image is documented (see I2 for the wording).
2. **Structure:**
   - The helper is in `cr0-common.ps1` with an injectable `$Probe`, the same pattern as `Save-SoakUpgrade`.
   - Tests use real temp files: only Identity, ProcessImage and ServiceImage are stubbed, and `Hash` is the real `Get-FileHash`. They cover:
     - match;
     - a different running binary;
     - a different `-Exe`;
     - an unreadable image;
     - a differing ImagePath hash.

     Each refusal test checks its message.
   - The `finally` cleanup includes `$other`.
   - Syntax read twice and looks valid. In particular:
     - `{ … }.GetNewClosure()` inside a multi-line hashtable literal;
     - `& $p.ProcessImage (& $p.Identity $Name).pid` (member access on a parenthesised argument);
     - the leading `-` in the `-like '-Exe is not*'` pattern.

     The file's parse test covers all three changed scripts.
   - Not run (no pwsh).
3. **Scope:** five files, all owned:
   - the decisions file;
   - the `cr0-common.ps1` helper only;
   - two lines in `cr0-soak.ps1`'s attach validation;
   - the test file;
   - one runbook paragraph line.

   `analyze_cr0.py` is untouched.
4. **Privacy:** no hashes, paths, user names or machine names in committed files. The error messages carry no paths. The test fixtures live in a temp directory and are deleted.
5. **Hygiene:**
   - No model identifiers in code or docs; the commit trailers are the harness-supplied ones.
   - No numbers of 12 or more digits.
   - No clock times; the date-only filename is UTC.
   - No sleeps or waits added.
