# X-store-wal-nits decisions (#931)

The optional findings from the runner's review of #928 (X-flake-store-migrate-race), in `crates/store/src/schema.rs` (architecture §12, the store).

**Supersedes** the root-cause wording of `2026-11-15-X-flake-store-migrate-race.md`:
- **"while another was switching it to WAL, or migrating it under `BEGIN IMMEDIATE`":** the migration case was wrong. By the time a migration holds its lock, the file's header already says WAL, so the switch has nothing to do.
- **The case that fails** is a connection opening a new store while another holds the write lock before the file is in WAL. That's either the other connection's own WAL switch, or its `auto_vacuum` setup on a new file.
- `use_wal`'s doc says the same now. The cause is unchanged: SQLite's busy retry runs only for a transaction started from nothing, so the read-to-write upgrade got `SQLITE_BUSY` at once.

## WAL must take
- **SQLite keeps the old journal mode without an error** where WAL can't work, for example on a filesystem without shared memory. The store then ran in rollback-journal mode, without the readers WAL lets run beside the writer.
- **Now the switch's reported mode is checked,** as `snapshots.rs` already does. Anything but `wal` is refused with `Error::Invalid`, which says the data folder can't use WAL and needs a local disk, not a network share.
- **A store in memory reports `memory` and opens,** since WAL doesn't apply there.
- **This is narrower than the FUSE warning (architecture §11, Unraid):**
  - A data folder on FUSE or a network filesystem still only gets a warning at startup. Only a folder where SQLite actually can't use WAL is refused.
  - Running there without WAL would break what the store assumes: readers beside the writer, and `open_reader`.
  - The runner's review asked for it to fail loudly. The runner flags it with this PR.
- **`a_folder_that_cant_use_wal_is_refused`** opens a file through SQLite's `unix-none` VFS, which keeps `delete`, and expects the refusal; that part runs on Unix only. It also checks that a store in memory still opens.

## The busy-timeout test's margin
- **`switching_to_wal_honours_the_busy_timeout`** now requires at least half the timeout, not all of it. Windows can wake a sleep slightly early, and half of 200 ms is still far from the bare pragma's microseconds.
