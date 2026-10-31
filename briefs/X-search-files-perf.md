# X-search-files-perf: the file-ID cache at scale, cancellation for file clauses, #274's search nits
**Model:** Opus: search internals (caches, cancellation, concurrency). Runner brief, 2026-10-31. Carries the L4 review's open perf items (I4, P2, P3; state `L4-followups`) and two optional findings from #274. No schema or API change. No `/security-review` needed, unless you change how visibility enters a cache key.

## Why
- **I4.** The retained file-ID cache (`crates/search/src/files.rs`, `CACHE_BYTES` 8 MiB, 4,096 entries) was sized on a small corpus. A broad file clause (`file:mkv`, `file:(*.srt)`) at millions of torrents may produce sets larger than the budget, which are then never cached, or sets that churn the cache. Nobody has measured it.
- **P2.** Eviction is O(n) per evicted entry: `min_by_key` over the whole map, inside the lock, in a loop. At 4,096 entries under churn, that's a scan per insert while every file-clause search waits on the mutex.
- **P3.** `/search` file clauses honour a `CancelToken`. Suggest, compare, the filters preview and the percolator call the same file-clause paths without a real token, or with one that never cancels, so a slow broad clause there can't be stopped.
- **#274 optional 2.** `crates/api/src/search.rs` (~l.459): kept recoveries are keyed by `fingerprint(ast, visibility, sort)` without `limit` (or `tz` with `time:`), so two live pages that differ only by limit overwrite one slot on every alternating request. It's correct, because the pointer check forces a recount, but the recounts are wasted work.
- **#274 optional 3.** `bench/src/bin/search-bench.rs` (~l.854): the row label says "p95 < 20 ms" but the check is `<=`.

## Read first
- `docs/decisions/2026-10-30-L3-L5-landing.md` (the file-cache budget, as an 8 MiB §2.19 flag), `-X-polish-1031.md` (the recovery cache), `docs/architecture.md` §3 (search latency budgets) and §7, and `runner/standards.md`.
- `crates/search/src/files.rs` (FileSetCache, enumeration, `CancelToken` use), `crates/search/src/cancel.rs`, and the callers of the file-clause path in suggest, compare, `crates/api/src/filters.rs` and the percolator (`grep -rn "files::\|file_set\|FileSetCache" crates`).
- `bench/` (the search-latency gates and how a new class is added; X-polish-1031 added `recovery_path`).

## Do
1. **Measure first (I4).** Add a bench class with broad file clauses over the bench's large seeded corpus: the hit rate, set sizes against the budget, and p95 with a cold and a warm cache. Report the numbers in the decisions file. Then choose, from the data:
   - keep 8 MiB;
   - store sets more compactly (sorted `u64` deltas, a bitset or roaring-style chunks per segment, or `shrink_to_fit` before charging);
   - or change the budget. **Changing the 8 MiB number is the runner's call under §2.26:** propose it with numbers in the PR, and don't treat it as settled.
   Gate the new class in the bench's `ok &=` chain at the existing search p95 budget.
2. **O(log n) or amortised O(1) eviction (P2):** an LRU list, a `BTreeMap<used, key>` index, or a clock sweep. Keep the current accounting (`retained_bytes` charges map capacity), and keep the existing budget tests passing. Add a test that fills past `CACHE_ENTRIES` and checks LRU order.
3. **Cancellation everywhere (P3):** give suggest, compare and the filters preview's file-clause paths a real `CancelToken`, tied to their request deadline or the caller's shutdown, as `/search` does. A cancelled scan builds nothing and caches nothing; the tests in `files.rs` already show the pattern. List each caller and its deadline in the decisions file. **The percolator is out of scope:** B-import-server-1 may hook the watermark in `crates/lodestar/src/`, so note the percolator's file-clause path as a follow-up instead.
4. **#274 nits:** add `limit` (and the tz offset where `time:` is in the query) to the recovery key, with a test that alternating limits reuse their own entries; print "≤" in the bench label.
5. **Tests:** condition waits only, no fixed sleeps, deadlines ≥ 10 s.
6. **Decisions file:** `docs/decisions/<date>-X-search-files-perf.md`.

## Own
`crates/search/src/**` (files.rs, cancel.rs, and the file-clause callers inside search), `crates/api/src/{search.rs,filters.rs}` (the token and key only) `bench/**`, their tests, and your decisions file. **Don't touch** `crates/pipeline/**`, `crates/store/src/import.rs` or anything in `crates/lodestar/src/**`: B-import-server-1 is working there now.

## Size
≲ 900 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the bench gates, and the openapi drift check all pass.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the bench numbers. Never merge. Work frugally. Push once per round of fixes.
