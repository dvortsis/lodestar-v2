# X-win-restart-followups: Windows restart follow-ups (portable successor before the 202, the portable console message, a fixed-port relaunch test, the relaunch flake)

**Model:** Opus. **Review:** a fresh Opus reviewer. Add `/security-review` if the token handling or process start changes. **Branch:** `slice/X-win-restart-followups`, cut from `origin/main`. Its deps, PK-win-restart (#605) and CMP-6 (#639), have both merged. **PR title:** `X-win-restart-followups: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/decisions/2026-11-08-PK-win-restart.md`, especially "The portable build" (the known gap: the 202 comes first), the token note, and the portable launcher note (line 34).
- `runner/slices.yaml`, the `X-win-restart-followups` entry and its notes. That entry is the spec. Also read the `PK-win-portable-upd` entry: it reuses the relaunch you change.
- The code on main:
  - `crates/api/src/system.rs` (`POST /system/restart`, the `SystemApi` trait);
  - `crates/lodestar/src/system/restart.rs`;
  - `crates/lodestar/src/windows/relaunch.rs` (read only);
  - `packaging/windows/portable/Start Lodestar.cmd`;
  - `crates/lodestar/tests/windows_service.rs` (`a_portable_restart_starts_a_new_copy`, line ~575);
  - B2c's response-drop hook.
- `runner/standards.md` is the bar.

## Owns
- `crates/api/src/system.rs`
- `crates/lodestar/src/system/restart.rs`
- `packaging/windows/portable/Start Lodestar.cmd`
- `crates/lodestar/tests/windows_service.rs`
- `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts`, regenerated with the repo's tooling only, never by hand.

**Shared edits the runner allows:**
- The matching `SystemApi` impl change wherever the lodestar crate implements it (likely under `crates/lodestar/src/system/`).
- New log events in the lodestar catalogue, with their golden. Codes only, never paths or arguments.
- One `docs/decisions/2026-11-09-X-win-restart-followups.md`.

**Not yours:**
- `update/**` and `windows/service.rs` (PK-win-upd-3 is open on them);
- `docs/user/**` (PK-guide-3);
- web components. A string change goes to the runner first.

## What to build
1. **The portable successor before the 202.** Start the portable successor before answering `POST /system/restart`. If the spawn fails, answer `409` with a stable code, and log it.
   - Keep the service and Unix behaviour unchanged.
   - Document the 409 in the API, and regenerate openapi.json and schema.d.ts.
   - Test it: a spawn that fails gives 409 and no exit; one that succeeds gives 202 and the hand-over.
2. **`Start Lodestar.cmd`.** It no longer prints "Lodestar stopped." and pauses while the relaunched copy runs in the same console.
   - Either loop while the exit code says a restart happened, or reword the message so it's true.
   - Keep Ctrl+C graceful.
   - Record what you couldn't verify on a Windows desktop.
3. **A fixed-port relaunch test** for the port-release wait.
4. **The relaunch flake.** Diagnose and fix the intermittent `a_portable_restart_starts_a_new_copy` failure, "no new copy served" (seen on #636 and #637; it passes on main and on re-run).
   - First make the test dump the relaunched copy's `lodestar.jsonl` and stderr on failure.
   - Then root-cause it. Don't widen a timeout without a reason.
   - Waits are conditions with deadlines of at least 10 s. No fixed sleeps.
5. **Notes for PK-win-portable-upd, in the decisions file:** icacls children inheriting `LODESTAR_RELAUNCH_TOKEN` is harmless; any code that starts `lodestar.exe` must `env_remove` the marker or go through `relaunch::command`.

## Checks before "ready"
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
- the openapi drift check;
- Windows CI green, including `windows_service.rs`;
- in `web/`, `npm run check` if the regenerated schema touches it.

Push once per round, then message the runner "ready" with the head. PK-win-portable-upd reuses the relaunch, so whichever lands second brings main in and re-runs the restart tests.
