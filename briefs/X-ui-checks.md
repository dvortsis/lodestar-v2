# X-ui-checks (brief, runner 2026-11-21)

Slice entry: runner/slices.yaml `X-ui-checks` (read its title and notes). Owns: web/e2e/ui-checks/**,
web/scripts/pseudo-locale.mjs, .github/workflows/e2e.yml, plus your decisions file (add to owns).
Source: docs/review/2026-11-13-ui-ux/ (01–04 and scripts/*.mjs): the UI review's throwaway scripts.

## Goal
Keep the review's checks as real Playwright checks that run in CI, so the regressions it found
can't come back: pseudo-locale (text expansion and untranslated strings), keyboard-only Settings
save, focus return after dialogs and drawers, overlay timings and radius, layout shift, chunk-load
failure, the rebuild and paused states, the phone alert-and-Send flow, and five fixture screenshots.

## Step 0: split before building (required)
This is too big for one PR (the runner splits slices estimated over ~3k changed lines, and PRs should stay
≲1,500 lines). Read the review docs and scripts, then send the runner:
- a list of the checks, each with its source (doc section or script), an estimated size, and whether
  an existing e2e spec already covers it (grep web/e2e first; don't duplicate);
- a split into 2–4 PRs (X-ui-checks-1, -2, …), each standalone, the first one the highest-value
  checks (keyboard Settings save, focus return, chunk-load failure are the likely first);
- what CI wiring each part needs in e2e.yml (prefer adding to an existing project over a new job:
  every new job adds queue time for every PR).
Build only after the runner answers.

## Rules
- Port the intent, not the scripts verbatim: use the repo's fixtures (`go`, `mock`, OpenApiMock),
  e2e/README.md conventions, axe helpers.
- Condition waits with deadlines ≥10 s; no fixed sleeps (`waitForTimeout` is a blocking finding).
- Screenshots only the CI way (marker-file regeneration); list every new baseline.
- PW_CHROMIUM_PATH=/opt/pw-browsers/chromium; never `playwright install`; --trace off.
