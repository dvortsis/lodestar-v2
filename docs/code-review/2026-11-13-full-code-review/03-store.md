# Store (`crates/store`) review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test code): `src/schema.rs` (including its tests), `src/lib.rs`, `src/sources.rs`, `src/compress.rs`, `src/filetree.rs`, `src/snapshots.rs` (1-1678), `src/import.rs` (1-1984), `src/filter.rs` (1-1007), `src/unverified.rs` (1-1630), `src/saved.rs`, `src/insights.rs`, `src/api_keys.rs`, `src/settings.rs`, `src/reporting.rs`, `src/floor.rs`, `src/subscriptions.rs`, `src/hot.rs`, `src/user_items.rs`, `src/user_data.rs`; `src/claims.rs` decode/encode path (918-1467); `src/insights_days.rs` 1-520 (compaction and reads); `src/rules.rs` 150-879; `src/swarm.rs` 60-590; `src/collections.rs` 40-270; `src/history.rs` (ring and decode).
- Skimmed: `src/claims.rs` 1-918 (model, limits, `check`); `src/insights_days.rs` 520-952 (`repair_rollups`); `src/memstats.rs` (two `unsafe` FFI reads, as the Cargo.toml comment says); test modules and `tests/*.rs` only by grep (what is covered, sleeps, ignored tests).
- Followed into other crates to confirm findings: `crates/pipeline/src/writer.rs` (commit and purge paths), `crates/search/src/{index,live,rebuild}.rs` (watermark handling), `crates/lodestar/src/update/journal.rs`, `crates/lodestar/src/update/windows/rollback.rs`, `crates/lodestar/src/cli/rollback.rs` (callers of the snapshot helpers), `crates/api/src/filters/subscriptions.rs`.
- Not read: the bulk of `tests/` and the in-file test modules (about 15k lines); `crates/lodestar/src/cli/{backup,vacuum,rollback,lock}.rs` (the backup/vacuum/restore commands live there, not in this crate; another area). No builds or tests were run. The `zstd` and `rusqlite` crate sources are not on this machine, so claims about their internals are marked.

## Findings

### STORE-01 Update rollback restores a pre-migration snapshot without the store's deletions and content-safety holds (only source purges are applied)
- Severity: blocking
- Kind: security (content safety) / data-loss guarantee
- Confidence: confirmed for the code path; likely for impact (not run)
- Where: `crates/store/src/snapshots.rs:1188-1254` (`apply_source_purges`; its doc says "That path renames a pre-migration snapshot over the store without `purge_for_restore`"), `crates/store/src/snapshots.rs:1352-1438` and `1623-1659` (`carry_holds`, `carry_lists`, `purge_for_restore`: what `lodestar rollback` does), `crates/lodestar/src/update/journal.rs:408-435` (`choose_snapshot`: settle, `apply_source_purges`, quick_check, hash), `crates/lodestar/src/update/journal.rs:437-470` (`restore`: rename), `crates/lodestar/src/update/windows/rollback.rs:1093-1112` (`choose`: hashes the snapshot only, no settle and no scrub at all). `purge_for_restore` has exactly one caller, `crates/lodestar/src/cli/rollback.rs:397`.
- What: there are two ways a snapshot becomes the store. `lodestar rollback` stages a copy and runs `purge_for_restore`, which applies the whole deletion journal and every hold by hash, merges the live store's `cs_hashes`, `cs_sigs`, `tombstones`, `quarantine`, core switches and blocklist subscriptions into the copy, and merges the journal (`docs/decisions/2026-10-30-F-rollback-journal.md`, `2026-10-30-F-rollback-holds.md`: "A rollback never brings back a deleted torrent", "A rollback keeps the holds recorded after the snapshot", flagged to the owner as content safety). The update rollback (automatic after failed probation, Settings → Updates → Roll back, `lodestar rollback --update`, all through `update/journal.rs`) renames the `pre-v<N>` snapshot over the store after `apply_source_purges` only. Nothing carries holds or the journal, and deletions are applied only as far as the last hourly `purge_snapshots` pass got. On Windows the helper applies nothing, not even `settle` or the source scrub.
- Failure scenario: v0.6 is installed by the updater and migrates (snapshot `lodestar.pre-v15.db`). Days later the user reports a torrent as abuse material (`add_cs_hashes` origin 1 + `delete_torrents`), then uses Roll back (architecture §12.2: available "while its snapshot is kept", i.e. 90 days). The restored store has no `cs_hashes` row for that hash and no tombstone, so the crawler may fetch, store and index it again, and the floor no longer refuses it. If the report was made in the hour before the rollback, the hourly pass has not reached the snapshot and the torrent row itself comes back. The live journal rows are lost too, so older snapshots stop being scrubbed for those hashes. On Windows, a WAL left beside the snapshot by an interrupted purge is dropped by the rename, which undoes that purge.
- Suggested fix: make the update rollback go through the same preparation as `lodestar rollback` (run `purge_for_restore(live, snapshot, now)` in place on the snapshot before it is verified and hashed; it already ends as one file with no WAL), or expose one `snapshots::prepare_for_restore` that both paths must call. On Windows, do it in the unprivileged service before the helper takes over, or refuse the restore when it has not been done.
- Tracked?: not found. `docs/decisions/2026-11-02-SRC-14b-2.md:93-94` covers source purges on this path only ("No other path renames a snapshot over the store"); the F-rollback decisions are scoped to `lodestar rollback`. Open PR 795 (X-win-upd-manual-rollback) builds the Windows manual Roll back and would inherit this. Cross-area: the defect is in `crates/lodestar/src/update`, the missing hook is the store's.

