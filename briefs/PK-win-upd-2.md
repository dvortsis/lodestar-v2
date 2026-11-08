# PK-win-upd-2: Windows MSI install through the SYSTEM update helper

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`. **Branch:** `slice/PK-win-upd-2` from `origin/main`. **PR title:** `PK-win-upd-2: Windows MSI install through the update helper`. **Steward:** `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

Runner brief, 2026-11-08T20:35Z. Slot 3. M6 critical path: PK-win-upd-1 (merged #630, 6554ca2c) → **PK-win-upd-2** → PK-win-upd-3 → G-m6. PK-win-portable-upd follows separately and owns disjoint files.

## The owner's decision
`docs/owner-decisions.md` §2.47 approved M6 **Q13** as the plan row gives it (`docs/plan/2026-11-06-m6-gap-audit.md` §5, Q13 row). The owner also confirmed this loosening by name: "the privileged MSI updater helper". **The plan row is the decision.** Build exactly that:
- A SYSTEM scheduled task, `Lodestar update helper`, registered by the MSI. It runs only `lodestar.exe update-helper` from Program Files, on demand and at boot. The service account may only start it.
- **Rule 1:** copy the MSI out of `<data>\updates\` into a folder only SYSTEM and Administrators can write, verify that copy, and install from it, so a file swapped after the check is never run.
- **Rule 2:** the rollback cache and the previous MSI's pins live in that folder too. Lay out the folder now; PK-win-upd-3 fills the rollback side.
- **Rule 3:** every file read from the data folder (`pending`, `rollback.json`, `history.json`, `trust.json`, the manifest) is parsed as untrusted input.
- **Rule 4:** refuse a version that isn't newer than the installed one, except the journal's rollback target with SYSTEM-held pins (PK-win-upd-3).
- **Rule 5:** channel and freshness are enforced as the engine does.
- **Rule 6:** fixed `msiexec` arguments.
- Tests cover a file swap and a replayed old manifest.

Anything beyond that row goes to the runner first: another privilege, another task trigger, or a wider ACL.

## Read first
- The plan: §1, §2.3, §4.3 (PK-win-upd-2), §5 Q13; architecture §12.2; `docs/decisions/2026-10-23-B-update-r2.md` (the reuse list, about lines 152-162, and the store-before-MSI order PK-win-upd-3 keeps).
- `docs/decisions/2026-11-08-PK-win-upd-1.md`, especially "For PK-win-upd-2 and PK-win-portable-upd":
  - `select` gives no artifact for an unknown kind, so install refuses it;
  - RC and nightly MSIs share their release's `ProductVersion`;
  - `NOT_BUILT_YET`;
  - `skipped` entries have the shape `{version, reason}`;
  - the service holds `<data>\bin\.lock` for its whole run, so the helper takes that lock before it writes, which it can do only once the service has stopped.
- `docs/decisions/2026-11-08-PK-win-restart.md` and `docs/decisions/2026-11-08-PK-manifest.md`.
- `crates/lodestar/src/update/` (`engine.rs`, `install.rs`, `windows/`, `layout.rs`, `journal.rs`, `pending.rs`), `packaging/windows/main.wxs` and `crates/update/` (read-only).
- `runner/standards.md` is the bar.

## Owns
- `packaging/windows/main.wxs`
- `crates/lodestar/src/update/windows/msi.rs` and `windows/helper.rs` (new)
- `crates/lodestar/src/update/engine.rs` and `install.rs`
- `crates/lodestar/src/cli/update_helper.rs` (new)
- `crates/lodestar/tests/update_windows_msi.rs` (new)
- `docs/decisions/2026-11-08-PK-win-upd-2.md`
- **One-line shared edits:** `update/mod.rs`, `update/windows/mod.rs`, `cli/mod.rs`.
- **Also allowed:**
  - `update/windows/check.rs`, for the skip-trim fix below;
  - correcting the PK-win-upd-1 decisions file's `trust_damaged` line;
  - regenerated `openapi.json` and `schema.d.ts`, only if the `/updates` contract changes (regenerate, never hand-merge).
- **Ask the runner first** for any `Cargo.toml` change, a new crate or anything else.

## Required, from #630's reviews (runner 2026-11-08)
1. **Refuse an unchecked bin.** The helper refuses to install from a bin folder it hasn't checked. A data folder set by `--config` or `LODESTAR_DATA_DIR` isn't ACL-checked today (`layout.rs` module docs).
2. **Refuse an unparsed build version.** The helper refuses to install when the running build's version fails to parse. Today it falls back to 0.0.0 (`update/mod.rs`), which would make every version "newer".
3. **Port the Unix skip trim.** Before you write `failed` or `manual` entries into `skipped`, port Unix `pending::trim` to `windows/check.rs` `skip()`: drop the oldest `user` skips first, and never drop `failed` or `manual`. Correct its "as on Unix" comment. Add a test where 64 user skips don't push out a `failed` entry.
4. **Retry a briefly held rename.** Retry a rename that fails with a sharing or access-denied error for a short time, as the index renames do. Antivirus and the indexer briefly hold fresh files (architecture B5a). Bound it, with no fixed sleeps in tests.
5. **Confirm `ProgramFiles`.** MSI detection depends on `ProgramFiles` being set in the service's environment. Confirm it is, or fall back to the known folder. Unknown fails safe, but would then refuse every MSI install.
6. **Keep the helper's environment clean.** Don't pass `LODESTAR_RELAUNCH_TOKEN` (PK-win-restart) to anything the helper or `msiexec` starts.

## Done when
- **On every OS:**
  - selection and refusal (unknown kind, not newer, wrong channel, stale manifest, unparsed build version, unchecked bin);
  - the copy-then-verify order;
  - fixed `msiexec` arguments.
- **On the Windows CI job:**
  - a file swapped in `<data>\updates\` after the check is never installed;
  - a replayed, validly signed older manifest is refused;
  - the staging folder's ACL (SYSTEM and Administrators only) is checked, and a non-private folder is refused;
  - the helper can't take `.lock` while the service holds it.
- **The MSI:**
  - `main.wxs` registers the task with exactly the plan row's triggers and principal;
  - a WiX build check on the Windows job, if the existing packaging job already builds the MSI.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- **The workspace forbids `unsafe`.** If an ACL or task API truly needs Win32 calls or a new crate, stop and tell the runner the options.
- **Size:** about 1,000 lines (the plan). Past about 1,500, tell the runner first.
- **Process:**
  - push once per round;
  - no fixed sleeps (condition waits with deadlines of at least 10 s);
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green on all platforms.

## These go to the runner
- Any privilege, trigger or principal beyond the Q13 row.
- `AllowDowngrades`, or any other downgrade path.
- A manifest format, channel or target-kind change.
- Loosening any layout guarantee.

## Coordination
- **Running now:**
  - CMP-6: `crates/api/src/companions.rs`, `crates/lodestar/src/system.rs`, `crates/lodestar/src/companions.rs` and `services.rs`;
  - OP-1: `.github/workflows/fleet.yml`, `infra/fleet/**`, the infra job in `ci.yml`;
  - X-wk-busy-retry: the worker runtime and the logging catalogue;
  - X-cloud-infra-2: `infra/**`.

  None of them touch your files.
- **Ready but not started:**
  - PK-win-portable-upd owns `windows/portable.rs` and makes its own one-line edit to `windows/mod.rs`;
  - X-updates-api-text owns `crates/api/src/updates.rs`, `dto/updates.rs`, `openapi.json` and `schema.d.ts`. If your PR changes the `/updates` contract, say so in the PR so the runner can order the two.
- PK-win-upd-3 follows you and owns `main.wxs` and `helper.rs` next, so leave the helper easy to extend with a `--resume` and rollback path.
