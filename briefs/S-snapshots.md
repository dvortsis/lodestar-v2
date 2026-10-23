# S-snapshots: Migration snapshots follow purge and expire after 90 days
**Model:** Opus (on-disk data, privacy, deletion). **Branch:** `slice/S-snapshots`.

**Why (owner-delegated decision, 2026-10-23; deciding factors UX and performance):** `<db>.pre-vN.db` snapshots are written by `VACUUM INTO` before each migration (`crates/store/src/schema.rs`) and kept forever. They keep quarantine names, deleted torrents and user text that the live store later purges or overwrites, defeating owner decisions §2.17 (deleted data is overwritten) and the 30-day quarantine purge (§2.16, §20). Decision: **run the same expiry purge on snapshots, and delete snapshots after 90 days** (the rollback window).

**Spec:**
- `runner/standards.md`; `docs/owner-decisions.md` §2.16, §2.17; architecture §7 (migrations, snapshots), §20 (quarantine purge, tombstones), §12.2 (rollback); `docs/decisions/2026-10-22-runner-secure-delete.md`; `docs/decisions/2026-10-22-M6-cli.md` (rollback lists snapshots).
- The code: `crates/store/src/{schema.rs,filter.rs,lib.rs}`, `crates/pipeline/src/writer.rs` (the hourly purge, `PURGE_EVERY`), `crates/lodestar/src/cli/rollback.rs`.

## Own
- `crates/store/src/snapshots.rs` (new) and minimal hooks in `schema.rs`/`lib.rs`; the purge call site in `crates/pipeline/src/writer.rs` (one line, named in the PR); `crates/lodestar/src/cli/rollback.rs` only for listing expiry info; tests; `docs/decisions/<date>-S-snapshots.md`; the user-facing note in `docs/operations.md` or the rollback README text if one exists.
- **Don't edit** the migration list, the filter crate or `crates/api`.

## Build
1. **Purge in snapshots:** when the live store purges expired quarantine names (and on the same hourly tick), apply the equivalent purge to each snapshot: open it read-write, check its `schema_migrations` version, and run a purge that matches **that version's** schema (quarantine/tombstone tables may differ or be absent in old versions: handle each known version, skip unknown ones with a logged warning). Use `secure_delete` and a TRUNCATE checkpoint so nothing survives in free pages or WAL. Deleted torrents and user data: if the live store deleted a torrent since the snapshot, delete it from the snapshot too only if that's tractable by hash; otherwise rely on the 90-day expiry and say so.
2. **Expiry:** delete snapshots older than 90 days (by the snapshot's own creation time recorded at write time, e.g. in a sidecar or the file's mtime set at creation; justify the choice) on the same tick, overwrite-then-unlink isn't meaningful on modern filesystems, so plain unlink plus a note. Never delete the snapshot a running rollback is using (M6-cli's lock).
3. **Rollback:** `lodestar rollback` lists each snapshot's age and when it expires; `stats` shows the count and total size.
4. **Settings:** the 90-day window is a fixed default now; if B2's settings registry makes adding a field trivial, add `snapshots.retain_days` (min 7, max 365, default 90) and say so.

## Tests
- A snapshot taken before a quarantine entry expires loses the sealed name after the live purge runs; the marker is absent from the snapshot file (search the bytes, as #81's tests do).
- Old-version snapshot fixtures (at least two schema versions) purge correctly or are skipped with a warning, never corrupted (quick_check passes after).
- Expiry deletes only snapshots past the window; a snapshot in use by rollback is never deleted.
- Deterministic clock.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass after merging `origin/main`.
- A PR titled `S-snapshots: Purge and expire migration snapshots`, with the per-version purge table, the expiry rule, and the user-visible changes.