### STORE-02 `Store::write` panics on a refused prepared torrent when the classification generation is stale, killing the writer task
- Severity: important
- Kind: correctness
- Confidence: confirmed (by reading; not run)
- Where: `crates/store/src/lib.rs:764-775` (`ins.id()` over `out.iter().chain(&prepared)`), `crates/store/src/lib.rs:499-508` (`Inserted::id` is an `expect` on `Refused`), `crates/store/src/lib.rs:1275-1292` (`commit_live` returns `Refused`), `crates/pipeline/src/writer.rs:724` (generation read before `judge().await`), `crates/pipeline/src/writer.rs:2477-2484` (`prepared` and `class_gen: Some(..)` in one `WriteBatch`), `crates/pipeline/src/writer.rs:852-853` (`.expect("writer transaction task")`)
- What: the stale-class branch of `Store::write` marks the batch's rows with `stmt.execute([ins.id()])` for every entry of `out` and `prepared`. `prepared` can hold `Inserted::Refused` (a fetch whose only finder is a disabled or purged source, SRC-14b), and `Inserted::id()` panics on it by design (`docs/decisions/2026-11-02-SRC-14b-2.md:46`: "It panics on `Refused`, and the writer reads `stored()`"). The adult-mark loop 15 lines above uses `filter_map(|i| i.stored())`; this loop was missed. The pipeline writer is the caller that passes both, and it unwraps the `spawn_blocking` join with `expect`, so the panic takes the writer task down (the transaction rolls back when the `Store` drops).
- Failure scenario: a source is disabled or purged while one of its keys is still being fetched (the result arrives in a later batch and is `Refused`), and in the same batch window a classification rule or correction is saved through the API (`bump_class_gen` on the API's own connection, `crates/api/src/filters.rs:3222`; `set_correction` at `crates/store/src/rules.rs:701`), so `rules::class_gen(&tx) != g`. `ins.id()` panics and ingest stops until restart. A narrow window (two user actions racing one batch), but nothing prevents it.
- Suggested fix: `for id in out.iter().chain(&prepared).filter_map(|i| i.stored()) { stmt.execute([id])?; }`, and a case in `tests/source_stop.rs` with a stale `class_gen` (that file never sets one).
- Tracked?: not found (`runner/slices.yaml`, `docs/decisions/*SRC-14b*`, open-PR list).

### STORE-03 After a power loss the store can fall behind the search index, and nothing detects it: ids are then reused under stale index documents
- Severity: important
- Kind: data-loss
- Confidence: likely (reasoned from SQLite's documented WAL + `synchronous=NORMAL` behaviour and the code; not reproduced)
- Where: `crates/store/src/schema.rs:936-937` (`journal_mode=WAL`, `synchronous=NORMAL`), `crates/store/src/sources.rs:95-99` (`id_seq` is a row in the same file), `crates/store/src/schema.rs:330-335` ("a deleted id is never reused: hot-column slots, index documents and the percolator watermark all key on it"), `crates/search/src/index.rs:2283-2286` (`catch_up_new` reads only `ids_after(watermark)`), `crates/search/src/rebuild.rs:619-650` and `crates/search/src/live.rs:196-240` (open paths: no comparison of the index watermark with the store)
- What: in WAL mode with `synchronous=NORMAL` SQLite does not fsync the WAL at commit, only around checkpoints, so an OS crash or power cut can roll back the most recent committed transactions (consistent, not durable). The Tantivy index commits durably on its own schedule and keeps its watermark in its own commit payload. After such a loss the index watermark W can be above the store's highest id M, and `id_seq.next` and `sqlite_sequence` have rolled back with the store. Nothing at start compares the two. (`lodestar rollback` and the backup instructions move the index aside by hand, `crates/lodestar/src/cli/rollback.rs:350`, `cli/backup.rs:180`; an unclean power-off has no such step.)
- Failure scenario: power cut seconds after a burst of commits that the indexer already committed. On restart new torrents take ids M+1.. again. (a) The indexer never indexes them (ids at or below its watermark): stored but unsearchable until a manual `lodestar reindex`. (b) Search hits for the stale documents resolve by id to the rows of different torrents (name from the index, hash and detail from an unrelated row). (c) Saved-search alerts and hot-column slots keyed by id inherit the mix-up. Silent.
- Suggested fix: at start, before the writer or indexer runs, compare the store's issued-id high-water mark (`id_seq.next - 1`) and `reindex_head()` with the index watermarks. If the index is ahead, raise `id_seq.next` past the index watermark and delete index documents above the store's max id, or discard and rebuild the index. Alternatively make id allocation durable (commit ingest batches with `synchronous=FULL`, or reserve id blocks with a synced write).
- Tracked?: not found (`docs/decisions` has nothing on `synchronous` or power loss for the store; `docs/architecture.md:182` only lists the pragmas).

### STORE-04 No production code trains or applies the file-tree dictionary the architecture and its size budget assume
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/store/src/lib.rs:1130-1215` (`train_file_dict`, `recompress_files`); callers: only `crates/store/tests/store.rs:284-306`; `docs/architecture.md:187` ("zstd with a trained dictionary"), `docs/architecture.md:1860` (344 B per blob with "zstd 19 + a 112 KiB trained dictionary")
- What: `Store::train_file_dict` and `Store::recompress_files` have no caller outside the store's own tests. `settings.file_dict` is therefore never set on a real store, every blob is written with `dict_id NULL` (or raw), and the remote-worker dictionary shipping (`crates/lodestar/src/cluster/filter.rs:137`, `serve.rs:1309`) always ships "no dictionary". The bytes-per-torrent budget was measured with the dictionary.
- Failure scenario: a store at the planned 10M+ scale carries file-tree blobs at the undictionaried size (the test at `tests/store.rs:288` asserts the dictionary more than halves them), so disk use and the mmap working set are above the documented budget, with no switch to fix it.
- Suggested fix: schedule training (once enough torrents exist, then `recompress_files` in small background batches), or record the decision not to and re-measure the budget. When wiring it: other open connections load `zstd_dicts` only at open (`Store::load`), so a reader opened before training fails `files()` with "zstd dictionary N is missing" until it is reopened.
- Tracked?: not found in `runner/slices.yaml` or `docs/build-plan.md` (grep "dict", "train").

### STORE-05 A migration that fails after its snapshot leaves one more full-size snapshot on every retry
- Severity: polish
- Kind: resource
- Confidence: likely
- Where: `crates/store/src/schema.rs:1059-1063` (snapshot before the migration loop), `crates/store/src/schema.rs:1173-1182` (`snapshot_path` picks the next free `.k`), test `existing_snapshot_does_not_block_migration` (1860) pins it
- What: every start that finds a pending migration runs `VACUUM INTO` a new `pre-v<N>[.k].db` first, then migrates. If the migration fails deterministically (bad data for a backfill, a constraint, a busy lock), each restart adds another identical full copy; they expire only after 90 days. The self-updater's probation caps this at three attempts, but a Docker image update under `restart: unless-stopped` does not.
- Failure scenario: 40 GB store, migration fails on a row; the container restart loop writes 40 GB per attempt until the volume is full, at which point the live store cannot write either.
- Suggested fix: reuse the newest existing `pre-v<N>` snapshot when the store has not been written since it was taken (same `schema_migrations` max and the snapshot is newer than the store's last change), or cap repeats and fail with a clear message.
- Tracked?: not found.

### STORE-06 `PRAGMA journal_mode=WAL` is set but its result is never checked
- Severity: polish
- Kind: correctness
- Confidence: likely (rusqlite's `pragma_update` discards the returned row; crate source not available here to re-read)
- Where: `crates/store/src/schema.rs:936` (compare `crates/store/src/snapshots.rs:1087`, which uses `pragma_update_and_check`)
- What: SQLite answers `journal_mode=WAL` with the mode actually in effect and silently stays in rollback-journal mode where shared memory is unavailable (some network and FUSE mounts). The store then runs with readers and the writer blocking each other (the design assumes "WAL lets readers run alongside it", `lib.rs:594-596`), and the WAL-based scrub reasoning no longer describes what is on disk.
- Failure scenario: data folder on a share without shared-memory support: API reads hit the 5 s busy timeout during ingest and return errors, with nothing in the log saying why.
- Suggested fix: read the mode back and fail the open (or log a clear warning) when it is not `wal`.
- Tracked?: partly: `runner/slices.yaml` E-M6 notes "FUSE/netfs detection" as a gap to audit.

### STORE-07 Review-queue and tombstone pages sort the whole table on every page
- Severity: polish
- Kind: resource
- Confidence: confirmed (schema has no index on `at`; plan not run)
- Where: `crates/store/src/filter.rs:430-464` (`quarantined_before`), `crates/store/src/filter.rs:757-785` (`tombstones_before`), `crates/store/src/schema.rs:515-529` and `845-846` (indexes: `quarantine_expiry (expires_at)`, `quarantine_rule`, `tombstones_rule` only)
- What: both queries filter and order by `(at DESC, hash)` with a `?1 IS NULL OR ...` predicate; neither table has an index on `at`, so each page is a full scan plus a top-N sort. Tombstones are never expired, so the table only grows. Also `held_by_rule`'s comment (`filter.rs:573`: "A scan, as `rule` has no index") is stale since V10 added the rule indexes.
- Failure scenario: a store with a few million tombstones: each page of the tombstone list or pack preview (`crates/api/src/filters/packs.rs:517`) scans them all on a reader.
- Suggested fix: an index on `(at, hash)` for both tables in the next migration slot, and split the `IS NULL` case into its own statement so the planner can use it.
- Tracked?: not found.

### STORE-08 A verified hybrid can make `upgrade_partial` hit the `torrents_v2` unique index and fail the whole batch
- Severity: polish
- Kind: correctness
- Confidence: suspected (needs two imported rows for one hybrid; not reproduced)
- Where: `crates/store/src/lib.rs:1439-1456` (`info_hash_v2 = coalesce(info_hash_v2, ?9)`), `crates/store/src/lib.rs:1318` (`find_any` returns the first match only), `crates/store/src/schema.rs:377-378` (unique index on `substr(info_hash_v2, 1, 20)`)
- What: if the store holds row A (partial import, v1 hash H1, no v2 hash) and row B (stored under the truncated v2 hash with `info_hash_v2 = V2`), a fetched hybrid (H1, V2) matches A first, and the upgrade's `UPDATE` sets A's `info_hash_v2 = V2`, which collides with B on `torrents_v2`. The constraint error fails `Store::write`, so every torrent in that batch is lost, and it repeats whenever the hybrid is fetched again. What I could not confirm: whether any importer writes a v2-only row for a torrent another dataset lists by v1 hash.
- Suggested fix: in `commit_one`, look up every swarm hash; when more than one row matches, merge or skip the v2 hash instead of updating into a conflict.
- Tracked?: not found.

### STORE-09 Per-batch `count(*)` scans on the import and usenet write paths
- Severity: polish
- Kind: resource
- Confidence: confirmed (by reading)
- Where: `crates/store/src/import.rs:476-483` (`JOB_SELECT` counts the job's candidates), loaded twice per batch at `import.rs:933` and `1044`; `crates/store/src/unverified.rs:1403-1417` (`cap_network`: `count(*)` over the tier, then `ORDER BY id LIMIT` with no `(network, id)` index) on every batch that adds a usenet item
- What: each import batch commit counts all of the job's waiting candidates twice inside the write transaction, and each usenet batch counts the whole tier (up to 1,000,000 rows) and, once at the cap, sorts it to find the oldest. Cost grows with what is already stored, inside the writer lock.
- Failure scenario: a 50M-record import with a large candidate share spends a growing part of every batch transaction counting index entries; other writers wait behind it.
- Suggested fix: read `open_candidates` only where it is used (job views, `set_import_phase`), and keep the usenet count in a small counter row or use an `(network, id)` index.
- Tracked?: not found.

### STORE-10 `writes_wait_for_another_writer` relies on a fixed 300 ms sleep and can pass without testing anything
- Severity: polish
- Kind: test
- Confidence: confirmed
- Where: `crates/store/src/user_data.rs:117-134`
- What: the holder thread sleeps 300 ms, then commits. If the main thread reaches its write after the holder has committed (a slow runner), the test passes without ever waiting on a lock. It cannot fail from slowness, so it is not the blocking kind of sleep in `runner/standards.md` §2, but it is a fixed sleep and a test that can pass vacuously.
- Suggested fix: have the holder release on a channel signal sent after the main thread has started its write (or assert the write took at least the hold time).
- Tracked?: not found (PR 778 fixes other test flakes).

## Checked and found sound
- Migrations (`schema.rs:1030-1080`): each runs in its own transaction with its `schema_migrations` row; a failed one rolls back; versions are contiguous (test at 1255) and the list is V1..V14 with V14 = SRC-1, matching `docs/build-plan.md` §0.7's queue (V15 reserved for CR-3). A newer store is refused before anything is written, for writers and readers (`check_current`, test at 1611). A crash between migrations of a multi-step upgrade leaves a recorded version and a snapshot per attempt.
- `V12_REPAIR` (`schema.rs:312-328`, `959-974`) is the one out-of-band schema change: applied by shape, in a transaction, documented as a pre-release fix. It is the only departure from "append-only, never edited".
- Pre-migration snapshot (`schema.rs:1092-1182`): `VACUUM INTO` a `.tmp`, fsync, rename, fsync of the directory on Unix; stale `.tmp` files removed; an existing snapshot is never overwritten; a snapshot that cannot be written stops the migration.
- Snapshot purge (`snapshots.rs`): plan per schema version with a test tying `Plan::KNOWN_THROUGH` to the latest migration (2305); look-first so an unchanged snapshot is not written; WAL + secure_delete + TRUNCATE so no pre-images; journal rows acknowledged only when every snapshot was scrubbed, by `(hash, at)` so rows moved forward meanwhile stay; margins for clock steps and other connections. `LIKE` is used once with a constant prefix that has no wildcard characters (706-708).
- SQL injection: every `format!`-built statement interpolates compile-time constants only (table names, column lists, rule-id constants, placeholder lists). All values are bound. Prefix matches on plugin ids and setting keys use `substr(key, 1, n) = ?` with a character count, not `LIKE`.
- File-tree blob (`filetree.rs:200-386`): counts bounded by remaining bytes, parent indexes validated before use, no recursion, unknown flags and trailing bytes refused; `shape` pre-check and the capped streaming decompress (`compress.rs:179-214`) for worker blobs. Property tests and a fuzz target exist.
- Claims blob (`claims.rs:941-978`, `1128-1467`): version and codec checked, zstd output capped at `MAX_PLAIN`, lengths checked before allocation, minimal varints, section/version agreement, trailing bytes refused; the schema `CHECK` caps match (test at `schema.rs:1817`). Unreadable claims are reported as such, not a panic (`unverified.rs:1333-1340`).
- `swarm_trackers` and `swarm_history` blobs (`swarm.rs:158-217`, `history.rs:149-167`): strict decoders; a corrupt history ring is replaced rather than failing the batch.
- Hot-column snapshot files (`hot.rs:319-383`): temp file, fsync, rename per column; stamp, length, file size and FNV checksum checked before use; any mismatch falls back to a rebuild from SQLite.
- Import batch commit (`import.rs:918-1053`): one `BEGIN IMMEDIATE` transaction for records, counters and cursor; replay by `seq`; job state and the batch's source checked inside it; fail-point test at every step. Holds are re-checked per record inside the transaction.
- Writer serialisation: read-then-write paths use `begin_write` (`BEGIN IMMEDIATE`); the remaining deferred `conn.transaction()` uses (`filter.rs` releases, `settings.rs`, `rules.rs` replace, `sources.rs` queue save) all start with a write, so they wait on the busy timeout instead of failing the upgrade.
- Integer handling: sizes go through `to_i64`/`get_u64` and metainfo caps totals at `i64::MAX`; source bitsets round-trip through `i64` consistently, including bit 63 (`CHECK ((sources >> source_id) & 1 = 1)` holds for it).
- API keys (`api_keys.rs`): only a SHA-256 of a 256-bit random secret and a public prefix are stored; lookup by prefix then a constant-time compare; revoked/expired reported only after the hash matched. Source credentials are stored sealed, keyed under the plugin id with `/` refused in ids (`sources.rs:230-234`).
- `delete`-then-scrub: every path that deletes names or torrents calls `scrub_wal` (PASSIVE then a 100 ms-bounded TRUNCATE) and the hourly purge retries.
- Promotion (`unverified.rs:799-939`): the id-sequence swap is inside one transaction and checked (`got != id` is an error); held hashes delete the item and journal it.

## Area-level concerns
- The hourly `purge_snapshots` runs on the pipeline writer's own store while the writer waits (`crates/pipeline/src/writer.rs:619-660`). A full pass (after each start and once a day) loads every tombstone and quarantine row recorded since the snapshot into memory (`Live::read`, `snapshots.rs:572-601`, 28+ bytes each) and runs up to four point queries per hash per snapshot. `docs/decisions/2026-10-23-S-snapshots.md` measured about 1 s at 200k tombstones; at several million (a drop-heavy rule set over 90 days) that is tens of seconds of stalled ingest and a few hundred MB transient, per snapshot. Worth re-measuring at the 10M gate.
- The store's content-safety checks by hash (`held`, `store_hold`, the V13/V14 triggers) cover `tombstones`, `quarantine` and `cs_hashes` only. Hashes that are on the floor through a floor-marked subscription list live in `subscription_data` blobs and are enforced by the filter engine alone; the store cannot back them up. That matches the module docs, but it means "the floor's hash list" in `import.rs`'s header is narrower than it reads.
- Unbounded growth with no store-side cap: clearnet unverified items (only the usenet tier is capped), `tombstones` (never expired), `events`, `reindex_log` ("rows are few" predates enrichment and refile writing one row per change). None is wrong today; each is a table someone will have to bound.
- `rollup_sums` and `rollup_day_sums` walk one recursive-CTE step per hour or day in `from..to`; the store does not bound the range, so it relies on every API caller clamping it (not traced here).
- `save_fetch_queue` rewrites the whole `fetch_queue` table in one transaction on each periodic save; its cost and WAL churn follow the queue length (bounded by the pipeline, not checked here).
- Several write connections exist in one process (`crates/lodestar/src/services.rs:115-216`), all with a 5 s busy timeout. Any single transaction longer than that on one of them (a large purge step, a big import batch, a TRUNCATE checkpoint over a large WAL) turns into `SQLITE_BUSY` errors on the others; the store has no retry, so the outcome depends on each caller.
- `compress.rs:168` passes `MAX_DECOMPRESSED` (1 GiB) as the capacity argument of `zstd::bulk::Decompressor::decompress` on every `Store::files()` read. Whether that reserves 1 GiB per call depends on the zstd crate version's `upper_bound` handling, which I could not read here; the dictionary test passing on the Windows CI runner suggests it does not. `claims.rs:969-973` sizes from the frame header instead, which would be the safe pattern for both.
- Test coverage is broad (property tests for history and file trees, fuzz targets for claims, file trees and worker blobs, crash fail points for imports, upgrade tests per migration). Gaps noticed: no test combines a refused prepared torrent with a stale `class_gen` (STORE-02); nothing exercises the update-rollback path against holds or deletions (STORE-01); no test for the store being behind the index (STORE-03).
