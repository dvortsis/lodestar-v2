# X-snapshot-claims-retention decisions

Closes the gap `2026-11-16-ENR-terms.md` records under "Migration snapshots keep claims past their retention": a snapshot (`lodestar.pre-v<N>.db`, kept 90 days) held identify claims as they were, so a claim near its source's limit could outlive it inside the snapshot by up to 90 days, breaking TMDB's six months. This merges before ENR-tmdb (#948), as the runner decided on 2026-11-16. Architecture §7 and §12 (snapshots), §18.5 (Sources).

## The rule

- **The hourly snapshot pass drops identify claims by the live retention sweep's rules, or stricter** (`crates/store/src/snapshots/claims.rs`, called from `purge_one`). For every source with a terms record (`enr10/terms`), a claim goes when it's at or past the record's drop age (`max_age` less a day, floored at two days) or stamped more than an hour ahead. That's `identify::sweep_expired`'s window, at the same moment.
- **The clock never runs behind a mark.** The clock is the pass's `now`, but never behind the snapshot's own `enr10/clock` mark for the source, nor behind the live store's mark for the same plugin. The live mark is the stricter extra: the snapshot's mark is older than the live one, so a clock that steps back after the snapshot was taken can't keep a claim either. Only the plugin's own marks count, as in the live sweep, so one plugin's mark (set while the clock ran fast, say) never empties another's claims.
- **The shorter of two records applies.** When both the snapshot and the live store record terms for the same plugin, the shorter retention wins. The host rewrites a restored store's record with the plugin's current terms when it registers the plugin, so the live record is what a restored store ends up with.
- **An unreadable record means the default, 180 days**, as in the live sweep: the default is `Terms::STRICT`'s retention, and no plugin's terms allow longer.
- **A missing record means the source isn't an identify source**, and its claims stay, as live. The runtime records terms before a plugin's first claim (`identify::record_terms`), so an identify claim never exists without a record. A record the live store holds for the plugin still applies to the snapshot's claims.
- **Failing closed.** A claim can't be checked in three ways:
  - **The record doesn't read:** the claim gets the default, which is the strict reading.
  - **The stamp is in the future:** the claim goes.
  - **The snapshot's claims can't be checked, however often the pass tries:** the claims stage meets `SQLITE_CORRUPT` or `SQLITE_NOTADB`, a missing table or column, or (for a newer schema) stamped claims without `sources`, `source_state` or `torrent_parsed.parser_ver`. Then the hourly pass deletes the whole snapshot and lists it in `PurgeReport::unchecked`. The writer logs each such deletion by path as `pipeline.snapshot_expired`, whose new `reason` field says "identify claims could not be checked against their terms". Both rollbacks refuse such a snapshot: `purge_for_restore` and `apply_source_purges` return the error, so nothing is restored.
  - **Any other error is retried, never a reason to delete** (runner, 2026-11-20). That covers a busy lock after the 200 ms timeout (the writer's hourly pass and `finish_purge`'s can meet), I/O errors (antivirus, network shares), a full disk, no memory and an interrupt. The pass warns and tries again next hour. The one-day margin before the limit leaves 24 passes, so a transient error never costs the user's rollback point. Tested in `a_busy_snapshot_is_kept_and_retried` and `only_lasting_errors_fail_closed`.
- **Dropped claims mark their torrent for the backfill** (`parser_ver = 0`), as `drop_one` does live, so a title they resolved leaves the record if the snapshot is ever restored. Torrents and every other row stay.

## Schemas

- **V15 and later only** (a new `claims` flag in `Plan`; V14 now has its own arm). V15 added the claim stamps. Its notes say no identify plugin ran on any store before it, so a V14 or older snapshot holds no claims with a retention. **No migration.**
- **An unknown newer schema** (a downgrade's leftover) still gets none of the other purges, but if it has stamped claims (`torrent_claims.stored_at`), they go by the same rules, with `foreign_keys` on, so a newer cascade leaves no orphan rows holding claim data. If it has no stamped claims, it's left as before and deleted at 90 days. If it has them without `sources`, `source_state` or `torrent_parsed.parser_ver` to check them by, or they can't be read for a lasting reason, the snapshot is deleted (failing closed). A transient error is retried.

## Bounded work

- **Chunked, resumable deletes by key.** The bundled SQLite has no `DELETE … LIMIT`, so the rows are chosen first, through the `(source_id, stored_at)` index V15 added: up to `CHUNK` (2,000) rows per transaction, each committing on its own. No cursor is needed. A deleted row no longer matches, so a pass stopped partway (by the budget, the cap or a crash) finds the rest due on the next.
- **New bound: a time budget, `PASS_BUDGET`, 30 seconds of claim deletes per hourly pass**, shared by every snapshot (runner, 2026-11-20). The pass runs inside the writer's `spawn_blocking` with the store taken, so this keeps it under 1 % of the hour. It's checked between chunks, so a pass overruns it by one chunk at most. A backlog left over is reported as a warning and goes on the next pass. The one-day margin before the limit leaves 24 passes for it.
- **A backstop row cap, `MAX_PER_PASS`:** 1,000,000 claim rows per snapshot per pass, which the time budget normally reaches first.
- **A rollback's copy has neither limit.** It must hold no expired claim before it's restored, and a rollback is a one-off that runs with the store closed.
- **A snapshot with nothing due isn't written.** One indexed probe per identify source decides it, so its modification time (its expiry) isn't touched. The deletes run under `secure_delete` in the pass's WAL window, like the rest of the purge.

## Restore and rollback

- **`lodestar rollback` (`purge_for_restore`)** drops every expired claim from the copy and raises each identify source's retention mark in the copy to the clock it used. A restored store's own sweep therefore never runs behind the store it replaced, even when the wall clock has stepped back. Tested in `a_restore_brings_back_no_expired_claim_and_keeps_the_clock`. `crates/lodestar/src/cli/rollback.rs` needs no change.
- **The update rollback (`choose_snapshot`) gets the same treatment, through `apply_source_purges`.** Roll back is offered for as long as the `pre-v<N>` snapshot is kept, up to 90 days, so the snapshot can be far older than the store it replaces. `apply_source_purges` already runs on it before it's verified and hashed. It now also drops every expired claim (no limit) by the live store's terms and marks, and raises the marks, the same as `purge_for_restore`. An error refuses the snapshot, as `choose_snapshot` already does for any scrub failure. `choose_snapshot` passes its clock in (`util::unix_now()`), as `lodestar rollback` does for `purge_for_restore`. That is a one-line change to `crates/lodestar/src/update/journal.rs`, which the runner asked for. Tested in `an_update_rollback_brings_back_no_expired_claim`.
- **Titles a dropped claim resolved.** Dropping a claim marks its torrent for the backfill (`parser_ver = 0`), but the backfill doesn't run on a snapshot. So a title a redistributable source (Wikidata, MusicBrainz) resolved stays in the snapshot's `torrent_parsed` until it's restored, which rebuilds it, or until the snapshot expires. That's within those sources' terms: CC0, which allows redistribution. TMDB's terms are `redistribute: false` (ENR-terms), so its answers never enter a record, and its claims leave the snapshot by the rules above.

## The terms-record reader (brief item 4)

- **The store has its own small reader of the stored record format.** `lodestar-sources` depends on `lodestar-store`, so the store can't use `Terms::from_record`. The reader needs only the retention: version byte 1, three flag bits, then eight big-endian bytes of `max_age` seconds. Anything else reads as the default.
- **`crates/pipeline/tests/snapshot_terms.rs` pins it.** Property tests over records `Terms::record` writes and over arbitrary bytes check that the store's drop age equals `Retention::drop_after` as the live sweep reads it. Another runs `identify::sweep_expired` on a real store, with stamps clustered at both edges, and checks it drops a claim exactly when the store's window does. A plain test checks the keys, the slack and the default. This file is outside the brief's owns; the brief's item 4 asks for the pin test.
- **The dependency direction is unchanged.**

## Shared edits (runner, 2026-11-20)

- **`crates/pipeline/src/writer.rs` and `crates/pipeline/src/log_events.rs`:** `pipeline.snapshot_expired` gains a `reason` field (a label, so the log catalogue's kinds and the settings golden don't change), and its message becomes "Deleted a snapshot". The writer logs a fail-closed deletion by path with it.
- **`crates/lodestar/src/update/journal.rs`:** the clock passed to `apply_source_purges`.
