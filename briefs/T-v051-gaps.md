# T-v051-gaps: targeted tests for untested release-delta changes
**Model:** Opus (store on-disk logic and auth). Runner brief, 2026-10-30. Tests only, no behaviour change.

## Base
Branch `test/v051-gaps` from `origin/main`. Open a PR to `main` when done. Follow `.claude/skills/steward/SKILL.md`; the runner merges.

## Why
The v0.5.1 delta mapping (`git show origin/runner-state:notes/v051-delta-draft.md`) found changes since the soaked build `744b84d6` with no targeted test. Two of them matter.

## Do
1. **B2, `crates/store/src/snapshots.rs` (d4a7aac4, about line 515).** This is the branch where `std::fs::metadata` on a snapshot fails with an error other than NotFound. The code warns "reading the snapshot … failed", sets `blocked = true` and skips the snapshot, which must block acknowledgement of journal rows.
   - Write a test that triggers it deterministically on Linux and macOS CI, which run as root, so permission tricks don't work. Use, for example, a symlink loop (`a -> b`, `b -> a`) named like a snapshot, so `metadata()` returns ELOOP.
   - Assert:
     - the warning;
     - the snapshot is skipped, not deleted;
     - deleted-hash journal rows aren't acknowledged on that pass;
     - a later pass with the file fixed acknowledges them.
   - Gate it with `#[cfg(unix)]` if needed, and say so.
2. **E1, `crates/api/src/auth.rs` (a2c15c9e).** `setup` returns `AuthError::SetupDone` when `state.password.is_some()`; the commit calls this defensive and unreachable today.
   - If a cfg(test) seam already exists in `src/auth/` (see `lock_tests.rs` and `hooks.rs`) that can reach it, add a unit test.
   - If reaching it needs a new seam, don't add one. Record in the PR description why it stays untested.
3. No fixed sleeps. Deadlines of 10 s or more if waiting.

## Own
Tests in `crates/store` (snapshots tests) and `crates/api/src/auth/` test files only.

## Done when
fmt, clippy `-D warnings`, and `cargo test -p lodestar-store -p lodestar-api` pass. The fresh-reviewer self-review is done. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number.
