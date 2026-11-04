# X-mem-gauges: memory attribution gauges (metrics only)
**Model:** Opus, because of the FFI and `unsafe`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33. Track R priority: this rides rc.4 if soak r4 fails, so the next verdict can say where the memory is.

## Why
The owner's soak review (PR #425, `docs/code-review/2026-11-04-soak-memory-review.md` §2 and §4.A) and the runner's evidence:
- r4's worker has RssAnon 492 MiB, of which AnonHugePages is 212 MiB.
- X-merge-policy measured the indexing path at about 2.7 KiB/torrent, against about 19 KiB/torrent in r4.
- SQLite (bundled), C zstd (store codec and tantivy's doc store) and any other C code allocate through musl's mallocng, not mimalloc. mimalloc is the Rust global allocator without `override`.
- Today nothing tells those heaps apart. `anon − mimalloc committed − SQLite` isolates the rest.

## Do
1. **Gauges**, sampled by the existing 60 s gauge task and read from cache by `/metrics`, the same pattern as the storage and cache gauges (`docs/decisions/2026-10-30-CR-0-acceptance.md`):
   - `lodestar_mem_sqlite_bytes` and `lodestar_mem_sqlite_highwater_bytes`: `sqlite3_memory_used()` and `sqlite3_memory_highwater(0)`, through rusqlite's `ffi`. Put the call in a new `crates/store/src/memstats.rs`, exposed from `lodestar-store`.
   - `lodestar_mem_mimalloc_committed_bytes`, plus `_peak_committed_bytes` if it's cheap: from `mi_process_info` (libmimalloc-sys `extended` feature). Check the cost; it must be well under 1 ms per call.
   - `lodestar_mem_rss_anon_bytes`, `lodestar_mem_rss_file_bytes` and `lodestar_mem_anon_huge_bytes`, self-read from `/proc/self/status` (RssAnon, RssFile) and `/proc/self/smaps_rollup` (AnonHugePages). Linux only; absent elsewhere. Parse defensively: a missing or odd field means the gauge is absent, never a panic.
   - `lodestar_search_segments{index="main|files"}`, plus a merges-completed counter per index if tantivy exposes one cheaply. Use a getter in `crates/search/src/index.rs` next to `mmap_cache_entries`. `segments()` already exists at :1128.
2. **The right process.** The values must come from the process holding the corpus: the `--child` worker under the supervisor, or the single process when there is none. Find how the worker's metrics reach `/metrics` today, and name the path in the PR. If the supervisor renders its own process's numbers, these gauges must be read in the worker.
3. **Rows in `docs/metrics.md`** for every new metric, in its existing format.
4. **Collector and analyzer** (`docs/soak/tools/**`), as CR-COL-4 did for the mmap gauge:
   - add the metrics to `collect.py`'s `KEEP` and the sanitizer's safe labels;
   - `analyze_cr0.py` reports each as a level and as a slope per hour over the window, and reports `anon − mimalloc − sqlite` as "other heap";
   - older snapshots without the metrics still analyze, reported as absent;
   - extend `test_analyze_cr0.py` with an end-to-end fixture and a missing-metrics case.

## Must not
- **Touch the crawler lifecycle, the sampler, the endpoint policy, the allocator's configuration, THP or commit cadence.** This slice must stay metrics only (delta class B/C), so it doesn't by itself force another soak. X-thp-off owns the allocator and THP startup lines.
- Call anything that allocates heavily or takes a lock on the search or write path. `mi_process_info` and the SQLite calls are global reads; say what each costs.

## Own
- New files: `crates/store/src/memstats.rs` and `crates/lodestar/src/memstats.rs`.
- The gauge wiring in `crates/lodestar/src/metrics.rs`, and its 60 s sampler task, wherever that lives (name the file).
- The getter lines in `crates/search/src/index.rs`.
- `docs/metrics.md` rows and `docs/soak/tools/**`.
- Tests, and the decisions file `docs/decisions/2026-11-04-X-mem-gauges.md`.

**Shared edits, to be named in the PR:**
- the `mod memstats;` line and its re-export in `crates/store/src/lib.rs`;
- the `features = ["extended"]` line in `crates/lodestar/Cargo.toml`, or a `libmimalloc-sys` dependency, and `Cargo.lock`. X-thp-off may also touch `crates/lodestar/Cargo.toml`. If that conflicts, whichever merges later brings `main` in.

## Tests
- Unit tests for the `/proc` parsers, including fixtures with missing fields, extra spaces, kB units and huge values.
- A test that each gauge renders and has its `docs/metrics.md` row (follow the existing pattern).
- On Linux, a test that the RssAnon gauge is non-zero and the SQLite gauge rises after opening a store and writing.
- The analyzer tests above. No fixed sleeps.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the soak-tools Python tests pass, Windows and macOS build with the Linux-only gauges absent, and a fresh self-review is done.

Branch `slice/X-mem-gauges`. The PR is titled "X-mem-gauges: Memory attribution gauges for SQLite, mimalloc, RSS and segments". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
