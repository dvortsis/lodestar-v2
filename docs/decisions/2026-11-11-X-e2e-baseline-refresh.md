# X-e2e-baseline-refresh decisions

The runner asked for the Insights state baselines (`web/e2e/states.test.ts-snapshots/**`) to be refreshed if they were still stale since #541, plus a `freezeClock` note in `web/e2e/README.md`. None of this changes an owner or security decision.

- **No baseline is stale, so none is regenerated.** The nightly E2E run on `main` (run 43007058662, at `7e4d532`, which includes this branch's base) passed the whole suite, `states.test.ts` among it, and the last PR runs passed it too. No Insights code has changed since WK-17 (#588), which is when these baselines were last updated. The update marker (`web/e2e/.update-snapshots`) was therefore never committed.
- **The README line** says to call `freezeClock(page)` before `go()` in any snapshot spec, and why: relative times come from the clock, so a baseline goes stale as the calendar moves (the 2026-11-06 Insights case).
