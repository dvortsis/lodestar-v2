# X-snapshot-claims-retention decisions

Closes the gap `2026-11-16-ENR-terms.md` records under "Migration snapshots keep claims past their retention": a snapshot (`lodestar.pre-v<N>.db`, kept 90 days) held identify claims as they were, so a claim near its source's limit could outlive it inside the snapshot by up to 90 days, breaking TMDB's six months. This merges before ENR-tmdb (#948), as the runner decided on 2026-11-16. Architecture §7 and §12 (snapshots), §18.5 (Sources).

## The rule
- **The hourly snapshot pass drops identify claims by the live retention sweep's rules, or stricter** (`crates/store/src/snapshots/claims.rs`, called from `purge_one`). For every source with a terms record (`enr10/terms`), a claim goes when it's at or past the record's drop age (`max_age` less a day, floored at two days) or stamped more than an hour ahead. That's `identify::sweep_expired`'s window, at the same moment.
- **The clock never runs behind a mark.** The clock is the pass's `now`, but never behind the snapshot's own `enr10/clock` mark for the source, nor behind the highest mark the live store holds for any source. The live mark is the stricter extra: the snapshot's mark is older than the live one, so a clock that steps back after the snapshot was taken can't keep a claim either.
- **The shorter of two records applies.** When both the snapshot and the live store record terms for the same plugin, the shorter retention wins. The host rewrites a restored store's record with the plugin's current terms when it registers the plugin, so the live record is what a restored store ends up with.
- **An unreadable record means the default, 180 days**, as in the live sweep: the default is `Terms::STRICT`'s retention, and no plugin's terms allow longer.
- **A missing record means the source isn't an identify source**, and its claims stay, as live. The runtime records terms before a plugin's first claim (`identify::record_terms`), so an identify claim never exists without a record. A record the live store holds for the plugin still applies to the snapshot's claims.
- **Failing closed.** In practice a claim can't be checked in three ways. The record doesn't read: it gets the default, which is the strict reading. The stamp is in the future: the claim goes. The snapshot can't be queried: the pass leaves the snapshot as it is, warns every hour and deletes it at 90 days, and a rollback refuses it (`purge_for_restore` returns the error, so nothing is restored).
- **Dropped claims mark their torrent for the backfill** (`parser_ver = 0`), as `drop_one` does live, so a title they resolved leaves the record if the snapshot is ever restored. Torrents and every other row stay.

## Schemas
- **V15 and later only** (a new `claims` flag in `Plan`; V14 now has its own arm). V15 added the claim stamps. Its notes say no identify plugin ran on any store before it, so a V14 or older snapshot holds no claims with a retention. **No migration.**
- **An unknown newer schema** is still skipped with a warning and deleted at 90 days, as before.

## Bounded work
- **Chunked, resumable deletes by key.** The bundled SQLite has no `DELETE … LIMIT`, so the rows are chosen first, through the `(source_id, stored_at)` index V15 added: up to `CHUNK` (2,000) rows per transaction, each committing on its own. No cursor is needed. A deleted row no longer matches, so a pass stopped partway (by the cap or a crash) finds the rest due on the next.
- **New bound: `MAX_PER_PASS`, 1,000,000 claim rows per snapshot per pass.** That's above the fastest the identify runtime could write claims: one claim per looked-up item, under plugins' network rate caps of a few requests a second. Even its fastest pace through items, about 200 a second (`SKIP_PAUSE`, for items the gate refuses), would be 720,000 an hour, so a snapshot never falls behind. A backlog left over is reported as a warning and goes on the next hourly pass, well inside the one-day margin before the limit. The pass had no time or IO budget before. This is the only new bound.
- **A rollback's copy has no cap.** It must hold no expired claim before it's restored.
- **A snapshot with nothing due isn't written.** One indexed probe per identify source decides it, so its modification time (its expiry) isn't touched. The deletes run under `secure_delete` in the pass's WAL window, like the rest of the purge.

## Restore and rollback
- **`lodestar rollback` (`purge_for_restore`)** drops every expired claim from the copy and raises each identify source's retention mark in the copy to the clock it used. A restored store's own sweep therefore never runs behind the store it replaced, even when the wall clock has stepped back. Tested in `a_restore_brings_back_no_expired_claim_and_keeps_the_clock`. `crates/lodestar/src/cli/rollback.rs` needs no change.
- **The update rollback (`choose_snapshot`, `apply_source_purges`) is unchanged.** It restores only the snapshot the same failed update took minutes before, so the snapshot's claims and marks are the live store's as of that update. The restored store's own sweep (`identify::sweep_recorded`, at start and on a timer) applies the live rules from there.

## The terms-record reader (brief item 4)
- **The store has its own small reader of the stored record format.** `lodestar-sources` depends on `lodestar-store`, so the store can't use `Terms::from_record`. The reader needs only the retention: version byte 1, three flag bits, then eight big-endian bytes of `max_age` seconds. Anything else reads as the default.
- **`crates/pipeline/tests/snapshot_terms.rs` pins it.** Property tests over records `Terms::record` writes and over arbitrary bytes check that the store's drop age equals `Retention::drop_after` as the live sweep reads it, and that its window equals the sweep's. A plain test checks the keys, the slack and the default. This file is outside the brief's owns; the brief's item 4 asks for the pin test.
- **The dependency direction is unchanged.**
