# X-ci-filter-docs-user (helper, Sonnet; CI)

Decided by: the runner (§2.33 best practice), 2026-11-14Z. Follow-up `ci-filter-docs-runs-rust`.

## Why
`.github/scripts/ci_changes.py` lists `^docs/user/` in `RUST_ALSO`, so any user-guide edit sets `rust=true` and runs fmt, clippy, openapi drift and the whole workspace's tests on Linux and Windows (about 55 min on Windows). #854, a guide wording change, paid this cost. The only Rust code that reads `docs/user/` is the guide-wording assertion at the end of the full-scrape test in `crates/sources-plugins/tests/safety.rs` (it walks every `.md` under `docs/user/`).

## What
1. Remove `^docs/user/` from `RUST_ALSO`. Keep every other entry.
2. Add a `rust_docs` output: true when a changed file is under `docs/user/` and `rust` is false. It's always true on pushes to main, nightly, manual runs and `full-ci`, the same as the others.
3. Add one small job in `.github/workflows/ci.yml`, `rust-docs`, ubuntu only, that runs when `rust_docs` is true: `cargo test -p lodestar-sources-plugins --test safety`. Use the same toolchain, cache and setup steps as the existing test job. Use the same `permissions` as the other jobs: no new permissions, secrets or environments.
4. Add `rust-docs` to the `ci-ok` aggregate, so a skipped job counts as passing and a failed one fails.
5. **Drift guard,** in `.github/scripts/test_ci_changes.py`: a test that greps `crates/`, `bench/` and `fuzz/` (`*.rs`) for the string `docs/user` and fails if any file other than the known reader list references it. The list currently holds `crates/sources-plugins/tests/safety.rs`. A new Rust reader of the guide must then either join the `rust-docs` job or put `docs/user/` back in `RUST_ALSO`.
6. Tests in `test_ci_changes.py`:
   - a docs/user-only change gives `rust=false` and `rust_docs=true`;
   - a docs/user change plus a crate change gives `rust=true`, and `rust_docs` may be false because the full job covers it;
   - `docs/user/03-keep-lodestar-safe.md` still sets `infra=true`;
   - push and `full-ci` set `rust_docs=true`.
7. Update the module docstring's Rust bullet to say the user guide runs only the `rust-docs` job.
8. Add a decisions file: `docs/decisions/2026-11-14-X-ci-filter-docs-user.md`.

## Owns
`.github/scripts/ci_changes.py`, `.github/scripts/test_ci_changes.py`, `.github/workflows/ci.yml` (the new job and its `ci-ok` entry only), the decisions file, and its own `runner/dashboard.yaml` placement line.

## Never
- Change permissions, secrets, environments, triggers or other jobs.
- Weaken or move the safety.rs assertion.
- Use fixed sleeps.

## Checks
Run `python3 .github/scripts/test_ci_changes.py`, or however the existing tests are run; see `ci.yml`. Run `cd tools/status-connector && npm test`. Run the YAML through actionlint if it's present. Push once, then send the runner `ready <PR> <sha>`.
