# X-search-retry-flush (runner brief, 2026-11-22Z)

Kind: core (Search route, focus and loading). Dep X-search-chunk-retry (#1057) is merged. Read its decisions file (`docs/decisions/2026-11-22-X-search-chunk-retry.md`) and r11's correction of it (`docs/decisions/2026-11-22-runner-records-r11.md`) first.

## What to fix
1. **Retry sends the query just typed.** The bar writes the URL after a 200 ms debounce and parse. A Retry pressed inside that window reloads at the previous query, losing what was typed.
   - Flush the bar's pending text into the URL before `location.reload()`, or have Retry read it.
   - Name the bar's file and the mechanism at step 0. Its owner slices are merged; if a file outside owns must change, list it and wait for my answer.
   - The query text is written exactly as the bar would write it: same parse, same lower-casing rules.
2. **A failed detail drawer says so.** `+page.svelte`'s drawer `import()` (around line 132) has no rejection handler. A failed DetailHost chunk is an uncaught rejection, and the drawer shows nothing.
   - Show a failure in the drawer's place the way the results do: `role="alert"`, one short line plus Retry, tokens only.
   - Retry may reload (the same Chromium constraint as #1057), keeping the drawer's `page.state` if it can. Otherwise say what's lost.
   - No uncaught rejection.
3. **Honour the Retry marker only for a load that starts soon after the reload.** On bare `/search`, a load that the 30 s fallback timer or a browse-mode keydown starts can still use the marker and move focus to the empty results while the body has focus. Pick the window at step 0, with a reason.
4. **The stale comment** at `+page.svelte:6` ("mounted from a lazy glob below").

## Rules
- **Size:**
  - Search route JS has 153 B of headroom (44,847 of 45,000 B). The cap stays (§10.1, and the owner's smallest-UI priority).
  - Lighthouse `total-byte-weight` on bare `/search` is 101,325 of 102,400 B.
  - Measure with `npm run size:ci` before and after, and send the numbers at step 0 and in ready. No limit raises.
  - `web/.size-limit.mjs` is not in owns.
- **Messages:** new strings go in the Search messages (en and zh). A route-only key, never a shared one that would pull a message chunk onto the first load (#1057's review).
- Never beside X-alert-noun (it owns the Search messages) or a slice that owns `web/src/lib/features/search/query/**`.
- Focus never leaves the bar while someone is typing (QueryBar's rule).
- **Tests (e2e in `chunk-load.routes.test.ts`):**
  - Retry within 100 ms of typing reloads at the typed query;
  - a blocked drawer chunk shows the alert with no page error, and Retry recovers;
  - a load started by the 30 s timer after Retry doesn't move focus.
  - No fixed sleeps; use the clock or condition waits with deadlines of at least 10 s.

## Owns
- `web/src/routes/search/+page.svelte`
- `web/src/lib/features/search/messages/en.json` and `zh.json`
- `web/e2e/ui-checks/chunk-load.routes.test.ts`
- `docs/decisions/2026-11-22-X-search-retry-flush.md`
- this slice's `runner/slices.yaml` entry
- Refine at step 0: the bar's file, if it must change.

## Step 0 (before building)
1. The bar's flush mechanism and its file.
2. The drawer failure's design and what a reload keeps.
3. The marker window.
4. Sizes now.
5. Files and an estimate.

## Branch and PR
Branch `slice/X-search-retry-flush`, PR title "X-search-retry-flush: Search: Retry sends the query just typed, and a failed detail drawer says so". Run the CLAUDE.md web gates, plus `npm run size:ci` and the chunk-load spec. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
