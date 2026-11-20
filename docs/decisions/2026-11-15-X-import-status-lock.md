# X-import-status-lock decisions

A UX fix from the import-host test run (runner, 2026-11-15): `lodestar import status` failed with a bare "database is locked" while an import was starting, though its help said it only reads.

## Why it failed

- **An import holds the store to itself.** `start` and `resume` (and `pause`, `cancel`, `remove`) open the store with `locking_mode = EXCLUSIVE` and take the write lock, so no server or second writer can run beside them (`open_exclusive` in `crates/lodestar/src/cli/import.rs`). In WAL mode that exclusive lock also keeps readers out. That's by design and unchanged: the write lock isn't weakened, and two writers still can't run.
- **`status` already reads on a read-only WAL connection** (`Store::open_reader`), which doesn't wait on the running server's writes. Only a command holding the store exclusively blocks it.

## What changed

- **`status` and `inspect` wait, then say what to do.** The store's busy timeout (5 s) bounds the wait on a held store. Past it, they refuse with a sentence instead of the SQLite error: another lodestar command has the store to itself; an import reports what it did when it stops, and Ctrl-C where it runs pauses it at the last saved batch; run the same command again once it has paused or finished. Nothing was read and nothing changed.
- **Every store read in `status` and `inspect` maps the lock** (`read_err`): opening the store, reading the jobs, and `inspect`'s job lookup. `inspect` reads the torrent count once, mapped, and hands it to its size estimate (`space_with`), whose fallback would otherwise hide a held store.
- **The help text now says it.** `status` and `inspect`: "only reads; safe while the server runs; while an import runs it can't read the store, and says so". `docs/operations.md` says the same in the import section.
- **Not done:** showing an import's progress from another process while it runs. That needs the import to publish its progress outside the store (a side file, say). It's a possible follow-up, not part of this fix.

## Help claims checked

| Command | Claim | Verdict |
|---|---|---|
| `import status` | safe while the server runs | True (read-only WAL connection); it now also says what to do while an import holds the store. |
| `import inspect` | only reads; safe while the server runs | True; the same message added for a held store. |
| `backup` (`backup.rs`, `cli/mod.rs`) | a consistent copy, safe while the server runs | True: a read-only connection and `VACUUM INTO`. Covered by `backup_while_the_store_is_in_use` in `crates/lodestar/tests/cli.rs`. Unchanged. |
| `stats` (`stats.rs`, `cli/mod.rs`) | safe while the server runs | True: a read-only connection. Covered by `stats_read_while_running`. Unchanged. |

`backup` and `stats` make no claim about running beside an import, and their own busy timeout (30 s) bounds their wait, so they're unchanged.

## Known, unchanged

- **A reader at the moment an import starts makes that start refuse** (`resume_refuses_another_source_and_any_reader_blocks_writes`): the exclusive lock can't be taken while any connection is open. The refusal says to stop the other process and try again, which is safe.
- **The new test waits out the busy timeout** once per locked call, about 25 s in all. It's bounded, with no fixed sleep.
- **Logged by the runner, not changed here:** in the store crate, `Store::load` reads `codec_has(..).ok() != Some(true)`, so a busy error in a narrow race reads as `Corrupt`. Existing code; a follow-up.
