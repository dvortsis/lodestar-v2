# S-search-recover-ui: "Removing X gives N" and the index-state banners
**Model:** Sonnet: UI per §10.4, binding fields B-search-recovery (#266) added. Runner brief, 2026-10-31. No `/security-review`; the server already applies visibility to every count.

## Why
#266 added `recovery` on `/search` (per-clause counts for an empty result) and `GET /search/state` (index size, `cold_start`, `growing`, growth per hour, rebuild progress, unavailable fields). The Search screen doesn't use them yet: zero-result recovery only offers remove-a-filter with no counts, and the "still growing" banner uses `/stats` plus a local threshold (`ResultsPanel.svelte`'s `COLD_START_BELOW`).

## Read first
- **`docs/decisions/2026-10-31-B-search-recovery.md`: "Per-clause counts", "Index state" and "UI follow-up" are the spec.**
- `docs/architecture.md` §10.4, the States table (No results, Index rebuilding, Cold start; around line 455) and line ~1311 (the full-page `searching_crawling` state); §10.13 (announcements); §10.1 (budgets).
- `docs/decisions/2026-10-31-S-search-6.md` and `-S-search-2a.md` (the existing banner and empty state).
- `web/src/lib/features/search/{results,query}/**`, `clauses.ts`, and the shapes in `web/src/lib/api/schema.d.ts` (`SearchRecovery`, `SearchIndexState`).
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md`, `docs/metrics.md` (formatting and "≈").

## Do
1. **No results:** for each `recovery.clauses[]` with `total > 0`, show "Removing `{clause}` gives {total}" (`formatCount`, "≈" when `total_approx`). One click searches `without` (the reduced AST from the server; never re-parse). Entries without `total` show nothing; `skipped > 0` shows nothing extra. Keep "Clear all filters". Keyboard reachable, announced once through the existing polite region.
2. **Banners from `/search/state`:**
   - `cold_start` gives the full-page "searching/crawling" state; `growing` gives the "The index is still growing — N torrents, +X/h" banner (`growth_per_hour` null means omit the rate, never show 0).
   - `rebuild` gives the "Index rebuilding" banner with progress (done/total), and lists `unavailable` fields when present.
   - Remove `COLD_START_BELOW` and the `/stats` dependency for these.
   - Fetch once per Search visit plus on the existing `event: rebuild` SSE, with no polling loop.
3. **Sizes:** /search first load must not grow beyond ~150 B; load recovery rendering and banners with the results chunk or on demand. Never raise a limit. CI's Lighthouse `total-byte-weight` on /search must stay under 102,400 B. If you can't fit, stop and report.
4. **i18n:** `search_recover_*` and `search_state_*` keys (en and zh).
5. **Tests:**
   - vitest: mapping (only `total > 0`; "≈"; absent total hidden), applying `without`, banner choice from `cold_start`, `growing` and `rebuild`, and a null rate omitted;
   - Playwright: zero-result recovery applies a suggestion (keyboard), the growing banner, and the rebuilding banner, all against the mock (add fixtures), with axe;
   - condition waits only, no fixed sleeps;
   - **no existing baseline changes expected.** If screenshots change, stop and tell the runner.
6. **Decisions file:** `docs/decisions/<date>-S-search-recover-ui.md`.

## Own
`web/src/lib/features/search/results/**` (the empty state and banners), `web/src/lib/features/search/live/**` only if the banner lives there, the message keys above, the search mock fixtures, `web/e2e/search-recover-ui.*`, and the decisions file. **Don't edit** `lib/state/**`, `lib/shell/**`, `lib/ui/**`, `lib/api/**` (regenerated types are already on main), `search/table/**` or `search/bulk/**`.

## Size
≲ 700 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the new spec 3× and the existing search specs. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked local config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally. Push once per round of fixes.
