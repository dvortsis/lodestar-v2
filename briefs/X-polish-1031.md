# X-polish-1031: backend polish from today's reviews (pipeline, Insights CSV, search recovery)
**Model:** Opus: the pipeline (concurrency), Insights rollups and search. Runner brief, 2026-10-31. Optional findings from the #266 and #267 reviews that the runner chose not to hold those PRs for. No schema or index format change.

## Do
1. **Saved refetches** (`crates/pipeline/src/lib.rs:~928`): they're dropped by `strategy == 200` alone. Make the drop also require `cohort == REFETCH_COHORT` (or the peer finder owning `source`), or add a test that no in-tree plugin manifest or strategy constant uses 200. Prefer the code fix.
2. **Insights floor** (`crates/lodestar/src/insights.rs:~243`): `min(cohort)` over the raw saved queue should skip `strategy == REFETCH` entries, so a refetch saved by a pre-#267 build doesn't hold sealing back once. Test it.
3. **A crawl sighting under a refetch lease:** in `crates/pipeline/tests/pipeline.rs` `refetches_stay_out_of_the_funnel` (or a sibling), send `Found::key(refetch, 0)` from the feed and assert no Harvested row, the cohort totals unchanged, and the record stored once with IMPORTED.
4. **CSV header guard** (`crates/api/src/insights/csv.rs`): extend `columns_are_sorted_but_the_annotations_lead` to render each panel's fixture and assert its header equals period plus the declared columns (with `_utc`), so an undeclared field can't move columns silently.
5. **Recovery caching** (`crates/api/src/search.rs`): cache `SearchRecovery` beside the cached empty first page, keyed by the same fingerprint and invalidated the same way, so repeats don't recompute up to 20 ms. Test that a cached repeat doesn't recount, and that a commit invalidates it.
6. **Bench:** add a gated zero-result class to `bench/` search latency that runs the recovery pass (the #266 author measured p95 ≤ 6.0 ms and max 10.1 ms on 200k with an uncommitted patch; see `docs/decisions/2026-10-31-B-search-recovery.md` "Cost"). Gate at the decisions file's 20 ms deadline. Follow the existing bench gating pattern.
7. **Decisions note:** one line in `docs/decisions/2026-10-31-B-refetch-funnel.md`: a recovered row has no import-job provenance, so Imports-screen counts by job won't include it (only `insights_state.imported` will). Add a decisions file for this slice only if a choice is made beyond the above.

## Read first
`docs/decisions/2026-10-31-B-refetch-funnel.md`, `-B-search-recovery.md`, `-IMP-5.md`, CLAUDE.md, `runner/standards.md`, and the files above.

## Own
The files named above, `bench/` (only the new class and its gate), and the decisions lines. Nothing else.

## Size
≲ 600 changed lines.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the search latency (§3) bench passes, including the new class;
- the openapi drift check passes (no API change expected).

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally. Push once per round of fixes.
