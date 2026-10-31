# S-insights-2: the Insights stage pages
**Model:** Sonnet: UI per §10.9.5, following S-insights-1's overview. Runner brief, 2026-10-31. M5 screen backlog: Insights, part 2 of 3. Its dependencies have merged: S-insights-1 (the overview) and B11b (the five stage endpoints, `export.csv`, compaction). No `/security-review`: this renders server values as text and charts.

## Read first
- CLAUDE.md's UI rules (tiny in-house SVG charts plus lazily loaded uPlot; no Tremor), `runner/standards.md` and `docs/owner-decisions.md`.
- `docs/architecture.md` §10.9:
  - §10.9.1, the stage strip;
  - §10.9.3, the analytical integrity rules;
  - §10.9.4, the interaction map;
  - **§10.9.5, the stage detail pages (the spec);**
  - §10.9.6, chart craft, states and layout.
- `docs/metrics.md`: the formatting rules, and which series exist. **Never invent a series.**
- The API: `crates/api/src/insights.rs` and `insights/stages.rs`; take shapes from `web/src/lib/api/schema.d.ts`.
- Existing code: `web/src/lib/features/insights/overview/**`, the S-insights-1 brief and decisions file (`git show origin/runner-state:briefs/S-insights-1.md`), and `web/src/routes/insights/[stage]/+page.svelte` (today an `UnavailablePage` placeholder).
- The `ui-design-skills` dashboard-storytelling and analytics-chart-selection guidance, applied in spirit as §10.9 already does.

## Do
1. **The five pages** at `/insights/{discover|fetch|understand|index|serve}` (`features/insights/stages/**`). Each follows the pattern: 3–4 KPIs, one primary trend, then drivers, then detail tables.
   - Include only the metrics §10.9.5 lists **that the stage endpoint actually returns.** Anything missing gets an honest "not available yet" in place, never a fake zero. List each missing item in the decisions file, with the series it needs.
   - Serve's top queries appear only when query logging is on (it's off by default).
   - Understand's blocklist drops are counts only: blocked content is never shown.
2. **Context:** a breadcrumb back to the overview, preserving the period and compare context in the URL. The stage strip highlights the current stage.
3. **Integrity (§10.9.3):**
   - "≈" where sampled;
   - stale flags;
   - cohort finality;
   - units explicit;
   - no dual axes;
   - empty, loading, error and partial states designed separately.
4. **CSV:** an export link per page if B11b's `export.csv` supports the stage.
5. **Sizes:** each stage route must stay within the §10.1 per-view budget, and the overview's first load must not grow. Load uPlot lazily. Add a size-limit entry per stage route at measured + 10%, and never raise an existing limit.
6. **i18n:** `insights_stage_*` keys in `features/insights/messages/{en,zh}.json`.
7. **Tests:**
   - vitest: per-page data mapping (including missing series becoming "not available"), formatting and "≈";
   - Playwright: each page renders against the mock with keyboard navigation, breadcrumb and context preserved, with axe;
   - condition waits only, no fixed sleeps;
   - **no baseline changes expected** outside new specs. If existing screenshots change, stop and tell the runner.
8. **Decisions file:** `docs/decisions/<date>-S-insights-2.md`.

## Own
`web/src/lib/features/insights/stages/**`, `web/src/routes/insights/[stage]/**`, the `insights_stage_*` message keys, `web/e2e/insights-stages.*`, the size-limit entries, and the decisions file.

**Don't edit:** `features/insights/overview/**` beyond the stage-strip link (justify it), nor `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`. If you need an API field that's missing, stop and tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, split it:
- **2a:** Discover, Fetch, Understand;
- **2b:** Index, Serve.

## Done when
These pass in `web/`:
- `npm run check`
- **`npm run e2e:typecheck`**
- `npx vitest run`
- `npm run build`
- `npx size-limit`
- `npx playwright test` for the new and the existing Insights specs, 3× for anything new (Chromium is at /opt/pw-browsers; never run `playwright install`)

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
