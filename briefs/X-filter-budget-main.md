# X-filter-budget-main

Runner brief (2026-11-21). Branch `slice/X-filter-budget-main`; PR title `X-filter-budget-main: bring the filter back under its 50 µs budget`.

## Why
The `filter budget (§20.8)` CI job is red on main. It runs `cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`, and only when a PR touches crates/filter (`needs.changes.outputs.filter`). On #1007's first head (2026-11-21T07:0xZ), eight "engine, packs on, 10,000 rules, mixed …" cases measured 55–73 µs against the 50 µs budget. The X-win-path-ignorables session reproduced it locally on main at e8e16420. `crates/filter` itself hasn't changed since 2026-11-15 (558a9121), so the cause is probably outside it: a dependency it calls (lodestar-metainfo's `format_char`, the tokenizer's crates), a workspace dependency bump, the stable toolchain, or the calibration workload's scaling. Until it's fixed, every PR that touches crates/filter is blocked.

**The 50 µs per torrent at 10,000 rules is the owner's number (§20.8). It doesn't change, and nor does anything that makes the measurement easier to pass.**

## Do
1. **Reproduce and measure** on this container, in release: the budget test's per-case numbers and the calibration factor it applies. Run it at least 5 times, and report the medians and spread.
2. **Find when it went red.** Bisect main from the last commit where the job passed (the last filter-touching PR on or after 2026-11-15; find its run's result through the PR checks, or by running the test at that commit) to today. Use the same machine, release build and calibration for every point. Record each point.
3. **Root-cause the commit or change** that moved the numbers. If it's in crates the filter calls, profile the hot path (`perf` if available, or `cargo bench`-style timing loops in a scratch copy) and name the function.
4. **Fix the cost, not the gate.**
   - The fix goes in whatever code got slower. If that's outside crates/filter, list the files and send them to the runner before editing, so the owns can be checked against running slices.
   - Don't change the budget, the case set, the rule counts, the calibration workload or its scaling. The one exception: if you prove the calibration itself is broken (for example it no longer scales with the machine), stop and send the evidence to the runner. That's a runner and owner question, not a fix you make.
   - If the cause is the stable toolchain (codegen) with no code change, stop and report the evidence. The runner will decide.
5. **Keep every other test green,** including the filter's property tests and golden corpus. Behaviour must not change: same matches, same normalizations.
6. Add `docs/decisions/2026-11-21-X-filter-budget-main.md`: the bisect table, the root cause, before and after numbers (medians of at least 5 runs), and why it holds on CI's runner.

## Owns
- `crates/filter/**`, except that behaviour changes need runner approval first.
- Any other file only after the runner OKs it (step 4).
- The new decisions file, `runner/slices.yaml` (your entry), `runner/dashboard.yaml` (your placement line, only if no rule covers `X-`).

## Not
- No change to `.github/workflows/ci.yml`'s filter-budget job, to the budget test's thresholds or cases, or to the calibration.
- No `unsafe`, no new dependency without the runner's OK, no fixed sleeps.
- Don't fix the U+180B–180F gap in the filter's `invisible` here; it's queued separately (behaviour change).

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-filter`, the budget test in release (5 runs), `cd tools/status-connector && npm test`. The PR touches crates/filter, so the CI budget job runs on it and must pass.

When CI is green on your head, send the runner `ready <pr> <sha>`.
