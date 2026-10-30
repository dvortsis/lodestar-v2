# X-filter-budget-8573C: the filter budget gate fails on 8573C runners for code it doesn't test
**Model:** Opus (the content-filter performance gate). Runner brief, 2026-10-30.

## Base
Branch `fix/filter-budget-8573C` from `origin/main`. Open a PR to `main` and follow `.claude/skills/steward/SKILL.md`. The runner merges.

## The failure
On #210 (IMP-1, a store-only PR), the `filter budget (§20.8)` job (run 41065412960, job 124095631213) ran on an INTEL XEON PLATINUM 8573C:
- the 8573C's `ENGINE_SPEED` factor is 0.900, provisional, from one job (41015045891);
- the calibration said 0.92×;
- raw mixed was 43.4 µs, which by itself implies ≈ 43.4 ÷ 45.3 = 0.958;
- raw "mixed, 5 common words neutralized" was 48.0 µs, giving 48.0 ÷ 0.900 = **53.3 µs**, over the held 50 µs budget.

On the EPYC 7763 reference the neutralized case is ≈ 43.6 µs, faster than mixed. On the 8573C it's ≈ 1.1× mixed. **One per-model factor taken from the mixed set doesn't carry over to the neutralized case on this model.**

## Constraints (do not cross)
- The budget is **50 µs on the reference runner**, an owner number (§1.6.6, `docs/decisions/2026-10-22-filter-budget.md`). Never change it.
- "The table never moves to make a run pass" (props.rs). Every `ENGINE_SPEED` change needs a decisions entry with the runs and the arithmetic.
- No case gets dropped from "held" to make CI pass. If a held case can't be judged fairly on a model, that's a method decision for the runner: message it with the evidence before you change what's held.

## Do
1. **Gather the data.** List every `filter budget` job on the 8573C and its raw µs per case. Use the job logs on main and recent PRs (the GitHub API job list, filtered by the CPU line), plus the 7763 medians at the same code.
2. **Choose a fair method, with the evidence.** For example:
   - (a) re-derive the 8573C factor from ≥ 3 jobs;
   - (b) per-case factors per model, at least for the neutralized case, from the same jobs;
   - (c) when a listed model's per-case scaling is uncertain (provisional, or drifting more than MODEL_DRIFT), fall back to the calibration or report-only for that model and case, while the reference runner still holds every case.
3. Message the runner with the numbers and the proposal **before** changing what's held. Then implement it, with a decisions file `docs/decisions/<date>-X-filter-budget-8573C.md`.
4. **Engine regression check.** Also measure whether the neutralized case is genuinely slow on the 8573C (an engine cost, e.g. cache effects of the neutralize index), as opposed to scaling noise. If it's real engine cost, say so; an engine optimization would be a separate slice.

## Own
`crates/filter/tests/props.rs` and the decisions file. Name anything else.

## Done when
The gate passes on the 7763, the 9V45 and the 8573C at main's code. Show logs: dispatch `ci.yml` on your branch, re-running until each model has appeared, with the runs listed. fmt, clippy and the filter tests pass. Message the runner. Never merge.
