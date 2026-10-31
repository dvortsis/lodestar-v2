# IMP-4: the import driver, merge and dedupe, provenance, and the CLI
**Model:** Opus: the store writer path, the filter pipeline and untrusted import data. Runner brief, 2026-10-31. Lane I. Its dependencies have merged: IMP-1 (#210, V13 job table), IMP-2 (#225, Magnetico) and IMP-3 (#248, Bitmagnet).

## Why
IMP-1 built the contracts and the durable job table, and IMP-2 and IMP-3 built the two adapters. Nothing yet *runs* an import end to end. This slice adds the driver that runs batches through the filter engine into the store, source registration, the merge and dedupe rules, and the `lodestar import` CLI. After it, the owner can run the 10k and 100k pilots (master plan Part D5).

## Read first
- `docs/decisions/2026-10-30-IMP-1.md`: the contracts, the job table, the cursor, the one writer, "what this leaves".
- `docs/decisions/2026-10-30-IMP-2.md` and `docs/decisions/<date>-IMP-3.md`: each one's "Out of scope" names what IMP-4 owes. That includes:
  - a record `Store::prepare` rejects is committed as `Invalid`;
  - `OpenPolicy::approved_unc_roots` from the CLI;
  - persisting or printing per-reason counts;
  - the user docs about the WAL siblings beside the database.
- The master plan's "Imports" (owner-decisions §2.25, `docs/decisions/2026-10-30-master-plan.md`) and Part A6's acceptance criteria. **These carry over verbatim:**
  - batch commit plus cursor advance in one transaction, so a crash replays idempotently;
  - the cursor bound to source identity;
  - bounded reads;
  - the filter and tombstone checks run on every record;
  - truthful pause and cancel;
  - verification, completeness and recovery kept as three separate states;
  - private/public/unknown per record, never upgraded;
  - a file list doesn't mean a record is complete.
- `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md` §6–§8: the later design. The master plan's scope governs where it differs.
- `runner/standards.md`, and §20 (filters, tombstones, the floor) in `docs/architecture.md`.

## Do
1. **The driver** (`crates/sources-importers/src/driver.rs` or similar). It runs one job:
   - open the adapter, then check identity against the job's binding;
   - then, for each batch, run every record through the **full filter pipeline**: the floor, CS-1 hash lists, tombstones, core, packs, user rules. Tombstoned or deleted hashes are never resurrected;
   - then `Store::prepare`, then commit the batch's accepted records, findings and the cursor advance in **one** writer transaction.
   - Pause and cancel are checked between batches and report exactly what was committed.
   - A crash at any point replays idempotently. Test this with injected failures before commit, after commit, and mid-batch.
2. **Merge and dedupe** against existing records, by IMP-1's rules plus:
   - an import never overwrites a network-verified record's fields;
   - private is sticky (any source saying private wins);
   - a candidate (no name or info) never replaces a richer record;
   - import provenance is recorded per record and source, so a job's records can be listed and removed. A tested **"remove a job's records"** path supports rollback after a pilot (Part F17).
   - Imported cohorts keep their origin, so IMP-5 can keep them out of crawl-yield Insights. Don't build IMP-5 here.
3. **Source registration:** stable adapter sources (`magnetico`, `bitmagnet`), with upstream labels kept as provenance, not as independently observed discoveries.
4. **The CLI:** `lodestar import inspect|start|status|pause|resume|cancel|remove`.
   - `inspect` is read-only: identity, capabilities, a row estimate, and a free-space check against the measured per-record size, with a clear refusal if space is short.
   - `start` takes `--source magnetico --path …` or `--source bitmagnet` with IMP-3's structured connection flags.
   - Running `start` while the server is up goes through the single writer. If it can't do so safely in this slice, refuse with a clear message to stop the server first, and say so in the decisions file.
   - Keep `import-bitmagnet` as a compatible alias that prints migration guidance. The legacy cursor binding comes from IMP-1's "what this leaves".
   - `OpenPolicy::approved_unc_roots` comes from a flag or config.
   - Output is plain and scriptable (`--json` where it's cheap), and per-reason counts are printed or persisted.
5. **Throughput target:** state one in the decisions file, for example a time-to-import for 10M at ≤ 50% of the writer share (master plan F16). Measure it on a synthetic 100k fixture in a test or bench, so the pilots have a pass line. Don't tune it to win.
6. **Docs:** `docs/operations.md`, the imports section. Write it for an Unraid user, per the `user-docs` skill:
   - stop the server or not;
   - back up first (`lodestar backup`);
   - the WAL siblings note;
   - how to remove an import.
7. **Tests:**
   - end to end over both adapters' fixtures, with filter interaction (a floor hash in a source is refused, and a tombstoned hash stays gone);
   - idempotent replay under crash injection;
   - pause, resume and cancel truthfulness;
   - the merge rules, and remove-a-job;
   - CLI parsing and output.

   Use condition waits only, never fixed sleeps.
8. **Decisions file:** `docs/decisions/<date>-IMP-4.md`.

## Not in scope
- IMP-5 (Insights cohorts), recovery (CR-3), the dump parser, the wizard and uploads.
- No migration. **If you need a schema change, stop and ask the runner;** V14 is reserved for CR-3.

## Own
`crates/sources-importers/**` (except the adapters' internals, beyond small hooks), `crates/lodestar/src/cli/**`, the import paths in `crates/store/**` (justify each change), `docs/operations.md`'s imports section, and your decisions file.

## Size
Aim for ≲ 1,500 changed lines per PR. Plan the split:
- **4a:** the driver, filter integration, merge and dedupe, provenance and remove, with tests;
- **4b:** the CLI, docs and the throughput measurement.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass, including the PostgreSQL job with `LODESTAR_REQUIRE_POSTGRES=1`;
- the cargo-tree guard passes;
- `/security-review` is clean on each PR's diff.

Open the PR(s), follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with each PR's number and head. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
