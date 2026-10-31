# IMP-5: keep imported cohorts out of crawl-yield Insights; import annotations
**Model:** Opus: the store's reporting rollups and Insights semantics (§10.9.3 integrity). Runner brief, 2026-10-31. Lane I. Its dependency, IMP-4 (#253 driver, #257 CLI), has merged. **No schema change:** if you think you need one, stop and ask the runner (V14 is reserved for CR-3).

## Why
An import adds thousands to millions of records in minutes. Without separation:
- Insights' "Searchable torrents", "New searchable in range", the trends and the end-to-end yield would all show a crawl spike;
- the crawler gate's counters (`metadata_new_verified_total{origin=network}`, the master plan's Part A4 metric) could be polluted.

Imported records are not crawl discoveries. IMP-4 marks them with `flags::IMPORTED` (`crates/store/src/lib.rs:87`), with membership and quality rows. This slice makes reporting honour that.

## Read first
- `docs/decisions/2026-10-31-IMP-4.md`, "Imported cohorts keep their origin", and `-IMP-4b.md`.
- `docs/architecture.md`:
  - §10.9.3: discovery cohorts; two time bases, always labelled; final cohorts never change; removals are lifecycle events;
  - the rollup and cohort tables (`rollup_hour`, `cohort_hour`, `rollup_day`, `cohort_day`; around lines 195–210);
  - §10.9.2–10.9.5.
- `docs/metrics.md`: the crawler-gate counters with their origin split (master plan F14), and how Insights series are defined.
- `crates/store/src/{reporting.rs,insights.rs,insights_days.rs}`, `crates/api/src/insights.rs` and `insights/stages.rs`.
- The master plan's Part F14 (gate instrumentation, imports under a different origin) and H3 (the gate is measured on a fresh data dir; after an import, report novel against already-known separately).

## Do
1. **Rollups.** An imported record never enters a discovery cohort, the funnel or crawl yield.
   - Confirm what already happens on the import write path (does it bump `rollup_*` or `cohort_*`?).
   - Make every crawl-yield series exclude `IMPORTED` records: Searchable-new, the funnel, end-to-end yield, the Discover and Fetch stages.
   - "Searchable torrents" (a snapshot) may include imports, but must say so. Prefer a split: crawled vs imported.
   - Pick the cheapest correct mechanism within the §3 Insights rollup overhead budget (+≤ 5% p95, −≤ 2% ingest). Measure it.
2. **The gate counters.** The network-origin verified counters never count imports. Add or confirm an `origin` split (`network` / `import` / `recovery`) per F14, documented in `docs/metrics.md`.
3. **Annotations.** An import job's start and finish (and a removal) appear as Insights events or annotations on the trend charts (§10.9.2's events). They come from the job table, with counts, so a spike or step is explained, not hidden.
4. **API:** expose the split and the annotations through the existing Insights endpoints, as additive fields only. Regenerate `openapi.json` and `schema.d.ts`. The UI work is a separate small Sonnet follow-up; note in the decisions file what the screens should show.
5. **Novel vs known (H3):** add a counter or report field for imported records that were already present against new ones, so an operator can see the overlap.
6. **Tests:**
   - an import of N records leaves every crawl-yield series and the network gate counters unchanged;
   - "Searchable torrents" reflects the split;
   - annotations appear for start, finish and remove;
   - a crawl plus an import in the same hour keep separate counts;
   - removing an import doesn't rewrite final cohorts (§10.9.3);
   - a rollup-overhead measurement against the budget.
7. **Decisions file:** `docs/decisions/<date>-IMP-5.md`.

## Own
`crates/store/src/{reporting.rs,insights.rs,insights_days.rs}` and the import write path's reporting hooks, `crates/api/src/insights*`, `crates/lodestar/src/metrics.rs` (the origin split), `docs/metrics.md`, `web/src/lib/api/openapi.json` and `schema.d.ts` (regenerated only), and your decisions file.

## Size
≲ 1,500 changed lines.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the openapi drift check passes;
- `npm run check` passes in `web/` (types only).

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
