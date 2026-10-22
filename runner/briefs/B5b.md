# B5b: Background index rebuild into a new directory, with atomic swap
**Model:** Opus (core). **Branch:** `slice/B5b`. **Spec:** architecture §8 "Index evolution"; the §16 M2/M3 index decisions (each index keeps its own watermark in its commit payload, the file index lives in `files/`, and `reindex_log` is followed per index).

## Goal
A `SCHEMA_VERSION` or `TOKENIZER_VERSION` change must never mean downtime or an empty index. Today `index_dir(root)` returns `root/v{SCHEMA}-t{TOKENIZER}`, so a bump just opens an empty directory. Build the rebuild-and-swap so the M5 facets bump (B6) and the M7 bump can land safely.

## Do (inside `crates/search` only)
1. **`rebuild.rs`, the rebuild:**
   - Given the index root, find the newest *complete* older-version directory and keep serving from it.
   - Meanwhile build the current version into `vN-tM.building/` from the store, using the existing `Indexer::catch_up` with batches, the same watermark rules and both indexes (file index included).
   - Then mark it complete: write a marker file after the final commit, then rename atomically (`.building` → final).
   - Restartable: an interrupted `.building` directory resumes from its own watermarks. It doesn't start over.
2. **Swap:**
   - A serving handle (`ArcSwap`, or an `RwLock<SearchIndex>` behind a small `Serving` type) lets readers switch to the new index between queries.
   - The live indexer follows the new index after the swap, and the old one stops receiving writes.
   - Old directories are deleted after the swap, with retry on failure, since Windows can't delete mapped files (§11).
3. **Progress:** `RebuildProgress { phase, done, total, started_at }`, readable from the serving handle for the "Index rebuilding" banner (§10.4) and Insights.
4. **API for the binary:** `Serving::open_or_rebuild(root, store_path, config)`. Don't wire it into `crates/lodestar` here (B5a does that). Keep `SearchIndex::open` working for existing callers.

## Concurrency and correctness tests
- Rebuild while searches run against the old index: no errors, and no torrent is missing from either answer.
- Crash mid-build (drop the builder), then resume: the final doc count equals the store count.
- After the swap, the live indexer adds new torrents to the new index only.
- Two concurrent `open_or_rebuild` calls for one root: an exclusive lock file under the index root makes the second one fail cleanly.
- `reindex_log` entries written during the rebuild are applied before the swap or after it, and never lost.

## Don't
- Bump `SCHEMA_VERSION`. Tests should use a test-only version override (a constructor parameter), not the constant.

## Done when
- fmt, clippy and test pass.
- The search-latency gate is unchanged (`cargo run --release -p lodestar-bench --bin search-bench -- --torrents 200000 --gate`).
- Write a short §16 "M5 decisions (index rebuild)" entry in `docs/architecture.md`, appended at the end of §16.
