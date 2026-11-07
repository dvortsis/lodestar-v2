# X-gs-gate-polish: size-gate follow-ups from the GS-gate review

Runner brief, 2026-11-07T14:27Z. Model: Sonnet (CI and docs polish). A fresh Opus reviewer checks it before merge.

## What
The non-blocking notes from the #546 (GS-gate) review, in `runner/slices.yaml` (id X-gs-gate-polish):
1. Add `timeout-minutes` to the `size-budget` job in `.github/workflows/ci.yml` (about 40; look at recent size-budget run times and leave a comfortable margin).
2. The `--summary` doc comment in `bench/src/bin/size-report.rs` names `$RUNNER_TEMP/size-report.md` (the path CI actually writes).
3. `docs/decisions/2026-11-06-GS-gate.md`: say the measured sizes agree across runners "within about 0.2 B", not "hardware-independent" exactly.
4. Note in the same decisions file (and a short doc comment on `dir_size` in size-report.rs) that `dir_size` does not recurse, so a new subdirectory under a measured folder isn't counted. Reviewers of GS-2 and RX-1 must watch for new subdirectories. If making it recurse is a small change with a test, do that instead and say so; budgets stay unchanged.

## Owns
`.github/workflows/ci.yml` (the size-budget job only), `bench/src/bin/size-report.rs`, `docs/decisions/2026-11-06-GS-gate.md`.
Budgets (`STORE_BUDGET`, `BLOB_BUDGET`, `INDEX_BUDGET`, `FILE_INDEX_BUDGET`) are the owner's §3 numbers: don't change them.

## Rules
- `cargo fmt --check`, `cargo clippy -p lodestar-bench --all-targets -- -D warnings`, and `cargo run --release -p lodestar-bench --bin size-report` (or the job's exact command) locally if disk allows (`df -h` first; ≥ 15 GB free).
- Branch `slice/X-gs-gate-polish` from fresh origin/main. PR title "X-gs-gate-polish: Size-gate follow-ups from the GS-gate review". Small: under 150 lines.
- Use the steward skill. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green, including size-budget.
