# X-importer-schema-limit decisions (#1024)

Runner brief X-importer-schema-limit, 2026-11-21, from #1018's review. SQLite 3.54 adds `SQLITE_LIMIT_SCHEMA`, and rusqlite 0.41 exposes it as `Limit::SQLITE_LIMIT_SCHEMA`. Without it, a hostile Magnetico file with a huge schema could make schema loading allocate a lot of memory before the progress-handler deadline stops it.

## What changed

- **The cap:** `harden()` (`crates/sources-importers/src/magnetico.rs`) sets `SQLITE_LIMIT_SCHEMA` to `MAX_SCHEMA_OBJECTS`, which is 256. It sits next to the other limits, before any statement runs.
- **The refusal:** a file over the cap is refused as `Refusal::Unavailable("schema_too_large")`. That's the importer's existing path for a file it can't read, the same one `value_too_big` takes.
  - Its label is `source_unavailable`.
  - Its message has its own arm: "the database can't be read (schema_too_large): it has far more tables or indexes than a magneticod database has, so it isn't one Lodestar can import". The generic "check the file, or import a copy" wouldn't help, since a copy has the same schema. The text is fixed: it never names a table, an index or anything else from the file.
  - A running job blocks with IMP-1's `SourceUnavailable`, as it does for `value_too_big`.
  - There's no panic path: the error reaches `open()` through `map_err`, like any other SQLite error.

## What SQLite counts, and when (3.54.1 source, `libsqlite3-sys` 0.39.1)

- **What it counts:** `sqlite3InitCallback` checks, after each schema row it loads, whether the main schema's tables, indexes and triggers together pass the limit. Views are in the table hash, so they count too, as the C API doc says.
  - Internal objects count as well: `sqlite_schema` itself, every `sqlite_autoindex_*`, and the primary-key index each `WITHOUT ROWID` table makes, which has no row of its own in `sqlite_schema`.
  - The temp schema isn't counted. Attached databases don't matter here, since `SQLITE_LIMIT_ATTACHED` is 0.
- **When it applies:** the check runs while the schema loads, row by row. For a well-formed schema, the cap bounds parsing: SQLite refuses the file at the first object over it. For a schema SQLite finds corrupt, the statement deadline bounds parsing, as it did on `main` before this change: SQLite checks the cap only while no load error is set, so the file is refused as `Corrupt`. Each object's SQL is already capped at 64 KiB by `SQLITE_LIMIT_SQL_LENGTH`, and its columns at 64 by `SQLITE_LIMIT_COLUMN`.
  - The schema loads lazily: neither the open nor `harden()`'s pragmas need it, since `query_only`, `trusted_schema`, `cell_size_check` and `mmap_size` don't carry `PragFlg_NeedSchema`.
  - So it loads at `schema()`'s first `sqlite_master` query, with the statement deadline and the progress handler already armed.
- **The error:**
  - The callback sets "too many schema objects" with `SQLITE_ERROR`, but it stops the load by returning non-zero from `sqlite3_exec`, so the code that comes back is `SQLITE_ABORT`.
  - rusqlite reports that as `ErrorCode::OperationAborted`, carrying SQLite's message.
  - `map_err` maps exactly that pair (the code and SQLite's fixed text) to `schema_too_large`.
  - Any other `SQLITE_ABORT` keeps falling back to `read_failed`. So does this one if a later SQLite rewords its message: it's still refused, just with the generic label.

## The cap: 256, from magneticod's schemas

Each count below is the smallest `SQLITE_LIMIT_SCHEMA` the fixture opens under, so it includes the internal objects above. The fixtures replay magneticod's own DDL history (`pkg/persistence/sqlite3.go`), so each is shaped like a real file.

| Layout | Objects | Made up of |
| --- | --- | --- |
| v0 | 4 | `sqlite_schema`, `torrents`, its `UNIQUE` autoindex, `files` |
| v1 | 5 | v0, plus `info_hash_index` |
| v2 | 6 | v1, plus `readme_index` |
| v3 (also without `n_seeders`) | 18 | v2, plus the FTS5 table `torrents_idx`, its four shadow tables (`_data`, `_idx`, `_docsize`, `_config`) and the two `WITHOUT ROWID` primary-key indexes, plus four triggers and `modified_on_index` |

- **The largest real schema is v3, at 18.** Magnetico's schema never shrinks between versions, and the importer accepts v1 to v3 (v0 is refused for its missing files index).
- **256 is about 14 times that.** It leaves room for anything a user may have added by hand: their own indexes, `ANALYZE`'s `sqlite_stat1` and `sqlite_stat4`, or a second FTS table. That's far more than any of these needs.
- **It's still small enough to bound the work.** SQLite's own default is 10,000,000.

## Tests (`crates/sources-importers/src/magnetico/tests.rs`)

- **`magneticods_schemas_are_far_under_the_schema_cap`** measures every magneticod layout with a raw connection and pins the table above. It also checks that the cap is at least 14 times v3's count. If a fixture grows, the test fails and the number here gets rechecked.
- **`a_schema_at_the_cap_opens_and_imports`:** a v3 file with 238 extra indexes counts exactly 256. It opens through `open()` and imports all three of its rows.
- **`a_schema_over_the_cap_is_refused_without_naming_it`:**
  - one more index (257) is refused as `schema_too_large`, with the `source_unavailable` label, and its message names neither the extra indexes nor any table;
  - a file 8 times over the cap is refused the same way.
- **The real-shaped files still import:** `supported_layouts_open_and_read` opens and reads v1 to v3 through `harden()`, now with the cap set.
- **The tests generate every database themselves,** so there are no binary fixtures. The three new tests take about 1 s together. The over-cap file uses 8 times the cap, not 20,000 objects: building 20,000 indexes took 20 s and proved nothing more.
- **`only_sqlites_schema_limit_message_is_schema_too_large`:** a constructed `SQLITE_ABORT` with any other message, or none, still maps to `read_failed`.
- **`MAX_SCHEMA_OBJECTS` is pinned** at 256 by the first test, so a changed cap fails it.
- **Mutation checks:**
  - removing the `SQLITE_LIMIT_SCHEMA` line fails the over-cap test, because the file opens;
  - changing the matched message fails it too, because the refusal falls back to `read_failed`;
  - loosening the match to any `SQLITE_ABORT` fails `only_sqlites_schema_limit_message_is_schema_too_large`.

## Owns

`crates/sources-importers/src/magnetico.rs`, `crates/sources-importers/src/magnetico/tests.rs` (added to this slice's `owns` in this PR, with the runner's approval), this file and this slice's `runner/slices.yaml` entry. `fixtures.rs` is reused, not edited.
