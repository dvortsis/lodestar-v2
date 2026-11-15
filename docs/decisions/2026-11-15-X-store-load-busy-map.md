# X-store-load-busy-map decisions

A follow-up the runner logged from #908's review (2026-11-15): `Store::load` read the active file dictionary's check as `codec_has(..).ok() != Some(true)`, so a read that failed, a busy or locked store above all, was reported as `Corrupt("active file dictionary is missing")`. A store that's only busy must never read as corrupt: that invites a repair or reset nobody needs.

## What changed (`crates/store/src/lib.rs`)

- **A failed read stays its own error.** `Store::load` now calls `active_dict_present`, which returns `Corrupt` only when the read answered and the dictionary isn't there. Any read error, busy or locked included, comes back as the `Sqlite` error it is.
- **`Error::is_busy()`** says whether another connection's lock refused the call (SQLite busy or locked). Nothing is wrong with the store then, and the same call can be retried.
- **A busy store says so in words.** `Display` for a busy or locked `Sqlite` error reads "the store is busy: another process holds it (…). Nothing is wrong with the store; try again once that process has finished" instead of "sqlite: database is locked". Every caller that shows the store's error (the server at start, the CLI) shows that sentence. No other error's text changed.
- **Not touched:** the migration code in `crates/store/src/schema.rs`, which slot 2's X-store-migrate-race owns.

## Tests

- `a_busy_dictionary_check_is_busy_and_a_missing_one_is_corrupt`: the check against a store another connection holds exclusively is busy (busy timeout 0, so no wait), its text says to try again and never "corrupt"; a dictionary that's really missing is `Corrupt`.
- `opening_a_held_store_is_busy_never_corrupt`: opening a held store fails as busy once the store's busy timeout runs out (bounded, no fixed sleep), opens once free, and a store whose active dictionary is gone is `Corrupt`.
- The window the old code got wrong (the lock taken between `load`'s first reads and the dictionary check) can't be hit on demand, so the mapping is tested on `active_dict_present` directly.
