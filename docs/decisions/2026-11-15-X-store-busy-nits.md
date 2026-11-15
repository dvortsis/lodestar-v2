# X-store-busy-nits decisions (#937)

The optional notes from two reviews, folded into one slice by the runner: #919's (X-store-migrate-race, logged as X-store-migrate-nits) and #921's (X-import-busy-reuse). Nothing a user sees changes.

## The migration's lock window includes the snapshot
- **Adds to `2026-11-15-X-store-migrate-race.md`:** the one `BEGIN IMMEDIATE` that covers a migration also covers the pre-migration snapshot (`VACUUM INTO`, read through a second connection while the lock is held).
  - So on a large store, a long upgrade holds the write lock for the snapshot as well as the migrations. Another connection opening the store meanwhile waits its busy timeout (5 s), then gets the busy error.
  - That error is safe to retry: nothing was written. The cluster's start retries it (`2026-11-15-X-cluster-start-retry.md`), and the import commands say the store is in use (`cli/import.rs`).

## The busy test runs as the store runs
- **`a_migration_that_cant_get_the_lock_says_busy`** now sets both connections up with `configure`, as `Store::open` does, so the test runs in WAL, which it checks. Before, it ran in SQLite's default rollback-journal mode.
- **It also asserts `Error::is_busy()`** on the result. Everything it checked before stays: SQLite's busy code past the timeout, no snapshot, nothing migrated, and a migration once the lock is free.
- **Its busy timeout stays at 50 ms,** set after `configure`, so the test doesn't wait the store's 5 s.

## The floor writer's check runs off the async threads
- **The cluster floor writer's hourly expiry** checked `db.exists()` on the async task before handing the expiry to a blocking thread (`2026-11-15-X-store-migrate-race.md`).
  - That check is file I/O, so it now runs inside the same `spawn_blocking`, around the work it guards.
  - The behaviour is the same: no database file means no expiry and nothing created. `off_binds_and_creates_nothing_until_allowed` still checks that, after the writer's first expiry.

## Import reuses `Error::is_busy`
- **`cli/import.rs`'s `store_busy`** matched `Error::Sqlite` with SQLite's busy or locked code, which is exactly what `lodestar_store::Error::is_busy` matches. The three callers now use `is_busy`, and `store_busy` is gone. Users see the same messages.
- **`sqlite_busy` stays.** It tests a bare `rusqlite::Error`, from a call that doesn't go through the store's error type.
