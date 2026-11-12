# M8-WIN-1: Windows 1.0 soak: runbook, and analyzer checks for service restart, upgrade and uninstall
**Model:** Sonnet (helper). A fresh Opus reviewer follows.

Runner brief, 2026-11-12T18:51Z. Epic M8, on the path to G-v1-windows (the owner runs the soak on their own Windows PC). Deps ext:v051-soak-verdict and ext:owner-m8-q3 hold.

## Read first
- `docs/plan/2026-11-06-m8-hardening.md`: the "Windows soak" row (~27), the slice row (~61), G-v1-windows (~87), and the owner's OQ3 answer recorded under `docs/owner-decisions.md` (search "m8-q3" / "OQ3").
- `docs/decisions/2026-10-30-CR-0-acceptance.md` (the memory rule) and `docs/soak/tools/analyze_cr0.py` (`MEMORY_FIT`), `cr0-soak.ps1`, `cr0-common.ps1`, `test_analyze_cr0.py`.
- Use the project skill `user-docs` for the runbook's wording (the reader may not know Windows services).

## Do
1. **Analyzer** (`analyze_cr0.py`): add three checks beside the memory rule, each a pass/fail line with its evidence: **no service restart** during the window (from the collector's service start time or PID), **upgrade from the last release** (the service comes back on the new version with its data), and **clean uninstall** (service gone, program files gone, data kept or removed as the uninstaller says). **`MEMORY_FIT` and its numbers stay exactly as they are.** Older evidence without the new fields still analyzes (reported absent, not failed).
2. **Collector** (`cr0-soak.ps1`, `cr0-common.ps1`): record what the checks need (service PID/start time each sample; version before and after the upgrade step; the post-uninstall state). Nothing private: no paths with the user's name, no IPs, no credentials (follow the existing sanitizer).
3. **Tests** (`test_analyze_cr0.py`): pass and fail fixtures for each check, plus old evidence without the fields.
4. **Runbook** `docs/soak/v1-windows.md`: the owner's steps on the RC MSI in service mode, start to verdict, with the exact commands, how long each phase takes, and what to send back (and what never to send: raw logs, the database, credentials stay private).

## Owns
- `docs/soak/tools/cr0-soak.ps1`, `docs/soak/tools/cr0-common.ps1`, `docs/soak/tools/analyze_cr0.py`, `docs/soak/tools/test_analyze_cr0.py`, `docs/soak/v1-windows.md`.
- **Ask the runner first** for anything else. Don't touch `crates/**`, `packaging/**`, `.github/**`, `web/**`.

## Done when
- The three checks pass on good fixtures and fail on bad ones; `MEMORY_FIT` is unchanged (a test asserts its constants); old evidence still analyzes; the runbook is complete.
- **Checks:** `python3 -m pytest docs/soak/tools` (or the way CI runs them; check `.github/workflows`), and PowerShell parses cleanly if `pwsh` is available (say so if not).
- **Size:** about 250 lines (plan).
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, never merge. Branch `slice/M8-WIN-1`; PR title `M8-WIN-1: Windows 1.0 soak runbook and analyzer checks`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready <pr> <full sha>" once CI is green, using the Claude Code Remote MCP `send_message` tool.
