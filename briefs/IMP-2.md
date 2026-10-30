# IMP-2: Magnetico SQLite adapter (read-only, keyset, fuzzed)
**Model:** Opus. It parses untrusted input. Lane I of the master plan (`docs/decisions/2026-10-30-master-plan.md`, "Imports"). This is the plan's IMP-03 (`docs/plan/2026-10-29-magnetico-bitmagnet-migration.md` §5 "Magnetico SQLite", §9, §10). **`/security-review` before the PR.**

## Base and timing
- Launch after IMP-1 (#210) merges. Use its batch API and contracts (`docs/decisions/2026-10-30-IMP-1.md`); don't redefine them.
- Branch `slice/IMP-2` from `origin/main`. Target ≲ 1,500 changed lines; split the fuzz target and fixtures into a second PR if needed.

## Do
1. **Contract note first.** Add a short `docs/decisions/<date>-IMP-2.md` and send it to the runner for an OK before the reader code. It maps each Magnetico column to the IMP-1 record:
   - `info_hash`: a 20-byte BLOB, otherwise invalid;
   - `name`: raw bytes, decoded with a lossy flag;
   - `total_size`;
   - `discovered_on`: Unix seconds, range-checked;
   - `n_seeders`/`n_leechers`: hints only, never swarm truth;
   - `updated_on`, and `modified_on` ignored (it has a far-future default);
   - files: `path` as raw bytes, plus `size`.

   It also states:
   - **Privacy:** always `unknown`, because there's no private flag.
   - **Quality:** follows the IMP-1 rule for imported records. A record is never "verified" because Magnetico once fetched it.
   - **Readme text:** `is_readme`/`content` aren't imported. They're counted as a finding; annotations are IMP-06, deferred.
   - **File order:** file ids give a stable order, not the original bencode order or single-file mode, so neither is claimed.
2. **Opening the source:**
   - **Flags:** open read-only (`SQLITE_OPEN_READ_ONLY`), then `PRAGMA query_only=ON` and `trusted_schema=OFF`. Extension loading is off. Set `sqlite3_limit` bounds (length, SQL length, columns) and a progress handler, so every statement has bounded execution.
   - **Tables:** read only the base tables `torrents` and `files`. Never touch the FTS tables, views or triggers.
   - **Consistency:** refuse a live or inconsistent source. A non-empty `-wal` or a `-journal` sibling gives `source_not_quiescent`, with the safe step: stop magneticod, or use a consistent copy. Never use `immutable=1`, never write, never repair.
   - **Path:** an explicit path through a validated open function. Refuse device or pipe files, symlinks or reparse points that escape the given directory, and unapproved UNC paths. The CLI front end is IMP-4.
3. **Capability detection, not version numbers.**
   - Detect the required columns and the optional ones. Support the observed v3 schema and at least one older magneticod schema, each with fixtures.
   - Refuse unknown layouts with `unsupported_schema`, naming the missing columns.
   - The schema signature feeds the IMP-1 source identity: `{adapter=magnetico, dataset id, schema signature}`. The dataset id is content-derived (user_version, a normalised hash of sqlite_master SQL, page size and count, the first and last torrent id). A changed source resumes as `source_changed`.
4. **Keyset reads:**
   - **Torrents:** `WHERE id > ?cursor ORDER BY id LIMIT ?n`.
   - **Files:** for the batch's ids, ordered by `(torrent_id, id)`.
   - **No OFFSET** and no N+1 queries.
   - **Plan check:** assert the query plan uses the indexes (`EXPLAIN QUERY PLAN`) in a test on the fixture schema. If `files.torrent_id` has no index, use a bounded spool and never create an index on the source.
   - **Batch bounds:** ≤ 500 parents or 8 MiB, whichever comes first, with one reader. Respect the existing metainfo file-count and path limits.
   - **Over the limits:** a record over a limit is kept as partial, with a finding, and never silently truncated into a complete record.
   - **Malformed files:** a malformed file row marks its parent's file list incomplete, with a reason. It never drops the parent.
5. **Untrusted text:** names and paths are raw bytes. Decode them UTF-8 first, then lossy with a flag, and keep a hash of the original bytes (the same rule as IMP-0's SQL_ASCII handling). No path is ever used on the filesystem.
6. **Logs and metrics:** bounded reason enums and aggregate counts only. No names, hashes, paths or row content.

## Tests
- Fixture databases built in the tests: v3, the older schema, and a missing optional column.
- **Malformed rows:** a bad hash length, negative or huge sizes, invalid UTF-8, an oversized path, a file row whose parent is missing, a parent with no files, and duplicate ids across batches.
- **Source unchanged:** the file's bytes and mtime are unchanged after a full read, and any write attempt fails under `query_only`.
- **Resume:** resume mid-source, and a `source_changed` refusal.
- The non-quiescent refusal, and the query-plan assertions.
- **A property test:** random fixture DBs through the adapter into a temporary store keep IMP-1's counter conservation.
- **A `cargo fuzz` target** for the row normaliser, with seeds in `fuzz/seeds/`, added to the CI fuzz matrix.
- No fixed sleeps.

## Own
- `crates/sources-importers/src/magnetico.rs`, plus its tests and fixtures;
- the fuzz target and its CI matrix entry;
- the decisions file.

Name any other edit in the PR. Out of scope: the CLI (IMP-4), merge or dedupe policy beyond IMP-1's (IMP-4), annotations, and recovery.
