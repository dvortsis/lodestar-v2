# X-filter-budget-drift brief (runner, 2026-11-28Z)

**Slice:** X-filter-budget-drift. The §20.8 filter budget gate holds when the runner's speed drifts within one run.
**Model:** opus. **Branch:** `slice/X-filter-budget-drift`. **PR title:** `X-filter-budget-drift: The §20.8 filter budget gate holds when the runner's speed drifts within one run`.

## Why
- The `filter budget (§20.8)` job is inside the required `ci-ok`, so a flake there blocks every merge.
- On #1167's head d63a7d3a it failed with four "engine, 10,000 rules" cases at 54–71 µs against the 50 µs budget. #1167 touches nothing in `crates/filter`.
- The job's calibration drifted from 7.07 µs to 10.6–12.5 µs within the run, during the re-measure.
- The same job is green on `main`'s recent runs.
- The earlier fix X-filter-budget-remeasure (`docs/decisions/2026-11-21-X-filter-budget-remeasure.md`, commit a12a8e57) re-measures a held case once. It scales by the *faster* of two calibrations, so a host that stays slow through the re-measure still fails.
- See also X-filter-budget-noise and its records.

## Fixed (owner numbers, never change them)
- The 50 µs budget, the cases, the corpus and the rule counts stay as they are. Changing an owner-set number needs the owner.
- The gate must never read more leniently than the code's real speed on a quiet host. A real regression of 10% or more must still fail.

## Do
1. **Step 0** (message the runner before writing code):
   - Read `crates/filter/tests/props.rs` and both decisions files.
   - Explain why the a12a8e57 design still fails on in-run drift.
   - Propose a design. Candidates:
     - interleave calibration with each case's samples and scale each case by its own neighbouring calibration;
     - or, when the calibration itself drifted past a set bound during an attempt, call the attempt void and run it again (at most once, with a `::warning::`) instead of failing.
   - Show it with numbers. If you can, replay the failing run's figures from #1167's CI log: the job log for the "filter budget (§20.8)" check on commit d63a7d3a08c4957d4ebe98336cc29fa5208b386a. Don't put CI ids in the repo.
2. Build the design, with property tests showing two things:
   - the gate never passes a case whose calibrated time is over budget on a steady host;
   - a uniformly slowed host (factor 1.0–2.0, drifting within the run) gives the same verdict as a quiet one.
3. Record it in `docs/decisions/*-X-filter-budget-drift.md` as a runner refinement under §2.26. The budget itself is unchanged.

## Owns
- `crates/filter/tests/props.rs`
- `docs/decisions/*-X-filter-budget-drift.md`, with the heading `# X-filter-budget-drift decisions (#PR)`
- `.github/workflows/ci.yml` **only if** the job's invocation must change. Ask first.

## Rules
- Follow `.claude/skills/steward/SKILL.md`. Before pushing, run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar-filter`.
- No fixed sleeps. Never skip or loosen a test.
- No model identifiers in commits, the PR or code. Times in UTC with Z.
- The runner merges; you never do. When CI is green, send `ready <PR> <full sha>` to session_01EYhgh4JX3AmHzKXPjYCoUg with `send_message`.
