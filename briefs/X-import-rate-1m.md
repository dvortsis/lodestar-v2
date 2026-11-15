# X-import-rate-1m: import throughput at 1M scale

**Model:** Opus (store, import pipeline, concurrency). **Branch:** `slice/X-import-rate-1m`. **PR title:** `X-import-rate-1m: <title>`.

## Why
X-test-vm's 1M import (Magnetico 1M, deterministic, main@92b181bd, 2026-11-14) ran end to end at **990 torrents/s**, against the IMP-4b offline pass line of **2,000/s** (`docs/decisions/2026-10-30-IMP-4*.md`). The write phase ran at 1,541/s; the smoke test at 100k ran at 3,061/s. So throughput falls with scale. The pass line is not lowered (owner number).

## Steps
1. **Reproduce and profile at 1M** with a synthetic Magnetico-shaped source (the existing import bench or generator; extend it if needed). Break the time down by phase: read, filter/judge, store write (SQLite page cache, index b-trees, WAL checkpoints), search index (commits, merges), completion. Show where the per-torrent cost grows between 100k and 1M. Record the machine, cores and disk.
2. **Fix the top contributors** inside the importer and store paths, for example:
   - batch sizes and transaction size;
   - deferred secondary-index builds or `ANALYZE`;
   - checkpoint cadence;
   - prepared-statement reuse;
   - parallelism that holds the writer's single-writer rule;
   - a commit cadence for the import's own index writer.

   Keep every IMP decision: per-reason counts, refusal and filter behaviour, private-candidate handling, resumability and cancellation, and the online import's 1,000/s line.
3. **Search index files belong to X-index-bytes-1m** (running now in another slot), including the schema and `crates/search/src/index.rs` layout. If the fix needs a change there, tell the runner first; don't edit those files.
4. **Gate:** report the 1M end-to-end figure and the 100k figure before and after, on the same machine. If CI's import bench runs at a smaller scale, say so in the decisions file and keep its pass line.

## Rules
- Untrusted input: the import source. Keep every parser and bound as it is.
- No fixed sleeps in tests; condition waits need deadlines of at least 10 s.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), the import and store tests, and the parity test.
- Add your own `runner/slices.yaml` entry and placement rule, and nothing else in that file. Run `npm test` in `tools/status-connector`.
- Write a decisions file: `docs/decisions/2026-11-15-X-import-rate-1m.md`.
- Use the steward skill. Report `ready <sha>` to the runner.
