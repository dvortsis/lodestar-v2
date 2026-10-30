# X-clippy-msrv: remove the temporary MSRV lint allows
**Model:** Sonnet. The edits are mechanical, from `cargo clippy --fix`; review every hunk. Runner brief, 2026-10-30. **Launch when no large Rust PR is open across the touched crates** (the runner checks this). It touches about 60 files.

## Why
CR-0d-1 (#208) raised `rust-version` to 1.89 and temporarily allowed `collapsible_if`, `manual_is_multiple_of` and `chunks_exact_to_as_chunks` in `[workspace.lints.clippy]` (the workspace `Cargo.toml`). They hit about 125 sites in code CR-0d doesn't own.

## Do
1. Branch `fix/clippy-msrv` from `origin/main`, using a toolchain at least as new as CI's (`rustup update stable`).
2. Remove the three allows and their comment. Run `cargo clippy --workspace --all-targets --fix --allow-dirty -- -D warnings`, then finish the rest by hand.
3. **Behaviour must not change.** Review each let-chain rewrite for evaluation order and side effects; `is_multiple_of(0)` semantics differ from `% 0`, so check for zero divisors; check `as_chunks` remainder handling.
4. `cargo fmt`, workspace clippy `-D warnings` and `cargo test --workspace` pass.

## Own
Every file the fix touches, plus the workspace `Cargo.toml` lint block. No logic edits beyond the lints.

## Done when
Green locally, and the PR description lists the files and confirms no behaviour change. Message the runner. Never merge.
