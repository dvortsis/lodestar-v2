# X-snapshot-followups-2 decisions

The optional findings from #987's fresh review, given to the same session by the runner on 2026-11-20 (low priority). This builds on `2026-11-20-X-snapshot-followups.md`, and the newest file wins where they differ.

## A failed time restore says so
- **Before:** `KeepTime` ignored `set_modified`'s error. A snapshot whose time couldn't be put back after a purge wrote it could outlive its 90 days with no sign.
- **Now:** the failure is recorded in a thread-local `Cell`, which the hourly pass reads after each snapshot's purge (`note_restore`). It adds one warning naming the snapshot: "the date of the snapshot … couldn't be put back after Lodestar wrote to it; it may be kept longer than its 90 days". The pass runs on one thread, so a thread-local is the simplest seam. The pass clears it at the start, so a restore by `settle` or a rollback is never reported as the pass's.
- **Not a warning:** a file that's already gone (`NotFound`).
- **Tested:** `a_failed_time_restore_is_reported`, through a test-only switch (`FAIL_RESTORE`) that makes every restore on the thread fail.

## No write when the time is already right
- **The change:** `KeepTime` compares the file's time, to the second, with the time it would put back, and writes only when they differ. A pass with nothing to purge therefore no longer rewrites every kept snapshot's time each hour. It still opens each one through SQLite to look.
- **Tested:** `a_pass_with_nothing_to_do_writes_no_time` gives the file a sub-second time, which a write would drop, and checks that it survives an idle pass. It covers both the full purge and the claims-only purge.

## The old restore paths are gone
- **What's removed:** `purge_one`'s `restore_time` alias, `purge_claims_only`'s `restore(conn, _)` closure, and `apply_purges_only`'s manual restore with its `taken` local.
- **Why it's safe:** the guard is declared before the connection, so plain drop order closes the connection first. Where a function closes it explicitly before its result is read, a one-line comment says the order matters.

## A pass that looks at no snapshot forgets every sighting
- **The wording:** #980 and #987 said a lasting error deletes a snapshot when "the last pass found the same". But a pass that returned early kept the first sighting. That happens when the live store's holds can't be read, or when no snapshot is left to purge. Such a pass looked at nothing, yet the next pass could still delete.
- **The choice, failing toward keeping:** every way a pass can return before looking now clears every sighting: the snapshot folder can't be listed, the live store's holds can't be read, or no snapshot is left. A snapshot whose time can't be read this pass also loses its own sighting. A deletion therefore always needs two sightings, at least an hour apart on the monotonic clock, by passes that each looked at the snapshot, with no pass in between that didn't look. The doc comments say so.
- **Tested:** `a_pass_that_looks_at_nothing_forgets_the_sightings` covers the holds case. The other clears are defensive and untested, because they can't be reached without fault injection. A listing failure needs the folder unreadable, which root ignores. No snapshot being left means no sighting can remain anyway. An unreadable time needs a metadata error other than NotFound.

## Mutation-checked by hand
Each new test fails when its behaviour is removed: the warning, the no-write skip, and the clear on a failed read of the live store's holds.
