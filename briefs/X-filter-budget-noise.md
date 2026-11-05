# X-filter-budget-noise: make the filter budget gate robust to shared-runner noise
**Model:** Opus. Fresh Opus review follows. Runner brief, 2026-11-05 (a §2.26 performance-measurement call; the owner's 50 µs number is unchanged).

## Why
`filter budget (§20.8)` failed on #429 (head e2f578e, run 41937126100) in code the PR didn't touch: row "mixed, 5 common words neutralized" read 50.8 µs against 50; its seven passes ranged 36.4–63.9 µs on that runner, the other rows were flat, and the same check passed on two earlier heads. A flaky gate hands failures to unrelated PRs and costs CI re-runs for everyone. It isn't in `ci-ok`, but it is still red on PRs and reviewers must reason about it each time.

## Read first
`crates/filter/tests/props.rs` (`budget`, `spread`, `calibrate`, `scaling`, `REFERENCE`, `ENGINE_SPEED`, the DRIFT/ATTEMPTS logic), `docs/decisions/2026-10-22-filter-budget.md`, `docs/decisions/2026-10-30-X-filter-budget-8573C.md`, `.github/workflows/ci.yml` job `filter budget (§20.8)`, and the job log of run 41937126100 (fetch it with the GitHub MCP tools if you have them; otherwise ask the runner).

## Do
1. **Diagnose first, then fix.** Decide from the log and local runs whether the failure is (a) measurement noise the gate should absorb (which statistic it gates on, why a 36–64 µs spread didn't trigger the DRIFT re-measure or how three attempts all failed), or (b) a real headroom gap for that row on some runner models (the engine near 50 µs). Write the evidence into the decisions file.
2. **If (a):** make the gate decide on a robust statistic across passes/attempts (e.g. the fastest stable pass after calibration, or the median of the attempts that pass the drift check), keeping the existing rule that noise can only make the gate stricter, never hide a regression. Never raise the 50 µs number or exclude the row.
3. **If (b):** say so and propose the smallest engine change that restores headroom for that row, with before/after numbers; build it only if it is local and small, otherwise stop and report to the runner.
4. **Prove it:** run the budget test in release locally at least 20 times (`cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`), report the distribution per row, and show that a deliberate slowdown (e.g. a test-only spin in the mixed-neutralized path that pushes it to ~60 µs) still fails.

## Own
`crates/filter/tests/props.rs` and `docs/decisions/2026-11-05-X-filter-budget-noise.md`; engine files only under step 3 with the runner's go.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. PR titled "X-filter-budget-noise: …" against `main` from `slice/X-filter-budget-noise`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
