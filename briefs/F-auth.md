# F-auth: credentials and setup survive a failed auth.json save (A2-1, release blocker)
**Model:** Opus. **Security-sensitive:** run `/security-review` before the PR.

## Base and PR timing (read first)
- Branch `slice/F-auth` from `origin/next` (the rewritten history that replaces `main`; owner-decisions §2.25 on `origin/claude/busy-allen-aptli2`). Never base on, merge or pull the old `origin/main`: its history is being replaced, and a PR head containing the old root `15620683` or old tip `2d5e761d` will be refused.
- Until the owner force-pushes `main` (the runner tells you), GitHub cannot open a PR between the two histories. Build, test, self-review and push your branch, then message the runner "ready, waiting for the swap". Open the PR to `main` only when the runner says the swap is done.
- The review report is on another branch: read it with `git show origin/fix/dht-cr0:<path>`.

**Spec:** `docs/code-review/codex-review-v0.5-area-2-results.md` finding 1 (on fix/dht-cr0), architecture §12 and the M2 auth decisions, `runner/standards.md`.

## Problem
`crates/api/src/auth.rs` (~line 355, setup ~296–298) mutates the shared auth state (password hash, session key) before `self.save(&state)?` succeeds. On a save error the old password and sessions already stop working, the new password works until restart, and setup leaves `needs_setup()==false` with the token still pending; a restart then reverts.

## Do
1. Build the replacement state, persist it, and only then publish it under the write lock — for change_password, setup, and every other auth mutation that saves (audit them all; `set_proxy` and `rotate_sessions` already restore on failure, make them use the same pattern).
2. Setup consumes the token only after the save succeeds.
3. Tests (no fixed sleeps): the two reproductions in the report (a directory planted at `auth.json.tmp`), plus restart-after-failure for each mutation, plus the success paths unchanged. Add a property or table test over all mutating methods asserting "error ⇒ observable state unchanged before and after restart".

## Own
`crates/api/src/auth.rs`, `crates/api/src/auth/**`, `crates/api/tests/auth*.rs`, `docs/decisions/<date>-F-auth.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass; `/security-review` clean; fresh-reviewer self-review done.
