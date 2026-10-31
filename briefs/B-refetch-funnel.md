# B-refetch-funnel: keep start-time refetches of partial imports out of the funnel; IMP-5 polish
**Model:** Opus: the pipeline's worker, coordinator and writer (concurrency) and Insights integrity (§10.9.3). Runner brief, 2026-10-31. Follow-up to IMP-5 (#262), as the runner decided (option 1). **No schema change.**

## Why
`docs/decisions/2026-10-31-IMP-5.md`, "A gap this PR doesn't close", explains it:
- At each server start the pipeline queues up to 50,000 `FILES_PARTIAL` hashes for re-fetch (`crates/pipeline/src/lib.rs`, `strategy::REFETCH`), under a cohort of the start hour.
- Those fetches go through the funnel like a crawl's. The worker records PeerFound and Fetched, the coordinator records losses, and the writer records Stored even when the row is `Existing`.
- So the start hour's cohort shows fetched, stored and lost counts with no matching harvest. `stages.fetch` and the Fetch trend (`FUNNEL_EVENTS` `Fetched`) count them too. Yield isn't inflated, but conversions and loss shares for that hour are skewed.

IMP-5's reviewer also left four optional findings in the same code. This slice takes them.

## Read first
- `docs/decisions/2026-10-31-IMP-5.md` (whole file) and `-IMP-4.md`, `-IMP-4b.md`.
- `docs/architecture.md` §10.9.2–10.9.3 (cohorts, final cohorts never change) and `docs/metrics.md` (the funnel and Fetch-stage series, the gate counters' origin split).
- `crates/pipeline/src/**` (`strategy::REFETCH`, leases, the worker, coordinator and writer funnel hooks), `crates/store/src/{reporting.rs,import.rs}`, `crates/api/src/{insights/csv.rs,digest.rs}`.

## Do
1. **Refetch leases skip funnel recording** in the worker, coordinator and writer: no cohort row, no `FUNNEL_EVENTS` row, no Fetch-stage count. Pick the cheapest correct mechanism (a lease-origin flag carried to each hook is likely simplest). A refetch must still update the record itself (file list completion) as today.
   - Check the gate counters: a refetch is never `origin=network`. If it's counted anywhere today, move it to `origin=recovery` (or whatever `docs/metrics.md` names) and document it.
   - If a refetch's success or failure is worth seeing, expose it as its own counter (not in the funnel), documented in `docs/metrics.md`. Optional; say what you chose.
2. **IMP-5 polish (the #262 reviewer's optional findings):**
   1. `crates/store/src/import.rs` `annotate_removal`: the one-hour window also splits one long continuous removal into several annotations. Key the merge on the job alone (preferred), or reword the doc comment and the decisions file to match the behaviour.
   2. `crates/api/src/insights/csv.rs`: the annotations CSV used to be `at,kind,text`; IMP-5 put `import.*` columns between `at` and `kind`, which breaks positional readers. Put `at,kind,text` first and the `import.*` columns after them; regenerate `insights_export_annotations.txt` with the repo's tooling.
   3. `crates/api/src/digest.rs`: "New searchable" is crawled only. Add an "+ N imported" line when `new_searchable_imported > 0`, worded per `docs/ui-glossary.md`.
   4. A job blocked by `interrupt_import_jobs` that is never resumed or cancelled never gets `import_finished`. That's acceptable; say so in one sentence in your decisions file.
3. **Tests:** a refetch moves no cohort and no `FUNNEL_EVENTS` row and no network gate counter, while the record still completes; a crawl and a refetch in the same hour keep the crawl's counts exact; the CSV column order; the digest line; the removal-annotation merge. Condition waits only, no fixed sleeps.
4. **Decisions file:** `docs/decisions/<date>-B-refetch-funnel.md`. Update IMP-5's decisions-file "gap" paragraph to point at it (one line).

## Own
`crates/pipeline/src/**` (funnel hooks and lease origin only), `crates/store/src/{reporting.rs,import.rs}` (only as above), `crates/api/src/{insights/csv.rs,digest.rs}` and their goldens (regenerated), `crates/lodestar/src/metrics.rs` (only if a counter moves), `docs/metrics.md`, the IMP-5 decisions file's gap paragraph, and your decisions file. Regenerate `openapi.json` and `schema.d.ts` only if a type changes.

## Size
≲ 800 changed lines.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the openapi drift check passes;
- `npm run check` passes in `web/` if `schema.d.ts` changed.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally. Push once per round of fixes.
