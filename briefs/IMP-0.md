# IMP-0: Bitmagnet restore runbook and checker
**Model:** Opus (it reads untrusted database content). This is lane I of the master plan (`docs/decisions/2026-10-30-master-plan.md`, "Imports"; owner-decisions §2.25).

## Base and PR timing
- Branch `slice/IMP-0` from `origin/land/L7`. That landing range holds the import plans: `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md` and `docs/plan/2026-10-29-dht-import-wizards.md`. Each now opens with a scope banner, and **the banner governs**.
- The PR goes to `main` after L7 merges; the runner tells you when. Until then: build, test, self-review, push, and message the runner "ready, waiting for L7". Then merge `origin/main` once and open the PR.
- `main` was rewritten on 2026-10-30. Never use an older clone.

## Scope
The owner holds a Bitmagnet `pg_dump` (`backup.sql`, about 97 GiB, from a 23 GB ZIP; `SQL_ASCII` encoding; Bitmagnet 0.10.0 schema) and a PostgreSQL 16 data directory. The plan's CLI import (IMP-3/4, later) reads Bitmagnet through `crates/sources-importers/src/bitmagnet.rs`, from a **restored, isolated PostgreSQL 16**. This slice makes that restore safe and checkable. **No import code, no migration, no Lodestar store changes.**

1. **Runbook: `docs/operations/imports/bitmagnet-restore.md`.** Developer and operator doc, plain and exact.
   - Run PostgreSQL 16 in an **isolated container with no network** (`--network none`; connect over a Unix socket in a shared volume, or a loopback-only port if the owner needs one).
   - Restore from a **copy** of the dump or data directory. The originals are made read-only and never touched.
   - Size the disk first: the dump, the restored cluster, WAL and temp space, plus a reserve. Include the commands to check free space before each step.
   - Restore the dump with `psql` (or `pg_restore`, per the dump format). Keep the database's declared `SQL_ASCII` encoding; **never let the restore transcode.**
   - Attach a data directory with a matching `PG_VERSION`.
   - Verify with the checker (item 2). Then create a read-only role for the importer, with the credential delivered by a file or environment variable, never argv.
   - Teardown and cleanup.
   - It's for Unraid (Docker) and for Windows with Docker Desktop.
2. **Checker: `crates/sources-importers/src/bin/bitmagnet_check.rs`,** a small CLI using the crate's existing `tokio-postgres` dependency. Add no new dependencies without asking the runner.
   - **Schema and version:** the server is PostgreSQL 16; the tables and columns the importer needs exist with the expected types (Bitmagnet 0.10.0, matching what `bitmagnet.rs` reads); unknown extras are listed, not fatal.
   - **Row counts** per table, plus `pg_database` encoding = `SQL_ASCII`.
   - **Sampled checksums:** N random rows per table, hashing the **raw bytes** of each text column, read as `bytea` via `convert_to(col, 'SQL_ASCII')` or a `::bytea` cast. **Never decode text into a Rust `String`.** It reports how many sampled values are valid UTF-8, which is information for the later decoder (UTF-8, then the declared encoding, then lossy with a flag, keeping the original bytes' hash).
   - **Free disk** on the data volume, against a reserve given as an argument.
   - **Output:** JSON (stable field names) plus a human summary. Exit codes: 0 OK, 1 a check failed, 2 couldn't connect or bad usage.
   - **Credential:** from `--password-file` or `PGPASSWORD`, never argv. TLS is optional; plain is allowed only for a Unix socket or loopback.
   - **Bounded:** every query has a statement timeout; nothing runs unbounded `SELECT *`.
3. **Tests.**
   - The CI job "bitmagnet importer (PostgreSQL)" already spins up PostgreSQL; follow its pattern.
   - Build a fixture database in the same schema with an `SQL_ASCII`-encoded database and rows containing non-UTF-8 bytes (Latin-1, GBK, invalid sequences).
   - Assert the raw-byte checksums and the UTF-8 counts.
   - A test that a missing column fails the schema check.
   - A test that the password never appears in argv or in output.

## Own
`docs/operations/imports/**`, `crates/sources-importers/src/bin/**`, `crates/sources-importers/tests/bitmagnet_check*.rs` and test fixtures under `crates/sources-importers/tests/fixtures/bitmagnet_check/**`. Name any CI change you need in the PR description; don't make it.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass. The PostgreSQL tests are skipped without a server, as the existing importer tests are; say how you ran them.
- The fresh-reviewer self-review is done.
- Disk is limited: use one `CARGO_TARGET_DIR`, with `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_INCREMENTAL=0`.

## Standing instructions
- Read CLAUDE.md, `runner/standards.md` and `.claude/skills/steward/SKILL.md` first; follow steward from your first push.
- Work only on `slice/IMP-0` and edit only the paths you own.
- The PR title is `IMP-0: <title>`.
- Message the runner (`send_message` to session_01EYhgh4JX3AmHzKXPjYCoUg) when ready, after fix rounds, and when blocked.
- **Do not merge.** No model identifiers in commits, PRs or code.
