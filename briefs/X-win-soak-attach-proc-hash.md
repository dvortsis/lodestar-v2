# Brief: X-win-soak-attach-proc-hash (Sonnet; testing priority)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-win-soak-attach-proc-hash` from `origin/main` (#777 merged as c904f883). **PR title:** `X-win-soak-attach-proc-hash: the Windows soak checks the running process is the file it hashes`.

**Why now:** testing comes first (owner §2.62). The M8-WIN-1 review's optional O1 (`git show origin/runner-state:reviews/rv-777-908a48c6-delta.md`, Optional 1):
- In service mode, `cr0-soak.ps1` hashes the file named by the service's ImagePath at attach and on each sample. It never checks that the running service process was loaded from that file.
- If an MSI upgrade replaces the file but leaves the old process running (files in use, reboot pending), the soak binds the RC hash while it measures the old binary. The upgrade record catches this today, but only indirectly.
- `-Exe`'s hash (`$originalExeHash`) can also differ from the bound hash, so `run.sha256` and `run.version` would describe a different file.

## What to build
1. **At attach in service mode,** stop with a clear message and a non-zero exit when either is true:
   - the SHA-256 of the running service process's image (its main module path, via the existing helpers in `cr0-common.ps1`) differs from the ImagePath hash;
   - `-Exe`'s hash differs from the ImagePath hash.

   Record neither path in the evidence; hashes only, as today.
2. **Put the comparison in a small helper in `cr0-common.ps1`** that takes injectable probes, like the existing `Probe` hashtables, so it can be tested without Windows service calls.
3. **Tests** in `docs/soak/tools/test_cr0_collector.ps1`, using real temp files as #777's I3 fix does (only the service and process calls are stubbed):
   - same file → passes;
   - a different running binary → refuses;
   - a different `-Exe` → refuses.
4. **Runbook:** one line in `docs/soak/v1-windows.md` saying the soak refuses to start if the running service isn't the installed file, and what to do (restart the service, or reboot if one is pending). Plain and short, per the user-docs tone.
5. **Decisions file** `docs/decisions/2026-11-12-X-win-soak-attach-proc-hash.md`: what it refuses and why. A runner call (§2.33).

## Owned paths
`docs/soak/tools/cr0-soak.ps1` (the attach block only), `docs/soak/tools/cr0-common.ps1` (the new helper only), `docs/soak/tools/test_cr0_collector.ps1`, `docs/soak/v1-windows.md` (one line), the decisions file. Don't touch `analyze_cr0.py`: X-soak-screen-calibrate owns it right now.

## Checks before the push
- `pwsh` is not in the cloud container. CI's `soak collector (Windows)` job runs `test_cr0_collector.ps1`, so write the tests to run there. Read each changed PowerShell line twice for syntax.
- Re-read the diff: no paths, no 12-digit numbers, no local times.

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. When ci-ok (including `soak collector (Windows)`) is green, message the runner "ready <pr> <full 40-char sha>".
