# X-flake-search-keyboard-2: search table j/k keyboard test flake

**Runner brief, 2026-11-24 (UTC).** Helper (Sonnet-class), test and UI focus fix. Small (target < 200 lines).
- **Branch:** `slice/X-flake-search-keyboard-2` from fresh `origin/main` (or the branch the harness fixes).
- **PR title:** `X-flake-search-keyboard-2: Search table j/k keyboard test no longer flakes`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
`web/e2e/search-table.keyboard.test.ts:8` ("header sort from the keys, then rows with the arrows and j/k") fails at random: after `ArrowDown` focuses `table-row-2`, `j` leaves focus on row 2 and `table-row-3` is never focused (10 s timeout; the locator shows `tabindex="-1"`). Seen on #637 (after #477's fix) and again on #1108 run 45394086956 (GS-5, head 504987ec); a re-run passed both times. The runner said to launch this fix if it repeated. It's a flake in a shared suite, so every web PR pays for it.

## Read first
- The test, its fixtures (`web/e2e/search-results.fixtures.ts`, the `kbd` fixture in `web/e2e/support/`), and `web/e2e/README.md`.
- `web/src/lib/features/search/table/ResultsTable.svelte`, the panel's `active`/`move()` handling in `web/src/lib/features/search/results/ResultsPanel.svelte`, and the shortcut layer that maps `j`/`k` (a key pressed before the listener attaches is buffered; see `search-toolbar.keyboard.test.ts` "keys pressed before the key listener attaches").
- GS-5 (#1108, merged 2026-11-24) changed ResultsTable, ResultsPanel and columns; its decisions file is `docs/decisions/2026-11-24-GS-5.md`.
- `runner/standards.md` (no fixed sleeps; waits are conditions with ≥ 10 s deadlines).

## What to do
1. Find the root cause. Likely suspects, in order: the sort (`sort=-size`) re-renders the rows after focus moved, so `ArrowDown`/`j` act on a stale list or a stale `active` index; `j` goes through the shortcut layer while ArrowDown goes through the row's own handler, and the two disagree on the active row; or `j` arrives before the shortcut listener re-attaches after the URL change.
2. Fix the cause. If it's the app (focus or active-row bookkeeping), fix it there; if it's the test (it doesn't wait for a settled state the app exposes), wait on that condition, never on time. Don't loosen the assertion, skip, retry or quarantine the test.
3. Prove it: run the spec with `--repeat-each=50` (and `--workers` as CI uses) before and after, and say the counts in the PR. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`.
4. Add this slice's `runner/slices.yaml` entry (kind helper, model sonnet, owns below) and its placement rule if none covers it; run `cd tools/status-connector && npm test`.

## Owns
`web/e2e/search-table.keyboard.test.ts`, `web/src/lib/features/search/table/ResultsTable.svelte`, and the focus/active-row lines of `web/src/lib/features/search/results/ResultsPanel.svelte` if the cause is there (ask the runner before touching anything else). A decisions file only if you make a decision.

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, the repeat run above. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green; a fresh Opus review follows.

No AWS or cloud calls, no new accounts, no spending, no force-push.
