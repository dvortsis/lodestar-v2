# X-sleeps: Replace fixed sleeps in tests with condition waits
**Model:** Sonnet. This is test-only, pattern-following work (CLAUDE.md delegation policy). **Branch:** `slice/X-sleeps`. Owner-approved via #145 (`docs/owner-decisions.md` §2.24; `runner/standards.md` "no fixed sleeps").

**Spec:** `runner/standards.md` §2 (no fixed sleeps), CLAUDE.md "Multi-session work", `docs/decisions/2026-10-23-multi-session.md`.

## Scope
- On main there are about **92 fixed sleeps in 32 Rust test files**: `std::thread::sleep` and `tokio::time::sleep` under `crates/**/tests*` and `**/tests.rs` / `*_tests.rs`. The web e2e specs have **5 `waitForTimeout` calls**.
- Re-count them with `rg` when you start, and list every one in the PR with its decision.
- **Replace** every sleep that assumes work has finished with a poll for the condition: a deadline of **at least 10 s**, polling every 10–50 ms, failing with a message that says what it last saw.
  - Put one small helper per crate's test support where a crate has many, for example `wait_until(deadline, || cond, || describe())`, rather than copying loops.
  - In Playwright, use `expect.poll`, `toBeVisible` and similar auto-waiting assertions instead of `waitForTimeout`.
- **Keep** sleeps that only pace a writer or simulate time passing for the code under test, such as a deliberately slow peer. Add a one-line comment saying why each one stays.

## Own
- **Only the test files you touch,** plus at most one new test-helper module per crate under that crate's existing test support.
- **Do not touch** files owned by slices that are running now. Leave them, list them in the PR, and the runner gives them to their owners:
  - `crates/lodestar/src/logging/**` and the files B-log1b owns: `crates/pipeline/src/**`, `crates/dht/src/**`, `crates/api/src/auth*`, `crates/search/src/rebuild.rs`, `crates/peer-wire/src/fetch.rs`, `crates/lodestar/src/{windows,system}/**`;
  - B-update's `crates/update/**`, `crates/lodestar/src/update/**` and `crates/lodestar/tests/supervisor.rs`;
  - any `web/e2e` spec owned by a running web slice (search, detail, compare, discover, alerts, settings, filters).
- No production code changes. If a test can only wait properly with a production hook, stop and say so.

## Split
- Keep each PR at about 1,500 changed lines or less.
- Split by crate: one PR per crate group, on the same branch in sequence, or as `X-sleeps-<crate>` parts if the runner asks.
- Start with the crates whose tests have flaked: `lodestar` settings and tests, `search`, `store`.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the affected crates' tests pass.
- Each changed test runs 20 times in a row locally under load (`--test-threads` at the default and at 1) without failing.
- For web: `npx playwright test` on the specs you changed, on a private port.
- One push per round of fixes, with main brought in only when needed (multi-session rules).

## Report
Every sleep found, each with its decision (replaced, kept and why, or skipped as owned by another slice), the helpers added, and the 20× run results.
