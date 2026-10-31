# B-import-server-1: finish imported jobs on the server, and one shared filter loader
**Model:** Opus: the store's import state machine, the pipeline's filter engine and the server's background tasks (concurrency, content safety). Runner brief, 2026-10-31. Lane I, from IMP-4b's open items (`docs/decisions/2026-10-31-IMP-4b.md`, "The filter, as the server builds it" and the closing list). **No schema change** expected. `/security-review` is required, because this touches how the content filter is assembled.

## Why
- **Jobs never finish.** An offline import (`lodestar import start|resume`) ends in `indexing`. IMP-1 says it waits there for the index watermark to pass the job's `max_new_id`, then moves to `complete` or `complete_with_issues`. Nothing does that today: IMP-4b left it to "the server-side slice". Every pilot's job therefore stays in `indexing` forever, and `status` can't say when the import is searchable.
- **The filter engine is built in two places:** `crates/pipeline/src/lib.rs` (~l.463, private) at server start and `crates/lodestar/src/cli/import.rs` (~l.785) for the CLI. The #257 fresh review marked the drift risk important. A rule source added to one and not the other would let an import skip a filter the crawler applies.

## Read first
- `docs/decisions/2026-10-30-IMP-1.md` (phases, `max_new_id`, `complete` vs `complete_with_issues`, restart), `-IMP-4.md`, `-IMP-4b.md` (whole file), `-IMP-5.md` (the `import_finished` annotation).
- `docs/architecture.md` §20 (the filter pipeline, the locked floor, CS-1) and §6/§7 for the index watermark. `runner/standards.md`.
- `crates/store/src/import.rs` (Phase, the transition guard ~l.716–740, `interrupt_import_jobs`, `open_candidates`), and where the server records the index watermark (`grep -rn watermark crates/store/src crates/lodestar/src`).
- `crates/pipeline/src/lib.rs` (the engine assembly and its sources: floor sync, CS-1 entries and signatures, switches, packs, user rules, seal key) and `crates/lodestar/src/cli/import.rs`.

## Do
1. **One shared loader.** Make one public function in `pipeline` (or `filter`, wherever it fits without a dependency cycle) that builds the engine and sealer from the store and config exactly as the server does now. Both the server start and the CLI call it.
   - Keep each caller's behaviour: the CLI never creates `seal.key`, and a missing or bad key is a warning; rules that don't parse are skipped and counted.
   - Add a test that fails if the two paths diverge: the same store and config give the same verdicts for a hash in a config blocklist file, a CS-1 entry, a floor term and a user drop rule.
2. **Finish `indexing` jobs on the server.**
   - When the index watermark passes a job's `max_new_id`, move the job `indexing → complete` through the store's existing guarded transition. The store picks `complete_with_issues`. Cover jobs that reach `indexing` while the server runs, and jobs found in `indexing` at start (offline imports).
   - A job with no new rows (`max_new_id` none or 0) completes at once.
   - Use whatever the server already has for this (the indexer's commit hook, the watermark writer, or an existing periodic task); don't add a polling loop if a hook exists. If you do need a timer, it's event-or-interval, cheap on an idle store, and stops cleanly at shutdown.
   - Write the `import_finished` annotation IMP-5 expects at that moment, if IMP-5 left it to completion; check which.
   - Concurrency: the CLI holds the store's exclusive lock while it writes, so the server isn't running then. But a server and a read-only `status` can overlap; `status` must show the new phase without a restart.
3. **CLI wording:** `lodestar import status` no longer says the server "indexes them after it starts" once that's untrue; it says what happens now (one line, per the user-docs rules for CLI help).
4. **Tests:** the loader parity test; a job completing when the watermark passes `max_new_id` (and not before); `complete_with_issues` with open candidates; a job found in `indexing` at start; an empty job; shutdown while waiting. Condition waits only, no fixed sleeps, deadlines ≥ 10 s.
5. **Decisions file:** `docs/decisions/<date>-B-import-server-1.md`, plus a one-line pointer from IMP-4b's open list.

## Not in this slice
Importing through the running server's writer and `origin="import"` in the gate. That's B-import-server-2, after this merges.

## Own
`crates/pipeline/src/**` (the loader only), `crates/lodestar/src/cli/import.rs`, `crates/store/src/import.rs` (only if the transition needs a helper), the server's watermark/indexer hook in `crates/lodestar/src/**`, their tests, and your decisions file. Don't touch `web/`. Regenerate `openapi.json` and `schema.d.ts` only if an API type changes (not expected).

## Size
≲ 800 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, plus the openapi drift check. Run `/security-review` on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally. Push once per round of fixes.
