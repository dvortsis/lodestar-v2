# S-search-5: Search — table mode, column chooser, selection, bulk bar
**Model:** Sonnet: UI per §10.4, following the S-search-1/2/3/4 pattern. Runner brief, 2026-10-31. M5 screen backlog: "Search completion", part 2 of 3. Its dependency, S-search-3, has merged. No `/security-review`: this renders server values as text and calls existing endpoints.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md` and `docs/owner-decisions.md`.
- `docs/architecture.md` §10.4: "Table mode", "Selection and bulk actions", "Results toolbar" (select mode) and the result-row facts (around lines 435–455).
- Also: §10.1 (budgets), §10.2 (control heights; 32 px dense toolbars), §10.13 (announcements; the single-key rule for `x`), §10.15 (narrow widths: horizontal scroll contained in the table) and §10.16 (keys).
- §7 sorts, from `schema.d.ts` / `crates/api/src/search.rs`. Send-to-client and bulk send are the existing client API (architecture around line 2567: `Partial`, duplicates). Compare takes 2–5 hashes (the S-compare route). Export uses whatever endpoint already exists on main (`/export`, B10b). If one isn't there, leave Export out and record it as deferred.
- Existing code: `web/src/lib/features/search/{results,toolbar}/**`, `search/README.md`, and the S-search-1..4 decisions files. **Don't fork the row's data path:** reuse the results store.

## Do
1. **Table mode** (`features/search/table/**`): a real `<table>`.
   - A sticky header; sortable columns that drive the existing URL sort (no client-side resort of a paged list); numbers right-aligned with explicit units.
   - Horizontal scroll contained in the table at narrow widths.
   - The same infinite-scroll and "Load more" fallback as the list.
   - Toggled from the toolbar's density/mode control, with the mode persisted in the URL through the existing state. If `lib/state` has no field for it, stop and tell the runner.
2. **Column chooser:** show/hide and order, persisted per browser in `localStorage` (try/catch, validated on read). "Saved views" may be a named set in the same store; if they need a server API, defer them and record it.
3. **Selection** (`features/search/bulk/**`):
   - `x` or a checkbox selects a row, in both list and table. Shift-range selection is optional.
   - Selection survives appended pages and clears on a new query.
   - Announce the count through the existing polite region.
4. **Bulk bar:**
   - **Copy magnets:** newline-joined, through the existing magnet builder.
   - **Send:** the existing client endpoint. Show partial and duplicate results truthfully.
   - **Export:** only if the endpoint exists.
   - **Compare:** enabled for 2–5 selected, navigating to the existing compare route.

   Each action reports its result through the existing toast or announcement path.
5. **Sizes:** /search first load must not grow. Load the table, the chooser and the bulk bar on demand. Add size-limit entries at measured + 10%, report every delta, and never raise a limit.
6. **i18n:** `search_table_*` and `search_bulk_*` keys in `features/search/messages/{en,zh}.json`.
7. **Tests:**
   - vitest: column chooser persistence (including corrupt or unavailable storage), selection across pages and its reset, bulk Copy, Send partial and duplicate handling, and Compare enablement;
   - Playwright: table mode keyboard (header sort, row focus), select with `x`, the bulk bar, narrow-width containment, with axe;
   - condition waits only, no fixed sleeps;
   - **no baseline changes expected.** If screenshots change, stop and tell the runner.
8. **Decisions file:** `docs/decisions/<date>-S-search-5.md`.

## Own
`web/src/lib/features/search/table/**`, `web/src/lib/features/search/bulk/**`, minimal mount edits in `search/toolbar/**` and `search/results/**` (justify each in the PR), the message keys above, `web/e2e/search-table.*` and `web/e2e/search-bulk.*`, the size-limit entries, and the decisions file.

**Don't edit:** `lib/state/**`, `lib/keys/**`, `lib/shell/**`, `lib/ui/**`, `lib/api/**`. If you need something there, stop and tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, split it:
- **5a:** table mode and the chooser;
- **5b:** selection and the bulk bar.

## Done when
These pass in `web/`:
- `npm run check`
- **`npm run e2e:typecheck`** (not part of `check`)
- `npx vitest run`
- `npm run build`
- `npx size-limit`
- `npx playwright test` for the new and existing search specs, 3× for anything new (Chromium is at /opt/pw-browsers; never run `playwright install`)

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
