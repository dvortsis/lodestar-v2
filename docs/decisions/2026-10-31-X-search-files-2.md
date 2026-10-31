# X-search-files-2 decisions

The cold broad `file:(…)` clause against §3's "File-level query joined to torrents < 150 ms" row, plus two optional findings from #278. No schema, index format or API change.

## The cold path is gated (§3)
- **`FILE_P95 = 150 ms`** in `search-bench` (§3's file-level row). The "broad file (cold)" class (the cache emptied before each query) is now gated with it; "broad file (warm)" stays at `KEYWORD_P95` (30 ms). At the CI size the cold class passes with a wide margin, so the gate catches regressions rather than noise.

## Where the cold time went (1M torrents, 5 file-index segments, release build, 4 cores)
- **Measured first,** with temporary timers (not committed) around each step of `files::torrent_ids` and the torrent-side join. Per cold query, on average: 85 ms in all, of which **72 ms scanning the file index** (about 4.8M matching file documents a query), 5 ms sorting and deduplicating ids, 0.7 ms building the sets and 8 ms in the join to torrents (each torrent segment's id column against the union bitset).
- **The scan dominates, and it was serial:** segments were scanned one after another, one document at a time through the boxed scorer. The worst clause, `file:(size:>100MB)`, is a fast-field range over every file document, since `size` is a fast field without an index: 262 ms of scan on its own. Ids also needed a real sort: in merged segments they come as concatenated increasing runs, not one sorted run (up to 12 ms a clause).

## The fixes (`crates/search/src/files.rs`)
- **Segments scan in parallel** on the file index's own search executor (`prepare` already gives it up to `SEARCH_THREADS`, as for the torrent index, whose searches in `count.rs` and `lease.rs` already map over segments the same way). The cache is checked for every segment first, and the weight is only built if one misses. Sets are cached only once every segment's scan is whole, so a scan cut short caches nothing, not even the segments that finished (test `a_cancelled_scan_over_several_segments_caches_no_segment`).
- **Documents come 64 at a time** (`fill_buffer`) with their torrent ids read in one `first_vals` call, instead of one virtual `advance` and one column read a document. Cancellation is still checked at least every 2,048 documents.
- **Dense clauses collect straight into a bitset** over the segment's id range (the `torrent` column's min and max), when the scorer's size hint (matching files, an estimate) is at least one match per 64 ids of that range: no sort, no dedup. The bitset is transient, at most 8 bytes per expected matching file, like the old list before dedup, and with segments in parallel up to `SEARCH_THREADS` (4) exist at once. Sparse clauses keep the list. The sets kept, and so the cache, are unchanged. `IdPart::from_bits` then gives exactly the part `from_sorted` gives for those ids (the proptest checks both).
- **Not done:** skipping the rest of a torrent's files once one matches (it needs a forward search over the id column for every match and helps only packs), and a hand-rolled scan for a bare `size:` range. Neither is needed for the target.

## Before and after
- **1M torrents** (cold, 300 queries, half under a common word):

  | | p50 | p95 | p99 | max |
  |---|---:|---:|---:|---:|
  | before | 68.57 | 194.62 | 229.36 | 248.22 |
  | after | 30.63 | 101.85 | 132.81 | 158.05 |

  Per clause, cold, alone (ms, before → after): `file:mkv` 124.6 → 36.3, `file:(ext:mkv)` 59.4 → 26.5, `file:(ext:srt)` 16.2 → 8.4, `file:(ext:flac,mp3)` 69.4 → 35.8, `file:(ext:jpg)` 79.6 → 38.0, `file:(size:>100MB)` 287.1 → 126.5, `file:(ext:mkv,mp4,avi -path:sample)` 145.3 → 73.4. Warm p95 8.75 → 7.95 ms. Sets and their charged bytes are unchanged (0.12 MiB a clause).
- **200,000 torrents (the CI size)**, cold:

  | | p50 | p95 | p99 | max |
  |---|---:|---:|---:|---:|
  | before | 15.88 | 41.08 | 56.66 | 64.19 |
  | after | 6.89 | 18.85 | 22.32 | 28.04 |

  Warm p95 4.95 → 5.18 ms (noise). The full `search-bench --torrents 200000 --gate` passes.
- **At 10M** the scan grows with the file documents, so the cold p95 will pass 150 ms again on four cores; the fixes buy a constant factor (about 2×), not a different curve. The floor is the size range, which reads every file document's `size`. Indexing `size` in the file index would let it read postings instead, but that is an index format change, out of this slice's scope: **proposed for the runner (§2.26)** as a follow-up to measure at the 10M bench, alongside the warm path, which already holds every broad clause's sets.

## #278 optional findings
- **`IdPart` proptest** (`id_part_props`): random sorted, deduplicated sets built from dense runs and lone sparse ids at any base, so they cross 64-bit word boundaries at every offset and come out in both forms. `contains` (on every id, its neighbours and far probes), `for_each`, `last`, `is_empty` and the `IdSet` union and membership equal the plain sorted list; the bitset and list gathers, in any order and with repeats, give the same part.
- **The recency index is charged three slots an entry, not two.** Measured with a counting allocator under this cache's pattern (append at the end, hits moved to the end, pop the first): the B-tree's nodes take 1.4 to 1.9 slots an entry in steady state, but up to 2.52 while it fills, because Rust's B-tree leaves can run half empty. Two slots didn't always cover it; three do. It costs 56 bytes more an entry, at most 0.22 MiB of the 8 MiB at the 4,096-entry cap. The existing budget tests pass unchanged.
