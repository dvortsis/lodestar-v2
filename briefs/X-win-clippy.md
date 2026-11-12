# X-win-clippy (Sonnet): lint Windows-only code in CI, and fix what it finds

Runner brief, 2026-11-12 (UTC). Linux CI never lints `cfg(windows)` code, so Windows-only warnings reach `main`. Branch `slice/X-win-clippy` from current `main`. PR title `X-win-clippy: <title>`.

## Do
1. **Fix the Windows-only clippy warnings on `main`.** Known so far, from `cargo clippy --target x86_64-pc-windows-gnu`:
   - `crates/companions/src/config_files.rs:26`: unused import `File`.
   - Needless `mut`: `crates/companions/src/extract.rs:780`, `fetch.rs:584`, `layout.rs:693`, `manager.rs:530`.
   - `crates/lodestar/src/main.rs:301`: collapsible `if`.
   - Any others the Windows lint step shows. Pure lint fixes: no behaviour change. If a fix would change behaviour, stop and ask the runner.
2. **CI:** in `.github/workflows/ci.yml`'s `test` job, add a step `if: runner.os == 'Windows'` that runs `cargo clippy --workspace --all-targets -- -D warnings`. Put it before `cargo test --workspace` so it reuses the same build cache. The `test` job uses `dtolnay/rust-toolchain@stable`; add `components: clippy` to that step if the Windows runner needs it.
   - Approved by the runner (§2.33): no change to `permissions:`, secrets, triggers, `needs`, or the required checks' names. If any seems needed, stop and ask.
3. The container can't run Windows. Check locally with `rustup target add x86_64-pc-windows-gnu` and `cargo +1.99.0 clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`. That can fail to link or build some crates without a mingw toolchain; if it does, use `cargo check`-only lints (`clippy` doesn't link) and let CI's Windows job be the final word.
4. Decisions: `docs/decisions/2026-11-12-X-win-clippy.md` (short).

## Shared edits approved
The lint-only lines above in `crates/companions/src/*` and `crates/lodestar/src/main.rs`, and `.github/workflows/ci.yml`. Other open PRs touch companions and Windows code (X-svc-eventcreate-bounded #728). If you conflict after one merges, bring `main` in; never hand-resolve someone else's logic.

## Checks
`cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings` (Linux), the Windows-target clippy above, `cargo test --workspace`. CI's `test (windows-latest)` must be green with the new step. Steward skill; message the runner "ready <PR> <sha>" when ci-ok, web-ok and e2e-ok are green.
