# IMP-3: Bitmagnet adapter hardening
**Model:** Opus: an untrusted-input reader, credentials, and the import contracts. Runner brief, 2026-10-30. Lane I. Its dependency, IMP-1 (#210, V13), has merged, and IMP-2 (#225, Magnetico) is your pattern.

## Why
`crates/sources-importers/src/bitmagnet.rs` is the old M-era importer. It:
- connects with `NoTls` from a URL;
- reads `name` and paths into Rust `String`, which fails on the `SQL_ASCII` bytes IMP-0 found;
- assumes a name and size;
- pages by hash with no byte bound;
- has a cursor that isn't bound to the source.

IMP-4 (merge, dedupe and the CLI) needs Bitmagnet as an IMP-1 adapter, the way IMP-2 made Magnetico one.

## Read first
- `docs/decisions/2026-10-30-IMP-1.md`: the contracts, the job table, the opaque position, source identity, and the "verified / complete / recovery" separation.
- `docs/decisions/2026-10-30-IMP-2.md`: **your pattern.** Follow its sections: column mapping, quality, privacy and file order; untrusted text as raw bytes; file rows; opening the source; capability detection and identity; keyset reads; batch bounds; position. Where Bitmagnet needs a different answer, say so in your decisions file.
- `docs/operations/imports/bitmagnet-restore.md` (IMP-0): the restore you read from, which is PostgreSQL 16, isolated and read-only, on a copy. It also gives the `SQL_ASCII` rule.
- `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md`, "Bitmagnet PostgreSQL" (about line 164). Its scope note at the top governs: the dump parser and live-source import are deferred.
- `runner/standards.md`.

## Do
1. **An IMP-1 adapter** in `bitmagnet.rs`, with the same shape as `magnetico.rs`: open with a policy, then identity, capability detection and keyset batches that yield IMP-1 records and candidates. Keep the old `import()` path working until IMP-4 replaces the CLI. If it's cleaner, put the new adapter beside it, and say which in the PR.
2. **The connection:**
   - Use structured settings (host, port, dbname, user), not a free-form URL, plus a credential from an env var or a file, never from argv. The CLI can take `--password-file` and `LODESTAR_IMPORT_PG_PASSWORD`. Never log the password, and keep it out of `Debug`.
   - Use TLS with certificate verification for any non-loopback host. Plaintext only for loopback, with an explicit `--allow-plaintext-loopback` (or a config equivalent).
   - Take no arbitrary `options`, `search_path` or SQL from input.
   - Every session runs `SET default_transaction_read_only = on`, a `statement_timeout`, a `lock_timeout` and `idle_in_transaction_session_timeout`.
   - Reads are schema-qualified, allowlisted queries.
   - Pick the TLS crate to fit the workspace: prefer what is already in `Cargo.lock` (rustls). Any new dependency needs a reason in the decisions file, and the cargo-tree guard must still pass.
3. **Raw-byte text (IMP-0).** Read `name` and path columns as `bytea` (`convert_to(col, 'SQL_ASCII')` or a cast that keeps the bytes; whichever IMP-0 verified). Never read them as `String`. Decode the way IMP-2 does: UTF-8 if valid, otherwise lossy with a flag, with the original bytes kept in `name_raw`. If IMP-0 named a declared-encoding step, follow it. Rows with no name are **kept**, as candidates or partial records per IMP-1, never dropped.
4. **Capability detection by columns, with a fixture for each layout:**
   - the observed 0.10.0 schema;
   - with and without `files_count` and `torrent_pieces`;
   - the legacy `single_file` layout.

   Refuse an unsupported identity or schema with a fixed reason. Source identity is IMP-1's `{adapter, dataset, schema_sig}`, plus whatever binds it to *this* database: the system identifier or database OID and the name. A resumed job must not quietly continue against another database with the same name.
5. **Bounded keyset reads.** Page by `info_hash` (20-byte only; refuse other identity widths with a finding). Bound each batch by count and by memory, as IMP-2 does: 500 parents or 8 MiB, charged per kept row.
   - Fetch files with one ranged or `= ANY` query per batch, in bounded chunks.
   - A torrent with a huge file list is spooled or capped as `partial` with a finding. Never load every file for a batch into RAM.
   - Each batch is consistent in one read-only transaction, and no transaction stays open between batches.
6. **Quiescence:** a complete snapshot import needs a quiescent source. Detect a live source (for example, `pg_stat_activity` writers, or a change in `pg_stat_database` `xact_commit` across the open) and refuse it as `source_not_quiescent`. Live, best-effort import is out of scope.
7. **Privacy, verification and completeness** follow IMP-1 and IMP-2. Bitmagnet's `private` column maps to private only when it is true. Records are never `verified`. File order and single-file mode are claimed only when the schema really proves them; say which in the decisions file.
8. **Tests:**
   - Unit tests and property tests for the conversion: arbitrary bytes in names and paths, odd sizes, missing columns.
   - A `cargo fuzz` target or proptest over the row-to-record mapping.
   - Integration tests against PostgreSQL: CI's existing "bitmagnet importer (PostgreSQL)" job, extended. Cover each schema fixture, TLS refusal for non-loopback plaintext, the read-only session, the timeouts, a non-UTF-8 name, a no-name row, file-batch bounds, refusal of a source mismatch on resume, and the quiescence refusal.
   - No silent skip when PostgreSQL is missing in CI: the job must fail, not pass empty.
9. **Decisions file:** `docs/decisions/<date>-IMP-3.md`, laid out like IMP-2's.

## Not in scope
The SQL/ZIP dump parser, live-source import, the new CLI verbs (IMP-4), merge and dedupe (IMP-4), the Insights cohorts (IMP-5), and recovery (CR-3). No migration: V13 is already there. If you think you need V14, stop and ask the runner.

## Own
`crates/sources-importers/src/bitmagnet*` (a new submodule is fine), its tests and fixtures, and the `import-bitmagnet` lines in `crates/lodestar/src/cli/` for the credential and TLS flags. Also the CI PostgreSQL job steps for this adapter, and your decisions file. Don't touch `magnetico*` beyond sharing a helper; if you do share one, say so.

## Size
Aim for ≲ 1,500 changed lines (fixtures and tests are most of it). If it grows past that, split it:
- **part 1:** connection, TLS, credentials and the read-only session;
- **part 2:** the adapter, raw bytes, capabilities and batches.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the PostgreSQL integration job passes;
- the cargo-tree guard passes;
- `/security-review` is clean on your diff (run it yourself and fix what it finds).

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
