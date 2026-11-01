# X-file-clause-cold decisions

The cold broad `file:(…)` clause against §3's file-level row (150 ms), and cancellation for the percolator's file-clause path. Follow-ups from #278, #283 and X-search-files-perf. No schema, index format, cache budget or API change.

## Measured first (1M torrents, 29.5M file documents, release build, 4 cores)
- **The brief's 211 ms predates X-search-files-2.** On `main` before this slice, `search-bench --torrents 1000000 --only "broad file"` gave a cold p95 of 95.75 ms (p50 30.61, max 117.23; PASS at 150 ms) and a warm p95 of 8.03 ms.
- **Per clause, cold, alone** (local harness outside `bench/`, not committed: the file-set cache emptied before each of 40 runs, then `query` and `search` for 50 hits; p95 in ms):

  | clause | before | after |
  |---|---:|---:|
  | `file:(ext:mkv)` | 26.61 | 24.21 |
  | `file:(size:>1GB)` | 104.46 | 66.72 |
  | `file:(ext:mkv size:>1GB)` | 94.61 | 55.57 |
  | `file:(size:>100MB)` | 127.75 | 62.90 |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 68.74 | 36.79 |
  | 4 different `size:>` clauses at once, each cold | 200.02 | 180.83 |

- **First touch** (OS page cache dropped with `drop_caches`, a fresh process, one query per clause): `size:>1GB` 109–135 ms before, 68–77 ms after; the other clauses moved by the same proportion. Reading the file index from disk adds 0–30 ms, so the cost is the scan itself, not I/O.
- **Where the time went:**
  - The file scan (`compile`) is 85 of the 91 ms for `size:>1GB`. The join to torrents and the page read take the rest.
  - The scan itself is tight. A hand-rolled block read of the `size` and `torrent` columns took the same 115 ms, single-threaded over all 29.5M documents, as Tantivy's range doc set, about 4 ns a document, mostly bit-unpacking `size`. A narrower or hand-rolled scan buys nothing.
  - **The parallel scan was unbalanced.** Merging left one segment with 17.7M of the 29.5M file documents. Segments were the unit of parallel work, so that one took 78 ms on one thread while the other five (2M each) took 8 ms each on the rest. Four threads ran no faster than two (76 vs 79 ms).

## The fix: big segments scan in pieces (`crates/search/src/files.rs`)
- **A segment with more than 2M file documents (`PIECE_DOCS`) is cut into up to four doc ranges (`MAX_PIECES`, the file index's search threads)**, each scanned as its own task on the executor: the scorer seeks to the range's start and stops at its end. A segment's pieces gather into the same form (bitset or list, chosen by the segment-wide size hint), and their bitsets are OR'd into one part per segment. The cache, its keys, the parts kept and the 8 MiB budget are unchanged.
- **Transient memory stays bounded:** at most four gathers a segment, each at most the segment's id span in bits (at 10M torrents, about 1.25 MiB), up from one. The cap is why pieces are four at most, not a fixed size.
- **Cancellation and errors are unchanged:** every piece checks the token and the sibling-failure flag as a segment did, and nothing is cached unless every piece of every missed segment is whole.
- Tests: `pieces_cover_a_segment_in_order`, and `a_scan_in_pieces_finds_what_a_whole_scan_does`. The second scans in pieces of 1, 97 and 300 documents, ending inside a torrent's files, with a deletion, for dense, sparse and all-matching clauses, and checks the sets equal the whole-segment scan's.
- **Not done: warming the cache after open.** With the scan at 67 ms p95 or less for every broad clause, warming isn't needed for the target. It also can't cover the `size:` thresholds people type, since each threshold has its own key.

## Percolator file clauses stop under the run's token (`crates/search/src/percolate.rs`)
- **A search that may stop inside its scan (stoppable, or not the run's first) now compiles with `compile_cancelled` under the run's token,** so its `file:(…)` clauses' file-index scan stops at the job's budget (`PercolatorConfig::budget`) or on a cancel. A search stopped there is reported as not run and keeps its watermark, as a search stopped in its torrent scan already was.
- **The run's first search stays whole,** in its file scan too, so a run that starts one finishes one. Otherwise a saved search whose file scan takes longer than the budget would never move its watermark.
- Tests (each fails without the change): `a_cancelled_file_scan_stops_the_run`, where a stoppable search's file scan cancels partway, stops within `CHECK_EVERY` files and caches nothing; and `an_over_budget_file_scan_stops_promptly`, where a second search's file scan, about 20 s of work, stops at a 200 ms budget. That test waits on a channel with a 10 s deadline, with no fixed sleep.
- **Left alone:** the percolator job's token (`crates/lodestar/src/percolator.rs`) is a fresh one with a budget, not tied to shutdown. Tying it to shutdown is a `crates/lodestar` change, outside this slice.

## 10M and concurrency: a proposal, not built
- **Not measured at 10M:** the 1M corpus took 17 minutes and 1.8 GB here, and 10M would pass this session's disk allowance. Projected from 1M: the scan is linear in file documents (about 295M at 10M) and now spreads over four threads, so a cold `size:>1GB` is about 600 ms. That misses §3 on four cores. Text-only clauses (`ext:`, names) read postings and stay well under that, about 250 ms for `ext:mkv` by the same projection, still a miss.
- **Concurrent cold scans are CPU-bound:** four different cold size clauses need about 145 ms of CPU each on four cores, so 181 ms is near the floor at 1M with the current format.
- **Proposal for the runner (an index format change, so the owner sees it):** index a size bucket in the file index, a `u64` field holding `floor(log2(size) * 4)` (about 160 terms), indexed and not fast. A `size:` range becomes the union of the postings of the buckets it covers whole, plus the fast-field check for the two edge buckets.
  - **Space:** one Basic posting a file, about 1–1.5 bytes compressed, so roughly 30–45 MB at 1M (29.5M files; the file index is 668 MB today) and 300–450 MB at 10M.
  - **Rebuild path:** bump `SCHEMA_VERSION`, which names the index directory (`v{SCHEMA_VERSION}-t{TOKENIZER_VERSION}`). A new index is rebuilt from the store while the old one serves read-only (`Compat::ReadOnly` in `crates/search/src/index.rs`). Its file clauses keep working during the rebuild, because a field the old index lacks resolves to `ABSENT` by name.
  - It narrows the size scan to the matching files, so `size:>1GB` (8% of files) reads about a tenth of what it reads now.
  - It isn't built here. Measure it at 10M first, with the concurrent case.

## The gate line to add after G-m5a merges
In `bench/src/bin/search-bench.rs`, `file_sets_path`: add `file:(size:>1GB)` and `file:(ext:mkv size:>1GB)` to `broad` (the mix has no single-file AND of extension and size today). Then add one class:

```rust
("broad file (cold, 4 at once)", &mut cold4, None), // reported, not gated, until the size index is decided
```

`cold4` times four different cold `file:(size:>NMB)` clauses run at once on scoped threads, with the cache emptied before each round. The existing `("broad file (cold)", &mut cold, Some(FILE_P95))` stays as it is. CI's `search-bench -- --torrents 200000 --gate` line needs no change.
