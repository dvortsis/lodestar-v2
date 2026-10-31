# S-insights-imports: show the crawled/imported split and import markers in Insights
**Model:** Sonnet: UI per §10.9, binding fields IMP-5 (#262) already added. Runner brief, 2026-10-31. The "IMP-5 UI follow-up". No `/security-review`: this renders server values as text and charts.

## Why
IMP-5 keeps imported records out of crawl-yield series and adds additive API fields and annotations for them. The screens don't show any of it yet, so an import is invisible, apart from an unexplained step in "Searchable torrents".

## Read first
- **`docs/decisions/2026-10-31-IMP-5.md`, "What the screens should show"** (the spec for this slice) and "What changed" (the fields).
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md` (wording), `docs/metrics.md` (formatting; never invent a series).
- `docs/architecture.md` §10.9.2 (events and annotations), §10.9.3 (integrity: two time bases labelled, no stacking of different things, units explicit), §10.9.6 (chart craft).
- The fields in `web/src/lib/api/schema.d.ts`: `kpi.searchable_crawled`, `kpi.imported`, `kpi.new_searchable_imported`, `previous.kpi.new_searchable_imported`, `trend[].searchable_imported`, Index's `new_searchable_imported`, and the import annotation kinds (`import_started`, `import_finished`, `import_removed`) with their counts.
- Existing code: `web/src/lib/features/insights/overview/**` (KpiRow, Movement, model.ts, fixtures.ts) and `stages/**` (StagePage, model.ts, fixtures.ts).

## Do
1. **Overview KPI "Searchable torrents":** the total, with "Crawled" and "Imported" beneath it. When an `import_finished` marker falls in the range, the Δ says it includes an import.
2. **"New searchable in range":** the crawled count. When `new_searchable_imported > 0`, a secondary "+ N imported" line (`formatCount`), never added into the headline or its delta.
3. **Movement chart:** the crawl's line as now. Draw imports as a separate, quieter series from `trend[].searchable_imported`, or leave them out; never stack them into the crawl's line. Add import annotations as markers: "Import started", "Import finished: N new, M already known", "Import removed: N deleted". The markers are keyboard reachable and have accessible text.
4. **Index stage:** "New searchable" crawled, with "Imported" beside it. The `import_removed` marker explains a removal spike.
5. **States:** zero imports render exactly as today, with no empty "Imported 0" clutter; decide and say in the decisions file whether a zero row shows.
6. **Sizes:** the Insights overview first load and the stage route must not grow past their limits. Add nothing to first load that can load with the chart. Never raise a limit; if you can't fit, stop and report the numbers.
7. **i18n:** `insights_import_*` keys in `features/insights/messages/{en,zh}.json`.
8. **Tests:**
   - vitest: model mapping for the split, "+ N imported" shown only when > 0, the Δ note, and marker text with counts;
   - Playwright: the overview and Index with an import in the mock (add it to the fixtures), keyboard reaching a marker, and axe;
   - condition waits only, no fixed sleeps;
   - **no existing baseline changes expected.** If screenshots change, stop and tell the runner.
9. **Decisions file:** `docs/decisions/<date>-S-insights-imports.md`.

## Own
`web/src/lib/features/insights/overview/**` and `stages/**` (only for these bindings), the `insights_import_*` keys, the Insights fixtures and mock data, `web/e2e/insights-imports.*`, and the decisions file. **Don't edit** `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`. If a field you need is missing, stop and tell the runner.

## Size
≲ 800 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Insights specs, 3× for anything new. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless-shell version doesn't match, run with an untracked local config that sets `executablePath: '/opt/pw-browsers/chromium'`, and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally. Push once per round of fixes.
