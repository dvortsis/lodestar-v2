# X-import-refile-2: import polish from the #276 and #280 reviews
**Model:** Opus: the pipeline writer, the store's import counters and content safety (§20). Runner brief, 2026-10-31. The optional findings from B-import-server-1 (#276) and B-import-refile (#280), which the runner folds into one slice. No schema change. `/security-review` is required, because item 1 touches when a held name stays at rest.

## Read first
`docs/decisions/2026-10-31-B-import-refile.md`, `-B-import-server-1.md`, `-IMP-4b.md`, `2026-10-30-IMP-1.md` (conservation), `docs/architecture.md` §20.9, CLAUDE.md and `runner/standards.md`.

## Do
1. **Retry the sweep after a transient error** (#280 review 1). In `crates/pipeline/src/writer.rs` (`refile_candidates`, the Err arm), a store error such as SQLITE_BUSY past the 5 s busy timeout sets `candidates.done = true`, so names the current rules hold stay at rest until the next rule change or restart.
   - Leave `done = false` and retry on a later flush tick, through a `retry_at` checked in `note_engine` or the tick, with a backoff so it never spins.
   - If backfill has the same pattern, say so in the decisions file and fix it the same way only if it's a few lines; otherwise log a follow-up.
2. **The HeldSet race** (#280 review 2). In `writer.rs`'s `Ok(Some(b))` arm, insert into `self.held` only when `self.filter.current().version == version`; otherwise drop the hashes, because the restarted sweep or a store reload settles them.
3. **The counter clamp** (#280 review 3). In `crates/store/src/import.rs` (~l.1399), `candidates = max(candidates - 1, 0)` can hide a conservation break. Skip the counter move when `candidates = 0` (with a debug assert), so `filtered`/`quarantined` never gain a phantom +1.
4. **A live log sink in the no-name test** (#280 review 4). The capture test in `writer.rs` (~l.1230) passes even with no sink. Add a positive control, a known event or `lines("pipeline.")`, so it proves the sink is live.
5. **Decisions notes** (#280 review 5). In the B-import-refile decisions file, add one line: hash-trigger removals (CS-1 hashes, tombstones) and candidates stored as torrents don't move counters or flip `complete_with_issues`; only the sweep does.
6. **A class-rule test** (#280 review 6). In `import_refile.rs`, an Is-adult class rule plus the Adult-only core gives an `adult_hold` quarantine for a matching candidate, using `load_class_rules` as `tests/corrections.rs` does.
7. **Parity for floor-marked subscribed lists** (#276 review 2). In `crates/lodestar/src/cli/import/tests.rs` (the `seeded` closure, ~l.487), add a subscription with `floor: true`, store a version holding `hash_of(6)` via `Store::record_refresh`, and assert it's blocked on both paths. Remove the "Left untested" bullet from the B-import-server-1 decisions file.
8. **A stale sentence** (#276 review 1). In `docs/decisions/2026-10-31-IMP-4b.md:141`, drop "`status` says the server indexes them after it starts." and keep "Done in …".
9. **Unify the importer's judge with the sweep** (from #280's decisions file). Make `sources-importers`' `judge_candidate` call the same function the sweep uses (pipeline `enrich.rs`), so the two can't drift, including the hybrid's truncated v2 hash check. If a dependency cycle prevents that, put the shared function where both can use it and say where.
10. **Tests:** condition waits only, no fixed sleeps, deadlines ≥ 10 s.
11. **Decisions file:** `docs/decisions/<date>-X-import-refile-2.md`, one line per item.

## Own
`crates/pipeline/src/{writer.rs,enrich.rs}` and their tests, `crates/store/src/import.rs` (item 3 only), `crates/sources-importers/src/driver.rs` (item 9), `crates/lodestar/src/cli/import/tests.rs` (item 7), the three decisions files named above, and yours. **Don't touch** `crates/search/**`, `bench/**` or `web/`: X-search-files-2 and S-insights-3 are running.

## Size
≲ 600 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (set `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`, because full debuginfo fills the disk), plus the openapi drift check. Run `/security-review` on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
