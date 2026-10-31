# S-search-6: Search — the live banner, zero-result recovery, cards density
**Model:** Sonnet: UI per §10.4, following the S-search-1..4 pattern. Runner brief, 2026-10-31. M5 screen backlog: "Search completion", part 3 of 3. Its dependencies have merged: S-search-3 and B4. No `/security-review`: this renders server values as text.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md` and `docs/owner-decisions.md`.
- `docs/architecture.md` §10.4, the "States" table (around line 455):
  - **No results:** zero-result recovery from per-clause counts ("Removing `trackers:>=3` gives 1,240"), plus "Clear all filters".
  - **Live:** a "12 new results · Show" banner; the list never reorders under the user; a throttled polite `aria-live` announcement.
  - **Cold start / index rebuilding:** banners, if the server exposes the state.
  - Cards density.
- Also: §10.13 (announcements and throttling), §10.1 (budgets), §10.15.
- The API: whatever main exposes for live updates (the SSE `/events` hub and the percolator, B9) and for per-clause counts (B4 / `/search` or `/parse`). Take shapes from `web/src/lib/api/schema.d.ts`. **If an endpoint you need doesn't exist, stop and tell the runner;** don't invent one.
- Existing code: `web/src/lib/features/search/{results,query,toolbar}/**` (query/Live.svelte now hosts the assist panel from S-search-4), `search/README.md`, and the S-search-1..4 decisions files.

## Do
1. **The live banner** (`features/search/live/**`): when new matching results arrive for the current query, show "N new results · Show".
   - Clicking Show prepends them; nothing reorders until the user acts.
   - The announcement is throttled, through the existing polite region.
   - The banner pauses while the user is scrolled deep or has a selection (S-search-5 is building selection in parallel; coordinate only through the results store's public surface).
2. **Zero-result recovery:** for an empty result, show per-clause "Removing X gives N" suggestions from the server's counts, each one click to apply through the AST, plus "Clear all filters".
3. **Cards density** (`features/search/cards/**`): the Cards view/density option in the existing toolbar density control, with the same row data and actions.
4. **States:** wire the "Index rebuilding" and "Cold start" banners if the server exposes them; otherwise record them as deferred, naming the needed endpoint.
5. **Sizes:** /search first load must not grow. Load the banner logic, recovery and cards on demand. Add size-limit entries at measured + 10%, and never raise a limit.
6. **i18n:** `search_live_*`, `search_recover_*` and `search_cards_*` keys.
7. **Tests:**
   - vitest: the banner's throttling, no reorder until Show, recovery suggestions applying through the AST, and the cards rendering;
   - Playwright: live banner keyboard and AT behaviour, zero-result recovery, and cards at each breakpoint, with axe;
   - condition waits only, no fixed sleeps;
   - **no baseline changes expected.** If screenshots change, stop and tell the runner.
8. **Decisions file:** `docs/decisions/<date>-S-search-6.md`.

## Own
`web/src/lib/features/search/live/**`, `web/src/lib/features/search/cards/**`, minimal mount edits in `search/results/**` and `search/toolbar/**` (justify each in the PR), the message keys above, `web/e2e/search-live.*`, `web/e2e/search-recover.*` and `web/e2e/search-cards.*`, the size-limit entries, and the decisions file.

**Don't edit:** `lib/state/**`, `lib/keys/**`, `lib/shell/**`, `lib/ui/**`, `lib/api/**`, `search/table/**`, `search/bulk/**` (S-search-5's), and `search/assist/**`.

## Size
≲ 1,500 changed lines. If it grows past that, split it:
- **6a:** the live banner and recovery;
- **6b:** cards.

## Done when
These pass in `web/`:
- `npm run check`
- **`npm run e2e:typecheck`**
- `npx vitest run`
- `npm run build`
- `npx size-limit`
- `npx playwright test` for the new and existing search specs, 3× for anything new (Chromium is at /opt/pw-browsers; never run `playwright install`)

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
