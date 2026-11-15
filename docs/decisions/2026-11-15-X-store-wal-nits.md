# X-store-wal-nits decisions (#931)

The optional findings from the runner's review of #928 (X-flake-store-migrate-race), in `crates/store/src/schema.rs` (architecture §12, the store).

**Supersedes** the root-cause wording of `2026-11-15-X-flake-store-migrate-race.md`:
- **"while another was switching it to WAL, or migrating it under `BEGIN IMMEDIATE`":** the migration case was wrong. By the time a migration holds its lock, the file's header already says WAL, so the switch has nothing to do.
- **The case that fails** is a connection opening a new store while another holds the write lock before the file is in WAL. That's either the other connection's own WAL switch, or its `auto_vacuum` setup on a new file.
- `use_wal`'s doc says the same now. The cause is unchanged: SQLite's busy retry runs only for a transaction started from nothing, so the read-to-write upgrade got `SQLITE_BUSY` at once.

## WAL must take
- **SQLite keeps the old journal mode without an error** where WAL can't work, for example on a filesystem without shared memory. The store then ran in rollback-journal mode, without the readers WAL lets run beside the writer.
- **Now the switch's reported mode is checked,** as `snapshots.rs` already does. Anything but `wal` is refused with a new `Error::WalUnavailable`. Its text is the fix: "the data folder can't use SQLite's WAL journal: keep it on a local disk, not a network share or a FUSE mount such as Unraid's /mnt/user".
- **Accepted by the runner** as a safety and UX call (`docs/owner-decisions.md` §2.26 and §2.33): without WAL the readers block on the writer, so failing loudly beats running degraded. The runner flags it to the owner under §2.19.
- **A store in memory reports `memory` and opens,** since WAL doesn't apply there.
- **This is narrower than the FUSE warning (architecture §11, Unraid):**
  - A data folder on FUSE or a network filesystem still only gets a warning at startup. Only a folder where SQLite actually can't use WAL is refused.
  - Running there without WAL would break what the store assumes: readers beside the writer, and `open_reader`.
  - The runner's review asked for it to fail loudly. The runner flags it with this PR.
- **`a_folder_that_cant_use_wal_is_refused`** opens a file through SQLite's `unix-none` VFS, which keeps `delete`, and expects the refusal; that part runs on Unix only. It also checks that a store in memory still opens.
- **Its counterpart on every platform, `an_ordinary_folder_passes_the_wal_check`:** a store in an ordinary local folder passes and is in WAL, so the check never refuses a normal install. Windows has no SQLite VFS that keeps another mode, so the refusal itself can't be forced there. The acceptance side is what's checked on every target (runner's standing rule for platform-gated tests).

## What the user sees
- **The console and `docker logs`:** `serve: the data folder can't use SQLite's WAL journal: keep it on a local disk, …`. `command_failed` prints the error's own text, and this one names no path. Before, `Error::Invalid` would have prefixed it with "invalid:".
- **The log file and the Windows service:** `app.command_failed` and `service.failed` carry a fixed kind, never the message, because a message can name a path. That kind is now `store_wal_unavailable` for this refusal (`failure_kind`, which walks the error's sources), not plain `failed` or `serve`. So a Windows service user, who has no console, can see why it stopped.
- **The user guide:** the data-folder warning in section 2 (`02-install-lodestar-on-unraid-and-docker.md`) adds one sentence: "**(Planned)** Lodestar refuses to start if SQLite can't use its WAL journal there."

## The busy-timeout test's margin
- **`switching_to_wal_honours_the_busy_timeout`** now requires at least half the timeout, not all of it. Windows can wake a sleep slightly early, and half of 200 ms is still far from the bare pragma's microseconds.
