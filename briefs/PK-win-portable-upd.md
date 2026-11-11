# PK-win-portable-upd (Opus, /security-review): Windows portable update and rollback

Assigned by the runner on 2026-11-11. Its deps (PK-win-upd-1, PK-win-restart) are merged, and X-win-restart-followups(-2) (#649, #651) changed the relaunch it reuses; both are on `main`.

## Read first
- The `PK-win-portable-upd` entry in `runner/slices.yaml`: its notes are binding (owner-only `<data>\bin` through `windows::acl`, refused when not private, in place before anything is written or run; `LODESTAR_RELAUNCH_TOKEN` kept out of anything the updater spawns; the Unix skip-list trim rule from PK-win-upd-2).
- `docs/plan/2026-11-06-m6-gap-audit.md` and architecture §12.2; the decisions files for PK-win-upd-1/2/3, PK-win-restart and X-win-restart-followups(-2).
- `crates/lodestar/src/update/windows/mod.rs`, `rollback.rs` (the MSI rollback, for the probation pattern), `crates/lodestar/src/system/restart.rs`.

## Owns
- `crates/lodestar/src/update/windows/portable.rs`, `crates/lodestar/tests/update_windows_portable.rs`, `docs/decisions/2026-11-11-PK-win-portable-upd.md`.
- One-line edit to `update/windows/mod.rs`. X-win-upd-bounds runs at the same time in `rollback.rs`, `helper.rs`, `layout.rs`; don't edit those. If you must, ask the runner first.

## Build
Verify the download (as the MSI path does), rename the running `.exe` aside, put the new one in place, relaunch through PK-win-restart's relaunch, probation (healthy within the bound, or rename back and relaunch the old one). Runs as the user who unpacked the zip: no helper, no new rights. Crash safety: every step is resumable from a persisted state, and a failure at any point leaves a runnable `.exe` in place. Tests cover each failure point.

## Rules
No new privilege. Never approve tokio `test-util`/`testing` on the `lodestar` package.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/PK-win-portable-upd` from the latest `origin/main`; PR title `PK-win-portable-upd: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `PK-win-portable-upd: PR #<n> ready for review, head <sha>` with CI green. Blocked: `PK-win-portable-upd: blocked on <what>`. The runner reviews and merges; you never merge.
