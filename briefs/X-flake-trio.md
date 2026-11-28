# X-flake-trio: fix three flaky Rust tests once each (runner brief, 2026-11-28)

Runner decision. Each of these failed a CI run on code the failing PR didn't touch; no fix PR is open for any of them. Fix each once, at its root cause, in one PR with one commit per test.

## The three
1. **`crates/dht/src/node/proof/tests.rs` ~line 1207, assertion "others are free".** Failed on `test (ubuntu-latest)` for #1181 (re-run passed). Runner's hypothesis, unverified: a false positive from the capacity-1 spill filter (a probabilistic filter sized for one entry reports "maybe present" for an unrelated key). Prove or refute it: find the seed or input that fails, reproduce it deterministically, then fix the test's expectation or the setup so it is exact (e.g. size the filter so a false positive is impossible for the test's keys, or assert on the exact set rather than "free"). Don't weaken what the test proves about the production code; if production code is wrong, say so and stop for a ruling.
2. **`crates/log/src/url.rs` ~line 4422, `url::tests::scrub_text_is_linear`.** Flaked on `test (macos-latest)` on main, and takes over 60 s on Windows every run (it's the longest test in the `log` lib, which bounds the sharded CI). It's a timing test of linear behaviour. Make it robust and fast: e.g. compare operation counts or the ratio of two sizes within one run with a generous bound, use input sizes that take well under a second, and never an absolute wall-clock limit tuned on one machine. It must still fail on a quadratic scrubber: show that by a local mutation (make the scrubber quadratic in a scratch copy) and report the result.
3. **`crates/pipeline/tests/core_rejudge.rs` ~line 985, `a_range_that_never_reads_doesnt_hold_up_the_next`.** Flaked on `test (macos-latest)` on main. Find the race (likely a fixed-time window or an ordering assumption), and replace it with a condition wait with a deadline of at least 10 s.

## Rules
- No fixed sleeps; condition waits with deadlines >= 10 s (CLAUDE.md). No skipped or ignored tests. No `#[cfg]` to dodge a platform.
- For each: name the root cause in the PR body and the decisions file, and show the reproduction (seed, loop count, or stress) before and after: run the test in a loop (at least 200 iterations, or under `stress`/CPU hogs) and report failures before vs after.
- Owns: the three test files above, plus test-only helpers in the same crates' `#[cfg(test)]` modules. Production code: none without a runner ruling.
- Decisions file: `docs/decisions/2026-11-28-X-flake-trio.md`.
- PR title: `X-flake-trio: <title>`. Follow `.claude/skills/steward/SKILL.md`. Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (or the touched crates if disk is tight), `cargo test` for the three crates.
- Send the runner a step 0 (the root cause found for each, and the fix planned) before building, then `ready <PR> <sha>` when green.
