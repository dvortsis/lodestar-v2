# X-snapshot-followups decisions

The items #980's delta review left, and one older retention bug it found. The runner gave them to the same session on 2026-11-20. This builds on `2026-11-20-X-snapshot-claims-nits.md`, and the newest file wins where they differ.

## 1. A snapshot's time survives every exit after a WAL fold (bug, predating #976)

- **The bug:** folding a WAL left beside a snapshot writes the file and moves its modification time to now. The expiry goes by that time. So an error after the fold made the snapshot live up to 90 days longer than it should: an early `?` in `purge_one` or `purge_claims_only`, or `apply_purges_only` returning early with nothing to scrub, all skipped putting the time back. Claims were still purged every hour, so this hit retention, not safety.
- **The fix:** a `KeepTime` guard, declared before the snapshot's connection. Locals drop in reverse order, so on every way out the connection closes first and the guard then puts the time back. It's in `settle`, `purge_one`, `purge_claims_only` and `apply_purges_only`. It always restores the time it read; on a path that wrote nothing, that's the time already there.
- **Tested:** `an_error_after_the_fold_keeps_the_time` folds a left-over WAL and then fails the first read after it, in the hourly pass and in the claims-only purge. Disabling the guard in either function fails the test.

## 2. The two sightings wait a real hour (runner's call)

- **The change:** a lasting error's first sighting now records the monotonic clock (`Instant`) beside the wall-clock time. The one-hour wait before deletion (`CONFIRM_SECS`) is measured on the monotonic clock. A wall clock stepping forward between two torn reads therefore can't shorten it.
- **Unchanged:** a wall clock that steps back behind the first sighting still starts the wait again. The record lives in memory only, so a restart starts it again too.
- **Seam:** `purge_snapshots` passes `Instant::now()` to `purge_snapshots_at`. Tests pass a clock of their own, with no sleeps.
- **Tested:** `the_two_sightings_wait_a_real_hour` covers both clock directions. Measuring on the wall clock instead fails it.

## 3. Test gaps from the #980 review

- **`a_retried_error_resets_the_two_sightings`:** a lasting sighting, then a retried error, then a lasting sighting an hour later keeps the snapshot. It runs on a known schema (`purge_one` meets an exclusive lock) and on a newer one (the claims-only purge meets a writer's lock at its switch to WAL). Keeping the sighting through a retried error, in either branch, fails it.
- **`skipped_and_newer_passes_clear_the_stuck_record`:** the stuck record clears on a pass that skips a snapshot with no schema version, as an older schema is skipped, and on one that purges a newer schema's claims. Stuck again, it's reported at once. Removing either clear fails it.

## 4. A stuck snapshot's new kind of error is reported at once

- **The change:** the stuck record keeps the kind it last reported (`busy`, `can't be checked`, …). A different kind is reported at once, outside the once-a-day limit. Before, a "busy" report hid a later "can't be checked" sighting for up to a day, so the `pipeline.snapshot_unchecked` deletion could come with no stuck warning before it.
- **No log-event change.** `pipeline.snapshot_stuck` already carries the kind.
- **Tested:** `a_new_kind_of_stuck_is_reported_at_once`. Ignoring the kind fails it.
