# X-ci-filter-rust-inputs

Runner brief, 2026-11-13. Sonnet, helper: the CI path filter. A follow-up to #827 (X-ci-filter-fleet, merged 3cf4fd5f).

## Why
`RUST_ALSO` in `.github/scripts/ci_changes.py` lists the non-crate files Rust reads, so editing one of them runs the Rust jobs. Several Rust inputs under `packaging/` are missing from it, so a PR that edits only one of them skips the Rust tests. rv-827 confirmed each on main:
- `packaging/windows/portable/Start Lodestar.cmd`: `include_str!` in `crates/lodestar/src/update/windows/portable.rs`.
- `packaging/windows/main.wxs`: read in `crates/lodestar/tests/legal_facts.rs`.
- `packaging/**/*.ps1`: walked in `crates/lodestar/tests/legal_facts.rs`. Today that's the ten scripts in `packaging/windows/`.
- `packaging/keys/*.pub`: read in `crates/update/src/keys.rs`.

## Must have
1. **Re-run the grep yourself on fresh main** before you change anything: every `include_str!`, `include_bytes!`, `fs::read*` and walk over `packaging/` in `crates/**`, `bench/**`, `fuzz/**` and the `build.rs` files. Add exactly what Rust reads. A starting point: `^packaging/windows/(main\.wxs$|portable/Start Lodestar\.cmd$)|^packaging/.*\.ps1$|^packaging/keys/`. Adjust it to what the grep finds, and list each match in the PR body with its file:line.
2. **Update the summary comment** at the top of ci_changes.py.
3. **Tests in `.github/scripts/test_ci_changes.py`:** one per new path, plus near misses. Cover `main.wxs.bak`, `join.wxs` if Rust doesn't read it, a `.ps1.txt`, and `packaging/keysx/`. Also add rv-827's two optional near misses: `packaging/companions/compose-old/a.yml`, and `packaging/companions/unraid` without the slash.
4. **The test in `crates/companions/tests/templates.rs` that checks for leftover files** walks only `compose/` and `unraid/`, so a stray file at the top of `packaging/companions/` goes unchecked. Note it in the PR body as a possible follow-up. Don't change it: that file isn't in your owns.

## Owns
`.github/scripts/ci_changes.py`, `.github/scripts/test_ci_changes.py`.

## Process
Branch `slice/X-ci-filter-rust-inputs`; PR title `X-ci-filter-rust-inputs: CI filter: run the Rust jobs when packaging files Rust reads change`. Follow `.claude/skills/steward/SKILL.md`. Run `python3 -I .github/scripts/test_ci_changes.py` before pushing. When CI is green, message the runner `ready <PR> <full sha>`. No cloud or AWS calls.
