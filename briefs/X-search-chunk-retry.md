# X-search-chunk-retry (runner brief, 2026-11-22Z)

Kind: core, model opus. Deps X-ui-checks-1 (#1036) and X-ui-checks-4 (#1048) are merged. From #1036: when the Search results chunk fails to load, `/search?q=` shows only the query bar, with no message, and the page throws an uncaught rejection.

## The bug
`loadResults()` in `web/src/routes/search/+page.svelte` (main 1da2253a, about lines 63-76) caches its `Promise.all` of six imports in `loadingResults ??=` and has no `.catch`. A failed chunk leaves `Results`, `Facets` and `Toolbar` undefined forever, and later calls reuse the rejected promise.

## Do
1. **A failure state where the results would be:** a `role="alert"` message plus a Retry button, as the other lazy screens do. Copy the pattern the Insights stage page uses (`insights-load-error` in `web/e2e/ui-checks/chunk-load.routes.test.ts`); find its component and reuse it or its markup and tokens. Don't invent a new style.
   - Wording, in en and zh: plain and short, saying what failed and what Retry does (e.g. "Search results couldn't load. Retry"). Check `docs/ui-glossary.md` for the right nouns.
   - The query bar stays usable. Typing a new query while in the failure state retries the load as well.
2. **Retry really reloads:**
   - Clear the cached promise on failure, so a retry calls `import()` again.
   - Check in Chromium that a dynamic import which failed once can succeed on retry. If the browser caches the failure for the same URL, use the same workaround the Insights page uses, and say which it is.
   - No unhandled rejection in any path.
3. **Focus and announcements:** the alert is announced once. After Retry succeeds, focus goes to the results region; it must not be lost to `body`. Keyboard-only users can reach Retry.
4. **Tests:**
   - Flip #1036's expected-gap case in `chunk-load.routes.test.ts` to assert the alert, then Retry bringing the results back.
   - Drop the page-error expectation, and assert that no page error happens.
   - Add a case: a new query typed in the failure state also recovers.
   - Condition waits only, with deadlines of at least 10 s.
5. **Size:** Search's first view is capped at 45 kB (§10.1), and the owner's priority is the smallest UI. Measure with `npm run size:ci` before and after, and send the numbers before raising any limit. Keep the failure markup in the route only if it is a few hundred bytes; otherwise discuss at step 0.

## Owns
- `web/src/routes/search/+page.svelte`
- `web/src/lib/features/search/results/**` (only if needed; say at step 0)
- the search messages en and zh files that hold the new strings (name them at step 0; never beside X-alert-noun-text or RX-2 if either is running)
- `web/e2e/ui-checks/chunk-load.routes.test.ts`
- `web/.size-limit.mjs` (the Search route and results entries only, measured first)
- `docs/decisions/*-X-search-chunk-retry.md`
- this slice's `runner/slices.yaml` entry

## Step 0 (before building)
1. The component or pattern you will reuse.
2. The message keys and their files.
3. How retry re-imports in Chromium.
4. Current sizes from `npm run size:ci`.
5. Files and an estimate.

## Branch and PR
Branch `slice/X-search-chunk-retry`, PR title "X-search-chunk-retry: <title>". Run the CLAUDE.md web gates plus the chunk-load spec. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
