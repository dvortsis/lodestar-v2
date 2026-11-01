# X-file-clause-cold: the cold broad file clause misses §3; the percolator's file path isn't cancellable
**Model:** Opus (search core, concurrency). An Opus reviewer follows. Runner brief, 2026-11-01. From the runner-state followups (#278, #283, X-search-files-perf).

## Why
1. **A §3 budget miss.** `docs/decisions/2026-10-31-X-search-files-perf.md` (#278) measured a cold, broad file clause (e.g. every `.mkv`) at 1M torrents: 211 ms p95, against §3's 150 ms file-level row. Warm it's 9 ms. #283 added: at 10M, the cold broad size scan may cross 150 ms on 4 cores, so indexing size in the file index (an index-format change) might be needed.
2. **The percolator's file-clause path uses a fresh `CancelToken`.** X-search-files-perf left it out to avoid overlapping B-import-server-1, which has since merged. An alert with a broad file clause can't be cancelled or budgeted.

## Read first
- `docs/architecture.md` §3 (search budgets and the file-level row), §8 (file clauses), §7 (the file index), §16 (decisions on the file cache).
- `docs/decisions/2026-10-31-X-search-files-perf.md`, `-X-search-files-2.md`, and the B-import-server-1 decisions.
- `crates/search/src/files.rs`, `crates/search/src/percolate.rs` (and wherever the percolator evaluates file clauses), `bench/` (`search-bench`, its `--gate`, and the 1M/10M synthetic corpora).

## Do
1. **Measure first.** Cold and warm p95 for broad file clauses (`ext:mkv`, `file:(size:>1GB)`, a combined one) at 1M and, if the bench supports it in reasonable time, 10M, on 4 threads. Where does the cold time go (I/O, decode, set building, the size scan)? Put a profile summary in the decisions file.
2. **Fix within the current on-disk format if you can:**
   - a faster cold path (a cheaper first-touch decode, prefetch, a narrower scan);
   - or warming the file-ID cache for the most common broad clauses after the index opens, inside the existing 8 MiB budget, off the request path, cancellable on shutdown.

   Target: cold broad p95 ≤ 150 ms at 1M. **Don't change the index format, and don't raise the 8 MiB cache budget** (an owner-set number via §16; a change goes to the runner).
3. **If the target needs an index-format change** (size in the file index), stop at a measured proposal: what changes, the space cost at 1M/10M, the rebuild path. The runner decides; the owner sees format changes.
4. **Percolator:** pass the real request/job `CancelToken` (with its budget) into the file-clause path. Add a test that a cancelled or over-budget percolation with a broad file clause stops promptly (condition wait, ≥ 10 s deadline).
5. **Gate it:** add the cold broad file clause to `search-bench --gate` at 1M with the §3 bound, if it fits CI time.
6. **Decisions file:** `docs/decisions/<date>-X-file-clause-cold.md` with before/after numbers.

## Own
`crates/search/src/files.rs`, `crates/search/src/percolate.rs` (the file-clause cancel path), `bench/**` additions, the CI gate line for search latency (only that job), tests, your decisions file.
**Don't edit** the store schema, the index schema or `crates/search/src/suggest/**`.

## Size
≲ 600 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `search-bench --gate` passing locally twice.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table (or the format proposal). Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes. Don't background long jobs with `&`.
