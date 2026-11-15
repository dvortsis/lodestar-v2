# X-index-bytes-1m: torrent index bytes per torrent at 1M scale

**Model:** Opus (search storage is core). **Branch:** `slice/X-index-bytes-1m`. **PR title:** `X-index-bytes-1m: <title>`.

## Why
X-test-vm's 1M import run (Magnetico 1M, main@92b181bd, 2026-11-14) measured the **torrent index at 652 B/torrent**. The budget is `INDEX_BUDGET` = 512 B in `bench/src/bin/size-report.rs` (§3/§10.1). The file index (769 B, under 800) and SQLite (769 B, under 1,000) were within budget. The budget is not loosened (runner; owner number).

## Steps
1. **Check the measurement first.** `size-report` counts only the top-level files in the index directory (`dir_size` doesn't recurse). An import-host figure taken with `du` can include segments that haven't been merged or garbage-collected yet, plus deleted documents waiting for a merge, or a second copy left by a rebuild. Reproduce at 1M with `size-report`'s own generator (and with a Magnetico-shaped corpus if one is available), after the final commit, merge and GC. Report segment count, deleted docs and bytes per torrent.
2. **Attribute the bytes.** Add a per-field and per-component breakdown, using tantivy's `Searcher::space_usage()`: postings, positions, term dictionary, fast fields, field norms, doc store, deletes. Expose it as a `size-report --space-usage` table, or as a small `index-stats` bench bin. Dev tooling only, nothing in the product binary. Give it a unit test on a small index.
3. **Fix within the budget, if step 1 confirms the overrun.** Choose by the biggest contributors, for example:
   - positions kept on fields that never take a phrase query;
   - stored fields that duplicate the store;
   - fast fields that no query or sort reads;
   - an oversized doc-store block, or a compression level.

   Every change must keep search behaviour and relevance: the relevance gate, the search tests and the query-language golden tests all pass unchanged. A schema change follows the existing schema-version and rebuild path, with an upgrade test.
4. **Gate:** CI's size-report keeps failing over 512 B. If CI only runs a smaller scale, state the 1M figure in the decisions file.
5. If the overrun is a measurement artefact (step 1), the deliverable is the stats tool plus a decisions note saying how to measure. Make no schema change.

## Rules
- Don't touch the crawler, the importer's throughput (that's X-import-rate-1m, a separate slice), or any budget number.
- No fixed sleeps in tests; condition waits need deadlines of at least 10 s.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), the search tests, the relevance gate and the size-report tests.
- Add your own `runner/slices.yaml` entry and placement rule, and nothing else in that file. Run `npm test` in `tools/status-connector`.
- Write a decisions file: `docs/decisions/2026-11-15-X-index-bytes-1m.md`.
- Use the steward skill. Report `ready <sha>` to the runner when green.
