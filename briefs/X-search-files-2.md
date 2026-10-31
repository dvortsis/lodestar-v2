# X-search-files-2: the cold broad file-clause path against §3, plus #278's optional findings
**Model:** Opus: search internals and the bench. Runner brief, 2026-10-31. Follow-up to X-search-files-perf (#278, merged 4d47440a). No schema or API change. No `/security-review` needed.

## Why
- §3 has the row "File-level query joined to torrents < 150 ms" (p95, set for the 10M target). #278 measured a **cold** broad file clause (`file:mkv`, every torrent with an .mkv) at **211 ms p95 at 1M torrents**, already over that row. The bench reports cold but doesn't gate it.
- The runner decided under §2.26 to merge #278 and close the gap here: gate what CI can measure, and find where the cold time goes.

## Read first
`docs/decisions/2026-10-31-X-search-files-perf.md` (#278's numbers and design), `docs/architecture.md` §3 (the latency rows) and §7, `crates/search/src/files.rs`, `bench/src/bin/search-bench.rs` (`file_sets_path`, KEYWORD_P95, the cold "browse" precedent), and `runner/standards.md`.

## Do
1. **Gate the cold path in CI:** add `FILE_P95 = 150 ms` (§3) and gate the cold broad-clause class with it at CI size. #278 measured about 40 ms at 200k, so it passes today and catches regressions. Keep warm at KEYWORD_P95.
2. **Profile the cold path at 1M** (release build, locally): split the 211 ms into the file-index scan, the id collection/sort/dedup, the set build and the join to torrents. Then, from the data, try the cheapest real fixes, for example:
   - collect ids straight into a bitset when the term's doc frequency is high (skip the sort and dedup);
   - reuse the segment's term postings without materialising a `Vec<u64>` first;
   - parallelise across segments if the search pool allows it without hurting other requests.
   Report before and after numbers at 200k and 1M. Getting under 150 ms at 1M is the goal, but don't trade correctness or memory for it. If it can't fit, say where the floor is and why, in the decisions file, and propose what the runner should decide (§2.26).
3. **#278 review, optional 2:** add a proptest for `IdPart`: random sorted, deduplicated sets (dense, sparse, crossing 64-bit word boundaries). `contains`, `for_each`, `last` and the `IdSet` union must all equal the plain sorted list.
4. **#278 review, optional 3:** the recency index's charge. Either charge ×3, or add a comment that the map's ×2 slack covers BTreeMap leaf underfill. Measure, then pick.
5. **Tests:** condition waits only, no fixed sleeps.
6. **Decisions file:** `docs/decisions/<date>-X-search-files-2.md`.

## Own
`crates/search/src/**` (files.rs and the set code), `bench/**`, their tests, and your decisions file. **Don't touch** `crates/store/**`, `crates/pipeline/**`, `crates/dht/**`, `crates/sources/**`, `crates/api/**` or `web/`: B-import-refile, X-polish-1031b and S-settings-2b are running.

## Size
≲ 600 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and the bench gates all pass.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after numbers. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
