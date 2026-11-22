# X-snapshot-claims-nits decisions

These are the optional notes from the runner's review of #976 (X-snapshot-claims-retention), given to the same session as a follow-up by the runner on 2026-11-20. This builds on `2026-11-20-X-snapshot-claims-retention.md`, and the newest file wins where they differ.

## O1: which branches the tests reach
- **`a_busy_snapshot_is_kept_and_retried`:** an exclusive lock stops `purge_one` at its first schema read. So it covers the purge's existing `Err` path and the daily stuck report, not the claims stage. Its doc comment now says so.
- **`a_lock_in_the_claims_stage_is_retried` (new):** a newer-schema snapshot under another connection's write lock. The lock still lets the pass read, so the error comes from the claims stage's own first write, the switch to WAL. That is the newer arm's retry branch. The test checks the snapshot is kept and named as stuck, and that the next pass purges it.
- **Covered by the classifier unit test only:** `purge_one`'s claims-stage read and delete errors. They can't be met without fault injection, so `only_lasting_errors_fail_closed` covers the classifier those branches share.

## O2: the time budget counts only the claims deletes, and the order rotates
- **`PASS_BUDGET` (30 s) is now a budget each claims purge draws down by the time it spends.** Before, it was a deadline set before the snapshot loop. The rest of the purge (journal, holds, filters) no longer counts against it.
- **The pass starts at a different snapshot each hour.** The starting snapshot is `(now / 3600) mod n` (`first_index`), needing no stored state. Every snapshot therefore goes first at least once every n passes, so a backlog in one can't keep the budget from the others for longer than that.
- **The rate argument, restated:** a snapshot's claims come due at the rate the live store wrote them, one per looked-up item.
  - The live store writes at most the identify runtime's pace. That's a few requests a second under plugins' rate caps, and even its fastest pace through items is about 200 a second.
  - Deletes run in 2,000-row transactions and reach thousands of rows a second, so 30 s an hour clears more than an hour of the live store's normal claim writes.
  - A backlog beyond that is named in a warning each pass. The one-day margin before the limit (`drop_after`) leaves 24 passes to clear it.
  - The 1,000,000-row cap per snapshot per pass stays as a backstop.

## O3: a snapshot stuck on a retried error is reported by path
- **The runner's call stands:** a rollback point is never deleted for an error that may be transient. There is no deletion backstop.
- **Instead, a stuck snapshot is reported by path.** A snapshot whose purge fails with a retried error is listed in `PurgeReport::stuck`, with a short, fixed name for the kind of error (busy, locked, I/O error, disk full, and so on). The writer logs it as the new event `pipeline.snapshot_stuck`.
  - **When:** when it first fails, then at most once a day per snapshot while it keeps failing. A clock that steps back reports again.
  - **Resetting:** a pass that purges it, or finds nothing to do, clears the record.
  - The record lives on the writer's `Store`, beside the snapshot marks, for the life of the process.

## O4: log lines and copy
- **`pipeline.snapshot_expired` is back to what it was before #976:** age only, with its fields `what`, `path` and `days`, and its message. Its UI copy ("Old snapshot removed") is right again.
- **New event `pipeline.snapshot_unchecked` (warn, `path`, "Deleted a snapshot that couldn't be checked")** is logged for a fail-closed deletion. It replaces the `reason` field #976 added. That fixes the `days = RETAIN_DAYS` line, which was wrong for a snapshot that wasn't deleted by age.
- **Both new events have catalogue rows:** `log_events.rs`, the settings golden, `event-names/pipeline.ts`, and English and Chinese copy.
- **The warning for claims left over** now names both limits: the time budget and the row cap.

## Files outside the owns
- **`crates/lodestar/src/settings/golden/logs_catalog.txt`:** regenerated for the two new events (`LODESTAR_BLESS=1`).
- **`web/.../event-names/pipeline.ts`:** the two new events' names, beside their message keys. This goes with the catalogue rows the runner allowed.

## A lasting error must be seen twice before a snapshot is deleted (runner, 2026-11-20)
- **The rule:** a snapshot whose identify claims can't be checked for a lasting reason (`SQLITE_CORRUPT`, `SQLITE_NOTADB`, a missing table or column, or a newer schema without what the check reads) is deleted only when the same is found on a later pass at least an hour after the first sighting (`CONFIRM_SECS`).
  - **The first sighting** keeps the snapshot, records when it was seen, and reports it as stuck (`can't be checked`, logged as `pipeline.snapshot_stuck`).
  - **Any other outcome in between resets the count:** a pass that checks the claims, finds nothing to do, or meets a retried error.
  - **A rollback still refuses such a copy at once.** Refusing deletes nothing.
- **Why:** this applies the rule of never deleting a rollback point on a possibly transient error. On filesystems with weak locking (Unraid's user shares, SMB), a torn read can report damage that then goes away. The writer's hourly pass and `finish_purge`'s pass can also meet on one snapshot.
- **Terms compliance holds.** The extra hour sits well inside the one-day margin before the limit.
- **Loosens #976:** #976 deleted on the first sighting. The runner flags this to the owner under §2.19.
- **Tested:** `a_snapshot_whose_claims_cannot_be_checked_is_deleted`, `a_newer_schema_that_cannot_be_checked_is_deleted` and `a_clean_pass_resets_the_two_sightings`.

## Stuck-record resets and rotation, tested
- **The stuck record clears** on any pass that purges the snapshot, finds nothing to do, or skips an older schema. `a_busy_snapshot_is_kept_and_retried` checks that a snapshot stuck again after a clean pass is reported at once.
- **Rotation** has a unit test of `first_index`, and an integration test where two snapshots are purged in one order and then the other.
- **Both fail when the behaviour is removed** (checked by hand).
