# B-refetch-funnel decisions

Closes the gap `2026-10-31-IMP-5.md` recorded: the start-time refetch of incomplete imports no longer lands in the crawl's funnel. Also takes the IMP-5 reviewer's four optional findings. No schema change, no API type change.

## Refetches stay out of the funnel (§10.9.2–10.9.3)
- **The mechanism is the key's cohort.** A refetch is queued under `REFETCH_COHORT` (`crates/pipeline/src/lib.rs`), a value no real hour reaches, instead of the start hour. Every funnel hook skips it through one predicate, `in_funnel(cohort)`:
  - the worker's `WorkerFunnel::record` (the pool's PeerFound and Fetched);
  - the coordinator's `lost` (every loss);
  - the writer's Stored, and its `STORED` rollup for a new row.
- **Why the cohort and not the lease's strategy:** the fetch pool (`lodestar_sources`, not this slice's) reports PeerFound and Fetched with only `(cohort, source)`, so the cohort is the one flag that reaches all three hooks without changing the `Funnel` trait. The coordinator's and writer's leases carry it too, restored from the queue.
- **`REFETCH_COHORT` is `i64::MAX / 3600`:** it sorts after every real hour (the queue's restore order, and the Insights floor's `min` over queued cohorts, so a refetch never holds back sealing), and the pool's `cohort * 3600` can't overflow. A refetch's lease takes this run's start hour as its discovery time, as it did before, so the filter and the hot columns see a real time.
- **The record still completes as today:** the writer merges the file list into the existing row (`Inserted::Existing`), and the gate counts it as `already_stored`.
- **A refetch saved in the fetch queue isn't restored.** Refetches are queued afresh from the store's `FILES_PARTIAL` rows at each start, so one whose import was removed while the server was stopped (`lodestar import` removes with the server stopped) is never fetched into a new, crawled-looking row, and a queue saved before this change can't put refetches back under an hour. Their attempt count resets each start, as a refetch dropped by the pool already did.
- **Result:** a refetch moves no `cohort_hour` row, no `FUNNEL_EVENTS` row (so not `stages.fetch` or the Fetch trend) and no `STORED` count. Tests: `refetches_stay_out_of_the_funnel` (a fetched and a lost refetch beside a crawl key in the same hour; the crawl's counts exact) and `a_saved_refetch_is_queued_afresh_from_the_store` (`crates/pipeline/tests/pipeline.rs`).

## The gate's origin split (master plan F14)
- **A refetch is never `origin="network"`.** One already stored, the normal case, was and stays `already_stored`. One that stores a new row, because the imported row was deleted while the fetch was in flight (a filter relabel or retention; `lodestar import` removes only with the server stopped), now counts under `origin="recovery"`, the label `docs/metrics.md` reserved, which this slice renders (`gate::Origin::Recovery`; `crates/lodestar/src/metrics.rs` renders every origin in `ORIGINS`, so it needed no change).
- **`discovery_to_searchable_seconds` is the network's only,** so a recovered row's wait never reads as crawl latency.
- **Left as it is:** such a row, rare, still looks crawled to the store's row-level counts (`NEWLY_SEARCHABLE` key 0, `rollup --repair`'s cohort Stored): its `IMPORTED` flag went with the deleted row. Fixing that needs a store change this slice doesn't own.

## No refetch counter of its own
- **Not added.** A refetch's success shows as `already_stored` (with have-filter misses), and every fetch is in the pool's `fetch_*` counters. A dedicated success/failure pair would need a new series and its render for a once-per-start batch; the record's `FILES_PARTIAL` flag clearing is the outcome that matters, and the Imports screen can read it. A later slice can add one if operators ask.

## IMP-5 polish
- **A job's removal is one annotation** (`annotate_removal`, `crates/store/src/import.rs`). The merge is keyed on the job alone, not on the last hour: a removal paused and resumed later adds to the same `import_removed` event, whose marker stays at the first step's time. This supersedes IMP-5's "A removal resumed later starts a new annotation". Test: `a_removal_is_one_annotation_per_job`.
- **The annotations CSV puts `at,kind,text` first again,** then `import.job`, `import.merged`, `import.new`, `import.removed`, `import.source`, so readers of the pre-IMP-5 columns by position keep working. `render` now keeps a panel's declared column order and appends any undeclared field, sorted; every other panel declares its columns sorted (a unit test checks), so their headers are unchanged. Golden `insights_export_annotations.txt` regenerated with `LODESTAR_BLESS=1`.
- **The digest shows imports beside "New searchable":** a line "+ N imported" (`format_count`) under the crawled count when `kpi.new_searchable_imported > 0`, in both the text and HTML parts, never added into the headline or its change. The wording is IMP-5's for the screens; `docs/ui-glossary.md` has no import entry yet, so the glossary's owner should add "Imported" there.
- **A job blocked by `interrupt_import_jobs` and never resumed or cancelled never gets `import_finished`;** that's acceptable, because its records written so far are already counted and the marker would claim a finish that didn't happen.
