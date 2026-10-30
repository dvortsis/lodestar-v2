# X-flake-shutdown-percolator: root-cause two timing failures seen on CI
**Model:** Opus (concurrency, graceful shutdown). Runner brief, 2026-10-30.

## Base
Branch `fix/flake-shutdown-percolator` from `origin/main` (rewritten history, 2026-10-30). Never use an older clone. PR to `main` when done. Follow `.claude/skills/steward/SKILL.md`; the runner merges.

## Evidence (CI on `land/L5` head `591c2a3f`, a range that doesn't touch either code path)
1. `test (ubuntu-latest)`, `crates/lodestar/tests/shutdown.rs` `a_stalled_download_cannot_hold_the_shutdown`: `panicked at crates/lodestar/tests/shutdown.rs:272:5: the shutdown took 301.98351ms`. The test expects the graceful shutdown to wait its 10 s bound (`took >= 9 s && < 25 s`) for a client that stopped reading. It finished in 0.3 s instead.
2. `test (windows-latest)`, `crates/lodestar/src/percolator.rs` `percolator::tests::the_task_turns_commits_into_alerts_quickly`: `commit-to-alert at 200 searches: 5.7443864s`, asserting `< 5 s` (B9 measured ≈2.07 s on Linux; `docs/decisions/2026-10-23-B9.md`).

## Do
1. **Shutdown first; it may be a real bug, not a flake.** Find why the server exited in 0.3 s with a stalled download open:
   - Is the test's "stalled" precondition (`queues()` on both ends, 500 ms with no growth) satisfiable before the server is actually blocked in a write, e.g. the whole response already handed to the kernel?
   - Or does the server drop the connection or skip the graceful wait (`crates/lodestar/src/services.rs` `with_graceful_shutdown` and the bound near line 555)?

   Reproduce under load (`cargo test -p lodestar --test shutdown -- --test-threads=1`, many times, with CPU stress in parallel). If the product is wrong, fix the product and keep the test. If the test's precondition is wrong, make it wait on a condition that proves the server is blocked writing. Never weaken the bound's meaning.
2. **Percolator.** Find what dominates the 5.7 s on Windows: the commit interval, timer resolution, the polling loop, or tantivy commit cost on a slow disk.
   - Prefer measuring the store-to-alert latency the test is about, from the commit actually landing, over loosening the bound.
   - If a bound change is the honest fix, base it on a measured Windows distribution and record it in a decisions file.
3. Rules: no fixed sleeps; wait on conditions with deadlines ≥ 10 s. Never skip, ignore or quarantine a test. Run each test at least 30 times locally after the fix.
4. Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar`. Fresh-reviewer self-review (steward §1).

## Own
`crates/lodestar/tests/shutdown.rs`, the test module of `crates/lodestar/src/percolator.rs`, and only if a product bug is proven, the shutdown path in `crates/lodestar/src/services.rs`. Also `docs/decisions/2026-10-30-X-flake-shutdown-percolator.md`.

## Report
Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the root cause for each, the evidence, and the PR link.
