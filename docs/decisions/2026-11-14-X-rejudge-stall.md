# X-rejudge-stall decisions

The core re-judge (§20.5, `crates/pipeline/src/core_rejudge.rs`) no longer stalls on a batch its breaker stops. The breaker (`BREAKER_ROWS`) fails a batch when more than half of its stored torrents, and more than 16, won't read, for example on a damaged dictionary. Before, the whole pass retried that batch from the same place indefinitely, so nothing after it was ever judged again.

## What the code does
- **The breaker's range is kept, and the pass moves past it.**
  - **What's kept:** the batch's phase, the id it started after, its last id, and how many stored torrents it covered.
  - **Where:** the settings key `filter.core.stalled` (`STALLS_KEY`), with the core version it's for: `v<version>;<phase>:<after>:<to>:<rows>;…`. Ids and counts only. This uses the existing text-settings calls, so the store crate and its schema are unchanged (no migration).
  - **Order:** the range is written before the pass's place moves past it. A restart between the two finds the batch again, the breaker stops it again, and the same range is kept once.
  - **Nothing in the range is touched:** no hold is written and no row changes. Rows that were held stay held, and nothing is stored or shown as passed because it was skipped.
- **The pass stays unfinished while any range is left.**
  - When the second phase ends with ranges left, the pass moves to a third phase (`RETRY_PHASE`) instead of finishing. Its marker stays, so the store isn't marked as judged by today's core, `is_current` stays false, and `note_judged` can't advance the high-water id.
  - The pass finishes only when no range is left. The ranges' key is removed first, then the store's pass keys. A restart between the two finds no range and finishes.
- **The ranges are judged again, oldest first, in their own phase.**
  - A range judged through is removed. One judged in part shrinks to what's left of it.
  - The breaker stops a range again: it goes to the back of the list, so the next try starts with another one, and the writer waits.
  - **The wait:** 5 min, doubling after each try the breaker stops again, up to 6 h (`STALL_RETRY_FIRST`, `STALL_RETRY_MAX` in `writer.rs`). The first run after a start tries at once, which covers a new build (a core change) and a repaired store.
  - **While waiting,** the passes that wait for the re-judge (the candidate re-file, the unverified sweep, the backfill) run. Before, a stalled pass kept them waiting too, in its store-error backoff.
- **Shown in health:**
  - **Gauges:** `lodestar_core_rejudge_stalled_ranges` and `lodestar_core_rejudge_stalled_rows`, row in `docs/metrics.md`.
  - **Log events:** `pipeline.core_rejudge_stalled` (Warn) when a range stops, and `pipeline.core_rejudge_stall_cleared` (Info) when one is judged through. Counts only: the range's rows and the ranges left.
  - **Retries the breaker stops again aren't logged,** so a store that stays damaged doesn't log every few hours. The gauges show the ranges are still there.
  - **`pipeline.core_rejudge_failed`** is now only for other store errors.

## Choices (the runner's call to confirm)
- **Ranges are per phase.** A torrent's first-phase range is judged again in the first phase. Its second-phase pass over the same ids may stop a second range there, which is kept separately.
- **The list is bounded.** A range that follows one of its phase extends it; the same range again is kept once. Past 1,024 ranges (`MAX_STALLS`), a new one widens the last range of its phase. Widening only judges again torrents already judged, which changes nothing they show.
- **A value that won't read** is taken as every torrent of the pass, in both phases (after the pass's start, to the end), so a damaged key never lets a range be skipped. Ranges kept for another core version belong to a pass that's gone (a new pass judges every torrent again), and are dropped.
- **Rows are approximate.** They're the stored torrents a range covered when it stopped, less those judged since. Merged ranges add their rows.

## Not changed
- **A missing compression dictionary** isn't the breaker: it's a store error, retried from the same place with the existing backoff, then one torrent at a time (`a_missing_dictionary_fails_the_batch`). The brief covered the breaker only; the runner can queue the missing-dictionary case if it wants it handled the same way.
- **A few unreadable torrents among many that read** are still held as unjudgeable, one by one, as before.
- **The store crate** isn't touched (#879 owns it).

## Also in this PR
- `update/supervisor.rs`: `child_args`'s doc comment moved back above `child_args` (the runner's nit from #869).
- **Required for the new log events,** outside the brief's paths: the event catalogue golden, the generated web event names and the English and Chinese name and help (`web/src/lib/features/settings/logging/`).
- **The pipeline event-names chunk's size budget** (`web/.size-limit.mjs`) goes from 2,560 B to 3,010 B: measured 2,704 B with the two new events, plus 300 B, by the rule `X-event-names-split` recorded. The chunk had used up its room for new events. A size budget is the runner's call (`docs/owner-decisions.md` §2.26).
- **Required for the gauges,** outside the brief's paths: `crates/lodestar/src/metrics.rs`. The core re-judge's progress wasn't reported in health before; it was only in logs.
