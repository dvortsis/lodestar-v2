# X-search-files-perf decisions

The file-set cache at scale (L4 review items I4 and P2), cancellation for `file:(…)` clauses outside `/search` (P3), and two optional findings from #274. No schema, index format or API type change.

## The file-set cache (I4): measured, then made compact
- **Measured first.** A new `search-bench` table, "broad file clauses", runs seven clauses that match a large share of the corpus (`file:mkv`, `file:(ext:mkv)`, `file:(ext:srt)`, `file:(ext:flac,mp3)`, `file:(ext:jpg)`, `file:(size:>100MB)`, `file:(ext:mkv,mp4,avi -path:sample)`). For each, it reports the torrents matched, what their ids take as a list (8 bytes each) and what the cache charges for them. Then it times the same mix (half alone, half under a common word) with the cache emptied before each query (cold, reported) and with all of them competing for it (warm, gated at `KEYWORD_P95`, 30 ms), and reports the warm loop's hit rate.
- **Before, at 200,000 torrents (the CI size):** one broad clause's sets were charged 1.50 MiB (`file:(ext:mkv)`, 122,211 torrents: 0.93 MiB of ids, plus `Vec` growth slack), and the seven together held 6.95 of the 8 MiB. At 1M torrents, one such clause needs about 7.5 MiB, so two broad clauses evict each other on every alternation, and at a few million torrents a single segment's set passes the budget and is never cached.
- **Decision: keep 8 MiB, store sets compactly.** Each file-index segment's set is now an `IdPart`: a sorted `u64` list (shrunk to fit before it's charged), or a bitset over the span from its least id to its greatest, whichever is smaller (`crates/search/src/files.rs`). Torrent ids are given out in order, so a broad clause's ids are dense: one bit an id of the span, against 64 bits an id in the list. A sparse set stays a list and never takes more than before. Membership, the union bitset and iteration answer the same for both (test `id_parts_take_the_smaller_form`).
- **After, at 200,000 torrents:** each broad clause is charged 0.03 MiB (about 50 times less); all 28 sets of the mix fit in 0.18 MiB with every segment lookup a hit. Warm p95 4.86 ms, cold p95 40.19 ms (ungated: it's the file-index scan itself, which the cache exists to skip).
- **After, at 1M torrents** (5 segments; the list column is what the ids alone would take before):

  | clause | torrents | ids as a list | charged now |
  |---|---:|---:|---:|
  | `file:(ext:mkv)` | 612,207 | 4.67 MiB | 0.12 MiB |
  | `file:(ext:srt)` | 98,501 | 0.75 MiB | 0.12 MiB |
  | `file:(ext:flac,mp3)` | 144,985 | 1.11 MiB | 0.12 MiB |
  | `file:(ext:jpg)` | 182,665 | 1.39 MiB | 0.12 MiB |
  | `file:(size:>100MB)` | 722,058 | 5.51 MiB | 0.12 MiB |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 633,588 | 4.83 MiB | 0.12 MiB |

  All 63 sets of the warm mix fit in 0.87 MiB, and every segment lookup hit. Warm p95 9.04 ms (gate 30 ms); cold p95 211 ms, ungated. A cold broad clause scans tens of millions of file documents, and that cost is what the cache saves. A set stays at one bit for each id in its span, so 8 MiB holds a broad clause's sets up to tens of millions of torrents.
- **No budget change is proposed.** The 8 MiB stays (decision `2026-10-30-L3-L5-landing.md`); with compact sets it holds every broad clause of the mix many times over.

## Eviction (P2)
- **Least recently used in O(log n):** beside the map, a `BTreeMap` from each entry's last use to its key. A hit moves its entry; eviction pops the first. It replaces the `min_by_key` scan over the whole map, inside the lock, for each entry evicted.
- **Accounting kept:** `retained_bytes` still charges the map's capacity; it now also charges the recency index (two key-sized slots an entry, for B-tree slack) and each key's string twice. The existing budget tests pass unchanged. Test: `past_the_entry_cap_the_least_recently_used_goes_first`, which fills past `CACHE_ENTRIES` and checks order and that the index stays in step with the map.
- **Hit and miss counters** (`SearchIndex::file_cache_hits`), for the bench's hit rate. Not exported to `/metrics`: that would touch `crates/lodestar/src/metrics.rs`, which another slice was editing.

## Cancellation for file clauses (P3)
`SearchIndex::query_cancelled` compiles a query under a token: its `file:(…)` clauses stop scanning the file index once the token stops, and a scan cut short builds and caches nothing (as `/search` already did through `Lease`). The callers:
- **Suggest** (`crates/search/src/suggest/mod.rs`): its counts compile under the request's token, which `GET /suggest` ties to the client (cancelled on disconnect) and to its 2 s budget (`crates/api/src/suggest.rs`). Test: `file_clauses_compile_under_the_request_token`.
- **Classification-rule re-queue** (`FiltersApi::requeue`, `crates/api/src/filters.rs`), the only file-clause caller in the filters API (the impact preview doesn't search): under a 10 s budget, `REQUEUE_FILE_BUDGET`. Past it, only the torrents beyond the watermark are marked and the response says `truncated`, as when `MAX_REQUEUE` cuts it short; the rule itself is already stored.
- **Compare** (`other_versions`, `crates/api/src/compare.rs`): unchanged. Its query is a title phrase plus year, season and episode ranges (`candidates_query`), never a `file:(…)` clause, so it never reaches the file index.
- **Follow-up: the percolator.** `percolate` (`crates/search/src/percolate.rs`) is given a token but compiles each saved query with the uncancellable `compile`, so a broad file clause in a saved search can't be stopped. Left alone here because B-import-server-1 may hook the watermark in `crates/lodestar/src/`; the fix is to call `compile_cancelled` with the token it already has.

## #274 optional findings
- **A kept recovery's key holds the page size, and the zone when the query has `time:`** (`recovery_key`, `crates/api/src/search.rs`), as the result-cache entry it's kept beside does. Two live pages that differ only by `limit` no longer overwrite one slot, so alternating between them counts once each. Test: `recovery_keys_hold_limit_and_the_zone_for_time`. `Node::has_time_of_day` is now public for it.
- **The bench's labels say "p95 ≤"**, matching the `<=` checks.
