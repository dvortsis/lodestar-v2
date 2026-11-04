# L4-followups decisions

The three follow-ups the 2026-10-30 L4 landing review raised (runner commit 4531e3d9): rollback and the live deletion journal, the file-clause cache at scale, and cancel tokens outside `/search`. The audit found the first two, and all of the third but compare, already on `main` from later slices. This PR records the audit with fresh numbers and adds the one missing token. No schema, index format, API type or budget change.

## Audit (main at de00b6c)
- **Rollback applies the live deletion journal: already done** (F-rollback-journal #241, then F-rollback-holds; `2026-10-30-F-rollback-journal.md`, `2026-10-30-F-rollback-holds.md`).
  - `crates/lodestar/src/cli/rollback.rs:396` (`purge_staged`) runs `lodestar_store::snapshots::purge_for_restore` (`crates/store/src/snapshots.rs:1616`) on the staged copy before the rename that makes it the store. It applies the whole journal and every hold, whenever recorded, and carries the holds and lists (`snapshots.rs:1110-1111`).
  - Tests: `a_rollback_never_brings_back_a_deleted_torrent` (`rollback.rs:539`: deleted after the snapshot, rolled back, still gone) and `a_rollback_keeps_the_holds_recorded_since` (`:558`).
- **File-cache budget: already capped and measured** (X-search-files-perf, `2026-10-31-X-search-files-perf.md`).
  - The cap: `CACHE_BYTES` 8 MiB and `CACHE_ENTRIES` 4,096 (`crates/search/src/files.rs:623-625`). It's enforced at insert: an entry over the budget bypasses the cache (`:746`), and least-recently-used entries go until both limits hold (`:755`). Tests in `files.rs` `cache_regressions`.
  - `search-bench` already has the "broad file clauses" table.
- **Cancel tokens:**
  - **Suggest: done.** Its counts compile under the request's token (`crates/search/src/suggest/mod.rs:288`), which `GET /suggest` ties to the client and its 2 s budget. Test `a_file_scan_cancelled_partway_stops_suggest_and_caches_nothing` (`:889`).
  - **Percolate's file clauses: done.** They compile with `compile_cancelled` under the run's token (`crates/search/src/percolate.rs:158-171`). Tests at `:697` and `:717`. A run's first search stays whole unless it's `stoppable` (live queries are). That's the queue's forward-progress guarantee, not a gap.
  - **Filters: done.** The only file-clause caller is the classification re-queue, stopped by the server's shutdown token (`crates/api/src/filters.rs:2523-2540`, bea8d2b1). The impact preview doesn't search. A request deadline there stays off on purpose (X-search-files-perf: it would leave Is-adult matches unmarked).
  - **Compare: was missing.** `other_versions` (`crates/api/src/compare.rs`) ran its candidate search and up to 1,000 per-candidate store lookups with no token. Its query never holds a `file:(…)` clause (`candidates_query`), so it never reached the file index, but a client that closed the drawer didn't stop it. Fixed below.

## Compare: the siblings search stops when its request is abandoned
- **`GET /torrents/{hash}/siblings` carries a request token, as `/suggest` does** (ownership widened to `crates/api/src/compare.rs` by the runner, 2026-11-04). A drop guard in the handler cancels it when axum drops the future (the client disconnected). `other_versions` compiles under it (`query_cancelled`, so a file clause added to `candidates_query` later stays stoppable), checks it before the search, and checks it before each candidate's lookups, the bulk of the work. A cancelled request ends with `503 search_cancelled`, which only a client that's gone sees.
- **No budget.** The candidate set is bounded (`SIBLING_CANDIDATES`); a deadline would only return a partial Related list.
- **`POST /compare` is unchanged:** it searches nothing, and reads at most five torrents' rows and file trees.
- **Tests** (`compare.rs`):
  - `other_versions_are_found_under_a_live_token`;
  - `a_cancel_during_the_candidates_stops_at_the_next` (a hook cancels at the first candidate; exactly one is visited, then `search_cancelled`), which fails with the per-candidate check removed;
  - `an_abandoned_request_stops`: the drop guard fires while the worker waits on the shared reader, and the worker ends with `search_cancelled`, waited for with a 30 s deadline and no fixed sleep.
  - The handler's guard wiring itself is the same two lines `/suggest` uses; an end-to-end disconnect test would go in `crates/api/tests/compare.rs`, outside this slice.

## The file-set cache at 1M, after SB-6
SB-6 (#415) added `size_bucket`, `category` and `pieces_root` to the file index. The cache holds torrent-id sets, so its charge shouldn't change; re-measured to confirm. `search-bench --torrents 1000000 --only "broad file"` (the bench's default size, the largest it has been run at), 5 segments, 4 cores:

| clause | torrents | ids as a list (MiB) | charged (MiB) |
|---|---:|---:|---:|
| `file:mkv` | 612,207 | 4.67 | 0.13 |
| `file:(ext:mkv)` | 612,207 | 4.67 | 0.13 |
| `file:(ext:srt)` | 98,501 | 0.75 | 0.13 |
| `file:(ext:flac,mp3)` | 144,985 | 1.11 | 0.13 |
| `file:(ext:jpg)` | 182,665 | 1.39 | 0.13 |
| `file:(size:>100MB)` | 722,058 | 5.51 | 0.13 |
| `file:(ext:mkv,mp4,avi -path:sample)` | 633,588 | 4.83 | 0.13 |
| `file:(size:>1GB)` | 588,814 | 4.49 | 0.13 |
| `file:(ext:mkv size:>1GB)` | 517,077 | 3.94 | 0.13 |

- **Warm mix** (half alone, half under a common word): 108 sets held in 1.14 MiB of the 8 MiB, 100% of segment lookups hit. Warm p95 8.42 ms (gate 30 ms); cold p95 51.85 ms (gate 150 ms); cold with 4 at once p95 235 ms (ungated).
- **Against X-search-files-perf at 1M** (0.12 MiB a clause, 63 sets in 0.87 MiB): the same per clause within rounding, with the mix now nine clauses. SB-6 changed nothing here.
- **Beyond 1M, from the charge's shape (not run: storing and indexing 10M takes hours here):** a dense set costs one bit for each id in its span, so a broad clause is charged about 0.13 MiB per million torrents. At 10M the nine-clause mix would want about 12 MiB. It doesn't fit: the cap holds memory at 8 MiB, and the least recently used sets are rescanned (the cold path, about 52 ms p95 at 1M). **No cap change proposed:** the cap is the memory guarantee (§3, `2026-10-30-L3-L5-landing.md`). Whether 10M needs a larger default goes with the low-resource and cache-size settings, not here.
