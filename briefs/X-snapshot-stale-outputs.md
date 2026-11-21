# X-snapshot-stale-outputs (brief, runner 2026-11-21)

Slice entry: runner/slices.yaml `X-snapshot-stale-outputs`. Owns: web/e2e/smoke.test.ts,
web/e2e/smoke.test.ts-snapshots/**, web/playwright.config.ts, plus your decisions file.

## Problem (from #1013)
- The `settings-bp2560-riverstone-dark` baseline lacked the Outputs nav item (OUT-2), yet the
  snapshot test passed. The light 2560 baseline lacks it too.
- So a whole nav row can go missing without any test failing.

## Do
1. Find out why: are the bp2560 smoke snapshots compared in PR runs at all (projects, testMatch,
   e2e.yml matrix), and does the screenshot tolerance (maxDiffPixelRatio / threshold, 0.001 or
   whatever is set) let a missing nav row through at 2560 px? Write down the numbers: pixels in a
   nav row versus the pixels the tolerance allows.
2. Make a missing nav item fail. Prefer a cheap DOM assertion in smoke.test.ts (every Settings nav
   destination is present and visible) over loosening or tightening pixel tolerance globally. If
   the tolerance is the real hole, tighten it only for the screens where it hides a row, with
   numbers.
3. Prove it: remove the Outputs nav item locally (a mutation; revert after) and show the test
   fails at every breakpoint, including 2560.
4. Regenerate the stale baselines the CI way (the repo's snapshot-regeneration commit flow, as
   X-page-frame used), never by hand on another OS. List every PNG that changes in the PR.
5. Not beside another slice that edits web/e2e/smoke*. No fixed sleeps; condition waits with at
   least 10 s deadlines.

## Gates
npm run check, npx vitest run, npm run build, npx size-limit, the smoke specs in playwright
(--trace off), status-connector npm test if you touch runner/*.yaml (only your own entry).
