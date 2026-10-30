# IMP-1: import contracts and the durable job table (V13)
**Model:** Opus. This is store schema plus a one-way migration, so main session or Opus only. Lane I of the master plan (`docs/decisions/2026-10-30-master-plan.md`, "Imports"; owner-decisions §2.25).

## Base and PR timing
- Branch `slice/IMP-1` from `origin/main`. Open a PR to `main`.
- `release/v0.5.1` is already cut (`c3f6341a`), so V13 may land on `main` now (plan G4).
- `main`'s nightly image will then carry V13. A v0.5.1 binary must refuse a V13 store cleanly, never corrupt it. Test that (item 5).

## Read first
- `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md` §6 (provenance, quality, the three separate states) and §7 (durable jobs). Its scope banner and the master plan govern; this slice is the plan's IMP-01 contracts plus the durability core of IMP-02.
- `crates/store/src/schema.rs` (MIGRATIONS, V12, `migrate`), `crates/store/src/lib.rs` (the single writer, source merge, prepared records).
- `crates/api/src/jobs.rs` (the in-memory registry, whose semantics must **not** be inherited).
- `crates/sources-importers/src/bitmagnet.rs` and `src/bin/bitmagnet_check.rs` (IMP-0).

## Do
1. **Contracts first:** `docs/decisions/<date>-IMP-1.md`, sent to the runner for an OK **before** you write the migration. It proposes:
   - the V13 tables, named per store conventions:
     - `import_jobs`: id, adapter and schema versions, source identity, phase, user control, options, counters, last error code, revision;
     - `import_cursors`, bound to the source identity;
     - `import_membership` (compact);
     - `import_candidates`: valid hashes too incomplete for the canonical row; no empty names, no zero sizes;
     - the per-record provenance, quality and privacy columns or tables.
   - the state model: phases inspecting → ready → reading → committing → indexing → complete / complete-with-issues; controls paused / blocked / cancelled / failed; `needs_credentials`;
   - the three independent per-record states: completeness, verification and recovery eligibility, plus private/public/unknown;
   - the counter conservation equation;
   - the store API for one destination batch.
2. **The batch commit:** one SQLite transaction holds the records, provenance and quality, the reindex entries, the outcome counters and the cursor advance. The ACK comes only after commit. Replay after a crash between commit and ACK is idempotent, and counters never double-count. It goes through the existing single writer, never a second one. The filter, floor, tombstone and quarantine checks run on every record. An import can never:
   - downgrade a verified row;
   - resurrect a tombstone;
   - clear a private restriction;
   - count as a DHT discovery: no `origin=network` in the #199 gate counters; use `origin=import`.
3. **Source identity:** a cursor is bound to `{adapter, dataset id, schema signature}`. Resuming against a different source fails with `source_changed`. The legacy unqualified `import-bitmagnet.cursor` is never trusted automatically.
4. **Durable jobs:** they survive a restart. A user-paused job stays paused. A job that was running revalidates its source, credentials and space before resuming, or blocks with a reason. Credentials are sealed references, never stored in cursor JSON, arguments, logs or metrics.
5. **Migration V13:**
   - additive only;
   - a bounded migration marks existing imported rows' verification `unknown`, without scheduling anything;
   - the backup and restore paths include the new tables;
   - a test opens a V13 store with the v0.5.1 code path and asserts a clean "newer schema" refusal. Add the refusal if it doesn't exist.
6. **Out of scope:**
   - adapters (Magnetico is IMP-2, Bitmagnet hardening IMP-3);
   - the CLI (IMP-4);
   - the UI;
   - recovery (CR-3, V14);
   - the SQL/ZIP parser;
   - any change to `crates/api/src/jobs.rs` beyond what's needed to not break.
7. **Tests:**
   - crash at every step of a batch (a fail-point seam), then replay;
   - pause, cancel and restart;
   - a changed source;
   - counter conservation under replay;
   - a tombstoned, quarantined or verified row against an import;
   - private stickiness;
   - a property test over random batch sequences;
   - the migration from a V12 fixture.

   No fixed sleeps.

## Own
`crates/store/src/**` (schema, the import module), `crates/store/tests/**`, and `docs/decisions/<date>-IMP-1.md`. Name any other edit in the PR: the pipeline gate's `origin` label, the backup file list, `docs/metrics.md`.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the steward self-review is done and the runner has OK'd the contract;
- the PR states its size reason if it's over ~1,500 lines, and is split if it's over ~3,000.
- Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg). No model identifiers in commits, PRs or code. Never merge.
