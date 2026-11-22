# X-flake-store-migrate-race decisions (#928)

`schema::tests::a_new_store_opened_at_once_is_created_once` (from #919) failed on #926's Ubuntu run with `DatabaseBusy`, "database is locked". It was a product bug, not a test race: one of eight real `Store::open` calls on a new file failed (architecture §12, the store). This is the runner's call, as a flake that fails other PRs.

## Root cause

`configure` switched every connection to WAL with `PRAGMA journal_mode=WAL`. That statement reads the file's header in a read transaction, then needs the exclusive lock to change the mode: a read upgraded to a write. SQLite refuses such an upgrade at once with `SQLITE_BUSY` rather than call the busy handler, since waiting there could deadlock, so the 5 s busy timeout set just before it never applied. A connection opening a new store while another was switching it to WAL, or migrating it under `BEGIN IMMEDIATE`, therefore failed straight away. The test catches it in about 1 run in 17: 12 failures in 200 runs of the two concurrent-open tests (two loops of 100 in parallel) before the fix, and 0 in 200 after.

## The fix

- **`use_wal` leaves a file that's already in WAL alone.** The mode lives in the file, so every open after the first has nothing to do and takes no lock for it.
- **When the switch is refused as busy,** it waits for the lock the way a write transaction from scratch does: `BEGIN IMMEDIATE; ROLLBACK`, which goes through the busy timeout. Then it looks at the mode again. That's at most three times, and it takes no fixed sleeps of its own.
- **A lock still held when the timeout runs out** surfaces as the busy error, as before (`Error::is_busy`). Nothing is retried past it, and nothing is hidden from the caller.

## Tests

- **`switching_to_wal_honours_the_busy_timeout`:**
  - while another connection holds the lock, the switch waits at least its busy timeout, then reports busy;
  - once the lock is free it switches;
  - in WAL, a writer elsewhere doesn't hold up a new connection's switch.
  - Run against the bare pragma, it fails after 73 µs.
- **Unchanged:** the two concurrent-open tests and `a_migration_that_cant_get_the_lock_says_busy`, which still checks that a migration past its timeout says busy and writes nothing.
