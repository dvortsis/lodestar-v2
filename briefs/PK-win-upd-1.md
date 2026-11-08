# PK-win-upd-1: Windows update check and notify, in the service

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`. **Branch:** `slice/PK-win-upd-1` from `origin/main`. **PR title:** `PK-win-upd-1: Windows update check and notify in the service`. **Steward:** `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

Runner brief, 2026-11-08T17:18Z. Slot 3. M6 critical path: PK-manifest (merged #618, 4f36875d) → **PK-win-upd-1** → PK-win-upd-2/3 and PK-win-portable-upd → G-m6.

## Read first
- `docs/plan/2026-11-06-m6-gap-audit.md`: §1 (fixed constraints: the `NT SERVICE\Lodestar` virtual account), §2.3, §4.3's **PK-win-upd-1** bullet (the spec), §5 Q13 (why install is NOT in this slice).
- Architecture §12.2; `docs/decisions/2026-10-23-B-update-r4.md` (lines ~24-25, 110: `unavailable: "windows_helper"`) and `docs/decisions/2026-10-23-B-update-r2.md` (~152-162: the reuse list).
- `docs/decisions/2026-11-08-PK-manifest.md`: what the manifest now carries (MSI, portable zip, bare binaries; channels stable/beta; `LODESTAR_CHANNEL`).
- `crates/lodestar/src/update/`: `layout.rs`, `trust_file.rs`, `check.rs`, `unavailable.rs`, `window.rs`, `policy.rs`, `mod.rs`; `crates/update/` (read-only).
- `runner/standards.md` is the bar.

## Owns
- `crates/lodestar/src/update/windows/mod.rs`, `crates/lodestar/src/update/windows/check.rs` (new)
- `crates/lodestar/src/update/layout.rs`, `trust_file.rs`, `unavailable.rs`, `check.rs`, `mod.rs`
- `crates/lodestar/tests/update_windows.rs` (new)
- one `docs/decisions/2026-11-08-PK-win-upd-1.md`
- Regenerated `web/src/lib/api/openapi.json` and `schema.d.ts` only if the `/updates` contract changes (regenerate, never hand-merge).
- **Ask the runner first** for anything else, including a new dependency or any `Cargo.toml` change.

## Do
1. **Check and notify on Windows run in the service, as on Linux:** the same `update::window`, policy and check request, unchanged. Choose the manifest's MSI or portable artifact by how Lodestar was installed (service/MSI vs portable). Say how install kind is detected, and make detection fail safe (an unknown kind notifies, never picks the wrong artifact).
2. **A Windows layout for `trust_file.rs`,** with the same guarantees as the Unix one: refuse reparse points (symlinks, junctions) on every path component Lodestar opens, atomic replace, one writer at a time, owner-only access. The workspace **forbids `unsafe`**: use std's safe Windows APIs (`std::os::windows::fs::OpenOptionsExt`/`MetadataExt` with `FILE_FLAG_OPEN_REPARSE_POINT` and the reparse attribute, `File::lock`, `std::fs::rename`). If owner-only ACLs truly need Win32 calls or a new crate, stop and tell the runner with the options. Record which guarantee rests on the MSI's folder ACLs versus code.
3. **`/updates` on Windows** stops answering `unavailable: "windows_helper"` for check and notify. Install and rollback stay unavailable with a reason (a new or existing reason value, documented) until PK-win-upd-2 or PK-win-portable-upd.
4. Treat every file read from the data folder as untrusted input (Q13's boundary applies even before the helper exists).

## Done when
- Tests on every OS for artifact selection (MSI vs portable vs unknown; missing artifact for this target; wrong channel).
- The Windows layout tested on the Windows CI job: a reparse point refused, atomic replace, a second writer blocked.
- The `/updates` contract test for Windows.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (Windows-only code compiles under `cfg(windows)` on the Windows job; use `cargo check --target x86_64-pc-windows-gnu` locally only if the target is already installed).
- **Size:** ~900 lines (plan). Past ~1,300, tell the runner first.
- **Process:** push once per round; no fixed sleeps (condition waits, deadlines ≥ 10 s); message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green on all platforms.

## These go to the runner
- anything that runs `msiexec`, starts a privileged process or needs a new grant (Q13 is the owner's);
- a manifest format, channel or target-kind change;
- loosening any layout guarantee on Unix.

## Coordination
Running now: CMP-5 (companions), WK-10e (#617), X-pk-manifest-hardening (release.yml, update_manifest.py), X-cloud-infra (infra/). None touch your files. PK-win-upd-2 and PK-win-portable-upd follow you and make one-line edits to `update/windows/mod.rs`; keep its module list easy to extend.
