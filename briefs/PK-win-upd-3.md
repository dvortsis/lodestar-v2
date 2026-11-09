# PK-win-upd-3 (Opus, security-sensitive): Windows MSI rollback and probation

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09 at about 00:15Z. It continues in PK-win-upd-2's session, which merged as #640 (4d65ab23). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/PK-win-upd-3` from main 4d65ab23 or later. PR title `PK-win-upd-3: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/architecture.md` §12.2.
- `docs/decisions/2026-10-23-B-update-r2.md`, especially the store-before-MSI order from P-release's review, around line 162.
- Your own PK-win-upd-2 decisions file, and #640's merged code: `helper.rs`, `msi.rs`, the pinned folders, `current\` and `previous\`, the request ids, `result.json`, and the `Unhealthy` outcome left for this slice.
- The `PK-win-upd-3` entry in `runner/slices.yaml`.

## Owns
- `packaging/windows/main.wxs`;
- `crates/lodestar/src/update/windows/rollback.rs`;
- `crates/lodestar/src/update/windows/helper.rs`;
- `crates/lodestar/src/update/journal.rs`;
- `crates/lodestar/src/update/rollback_cache.rs`;
- `crates/lodestar/src/update/probation.rs`;
- `crates/lodestar/tests/update_windows_msi.rs`;
- `docs/decisions/2026-11-09-PK-win-upd-3.md`;
- **shared, a one-line edit each if needed:** `crates/lodestar/src/update/windows/msi.rs` and `update/windows/mod.rs` (`msi.rs` for hardening item 1 below).

## What it is
- **Store before MSI.** On an unhealthy probation, restore the store snapshot first, then reinstall the previous MSI from the SYSTEM-only folder, with its pins re-verified before it runs.
- **The service stays manual during rollback,** so the SCM can't restart a half-rolled-back install.
- **`--resume` at boot,** so a rollback interrupted by a crash or reboot finishes.
- **A journal** recording every step, crash-safe: a new temporary file, then rename, under the pinned folder.

## Carried from #640's security review (non-blocking there, do them here)
1. **Quote the scheduled task's `<Command>`** (`msi.rs:489`), so the task never depends on how the command line is rebuilt.
2. **The helper's version floor on a first install.**
   - On a first install, the helper's forward-only trust mark has seen no manifest yet.
   - So also read the data folder's `trust.json` mark as a raise-only floor, or record in the decisions file why that isn't needed.
   - The aim: a replayed older, still-unexpired manifest can't install a withdrawn release.
3. **The race in `secure-datadir.ps1`** between its link scan and `Set-Acl` propagation. Either confirm the propagation doesn't follow junctions, or have the helper check that nothing still runs as the service account before msiexec. Record which.
4. **Keep `result.json` display-only:** the service can write it, so rollback decisions never read it.

## Tests
- Rollback after an unhealthy probation, the store restored before the MSI.
- An interrupted rollback finished by `--resume`.
- A tampered previous MSI or wrong pin refused before it runs.
- The journal survives a crash at each step.
- No fixed sleeps (condition waits with deadlines of 10 s or more), and no skipped tests.
- Never add tokio `test-util` or a `testing` feature to the `lodestar` package.

## Rules
- No model identifiers.
- Flag any choice the plan left open in the decisions file; the runner flags them to the owner (§2.19).
