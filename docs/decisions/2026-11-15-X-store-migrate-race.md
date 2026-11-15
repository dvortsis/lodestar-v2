# X-store-migrate-race decisions (#919)

Found while root-causing `cluster::serve::tests::off_binds_and_creates_nothing_until_allowed`, which failed about one run in three at `--test-threads=32`. It was not starvation and not a lost wakeup. The cluster's start failed once with `sqlite: table <name> already exists`, and nothing retried it (architecture §12, the store; §20, the cluster port).

## Migrations run under one write lock
- **The race:** `migrate` read the schema version outside any transaction, then ran each migration in its own deferred transaction. Two connections opening the same store at once both read version N, and both applied N+1. The second failed on a table the first had just created.
  - On a store being upgraded, both also took a snapshot through the same `.tmp` name, and one failed with a disk I/O error.
  - Any two first opens could hit it, and so could any two opens across an upgrade. It wasn't specific to tests.
- **The fix:** `migrate` takes one `BEGIN IMMEDIATE` transaction before it reads the version. Under that lock it creates `schema_migrations` if needed, reads the version, decides on the snapshot, applies every pending migration, then commits.
  - A connection that arrives second waits for the lock with the store's busy timeout (5 s), then finds the schema current and writes nothing.
- **The snapshot** is read through a second, read-only connection while the lock is held, because `VACUUM INTO` can't run inside a transaction. Nothing has been written yet and nobody else can write, so it's the pre-migration state, as before (§12.2).
- **The pending migrations commit together,** not one transaction each. A failure part-way through now leaves the store at the version it started from, with its snapshot, instead of at some version in between.
  - `packs.fresh` is still written with the creating migration, in the same transaction.
  - An upgrade's WAL holds every pending migration until the commit. Upgrades span a version or two, so that's acceptable.
- **The busy timeout is set first** in `configure`, so switching a new file to WAL also waits for a connection opening it at the same moment.
- **A connection that waits past its busy timeout** gets SQLite's busy error (`Error::Sqlite`, `DatabaseBusy`). It doesn't get a corrupt-store error or a failed migration, and nothing is written. `a_migration_that_cant_get_the_lock_says_busy` covers this.
- **Tests:**
  - Eight threads open one new store at once, twelve times over. Eight open a V13 store at once, six times over.
  - Each run checks that every open succeeds, every migration ran once, and the schema matches a single open's.
  - An upgrade must leave exactly one snapshot, and a new store must be marked fresh.
  - All of these failed before the fix.

## The floor writer doesn't create the database
- **Before,** the cluster's floor writer expired old reports at start, then hourly. Each expiry opened the store, so it created `lodestar.db` even with workers off.
  - The test's "not even the database is touched" held only because the test checked before its first await.
  - Its open is the one that raced the cluster's own first open.
- **Now** an expiry skips while there's no database file. With no file, no server has opened and no report was written, so there's nothing to expire.
  - The test waits for the writer's first expiry (a test-only counter), then checks that the database still doesn't exist. The check fails without the skip.

## Not in this slice
- **A start that fails isn't retried** while workers stay allowed. Only toggling them, or a reseal, opens the port again. That's X-cluster-start-retry: bounded backoff and the failure shown in status (runner, 2026-11-15).
- **`Store::load` and `open_reader`** are untouched; X-store-load-busy-map is working in that area.
