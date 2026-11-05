# X-search-files-4: meet the 10M file-search gate (cold p95 < 150 ms)
**Model:** Opus (search core). Fresh Opus review follows. Runner brief, 2026-11-05. Owner priority: performance.

## Why
X-search-files-3 (#429, merged b08e161f) made size clauses about twice as fast, but the 10M gate (architecture §3: a file-level query joined to torrents < 150 ms) is still missed: cold p95 263.7 ms, warm p95 259.8 ms (gate 30). Measured on `file:(ext:mkv)`: ~85 ms scanning the file index and ~55–65 ms joining to torrents, both linear in the corpus. The 8 MiB file-set cache hits 8.8% of lookups at 10M. Runner rulings (#429): sizing the cache with the corpus is off the table (the owner's memory guarantee); more cores is not a fix.

## Read first
`docs/decisions/2026-11-04-X-search-files-3.md` (options (a)–(d) and the lazy-edge-settle follow-up), `docs/decisions/2026-10-31-X-search-files-2.md`, `crates/search/src/files.rs`, `crates/search/src/compile.rs` (smart coverage), `bench/src/bin/search-bench.rs`, architecture §3 and the search sections.

## Do
1. **Rebuild the 10M bench index** the way #429 did (its decisions file and search-bench), and reproduce main's cold/warm p95 first.
2. **Measure option (a):** answer one-atom file clauses (`file:(ext:x)`, `file:word`) from the torrent index. It changes results because of smart coverage, so before any production code, write a short evidence note in the decisions file: which queries change, how (with examples from the relevance fixture), and the latency each option gives. Send it to the runner and wait for the runner's call on the semantics before wiring it in.
3. **Measure option (b):** torrent-level `size_bucket` terms. It's a search schema change (SB-6's population contract); estimate the index size cost against the §3 budget and the rebuild/upgrade path. Report; don't build without the runner's go.
4. **Anything else within `files.rs`** that cuts the scan or join (e.g. a cheaper id set for the join, block-skipping on the file index) can be built directly, with before/after numbers.
5. **Fold in #429's review nits:** assert the id column is `ColumnIndex::Full` in the join test, add an id > u32 case and a non-full column case; fix the wrong comment in `the_column_fallback_matches_nothing_outside_the_segments_sizes`; note which column-fallback guard is belt-and-braces; per-piece `scan_buckets` gather sizing.
6. **Gate report:** cold and warm p95 at 10M for the gate's query set, on the same hardware class as #429, with the method stated.

## Own
`crates/search/src/files.rs`, `crates/search/tests/files.rs`, `bench/src/bin/search-bench.rs` (reporting only), `docs/decisions/2026-11-05-X-search-files-4.md`. `compile.rs` and the schema only after the runner's go on (a)/(b).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the relevance fixture unchanged unless the runner accepts (a). PR "X-search-files-4: …" against `main` from `slice/X-search-files-4`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
