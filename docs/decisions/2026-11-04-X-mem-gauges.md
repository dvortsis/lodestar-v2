# X-mem-gauges decisions

Runner brief X-mem-gauges, 2026-11-04 (auto-approved under owner §2.33; Track R). Background: the owner's soak memory review, `docs/code-review/2026-11-04-soak-memory-review.md` §2 and §4.A. Soak r4's worker had RssAnon 492 MiB, 212 MiB of it in transparent huge pages, and nothing told the Rust heap (mimalloc) apart from the C heaps (SQLite, zstd, through musl's `malloc`). Metrics only: delta class B/C. No change to the crawler lifecycle, the sampler's cadence, the endpoint policy, the allocator's configuration, THP or commit cadence.

## What's measured, and from where
- **The process that holds the corpus.** On Unix, `serve` is the supervisor (`update::supervisor::run`), which never starts `Services` or an HTTP listener. The `--child` worker runs `Services::start` (`main.rs` `serve_as`), which builds the storage sampler and the `/metrics` router in that same process. On Windows and under `serve --fixture` there is one process. So every gauge here is read in the worker, by the worker, and served by the worker's `/metrics`. Nothing crosses a process boundary.
- **Path:** `services.rs` builds `storage::Gauges` and spawns `storage::spawn` → every 60 s `storage::sample` (on a blocking thread) calls `memstats::sample` → `/metrics` (`http.rs` `prometheus`) → `metrics::render_storage` → `render_memory`, from the cached atomics.
- **mimalloc's committed bytes** come from `mi_stats_get_json`, through the `mimalloc` crate's safe `MiMalloc::stats_json()` (the `extended` feature), not a direct `mi_process_info` call. The JSON's `process.commit_current` and `commit_peak` are exactly what `mi_process_info` returns: mimalloc v3 fills it from the main sub-process's `stats.committed`, which it counts in every build (OS-level stats don't depend on `MI_STAT`).
  - **Why not `mi_process_info` directly:** it's an `unsafe extern` function, and the workspace forbids `unsafe` (`unsafe_code = "forbid"`). The safe wrapper needs no exception.
  - **Cost:** the call copies mimalloc's statistics block, visits its heaps (one, by default) and formats about 15 KB of JSON, which it allocates through mimalloc and frees when the wrapper drops. Then `serde_json` parses it. Measured at 0.22 ms per call in the test profile (opt-level 1), 0.39 ms for the whole sample with both `/proc` reads. It runs once a minute on the sampler's blocking thread, never in a scrape. It takes mimalloc's own heap-list lock, no lock of the store, the writer or the index.
  - **New lock entry:** `extended` pulls in `cty` 0.2.2 (C type aliases for `libmimalloc-sys`). The brief approved the feature.
- **RssAnon, RssFile** from `/proc/self/status`, **AnonHugePages** from `/proc/self/smaps_rollup`. Linux only (`cfg(target_os = "linux")`); elsewhere they're absent.
  - **Parsing is strict:** the key must match exactly, appear once, and carry ASCII digits followed by `kB` and nothing else. A missing file, missing field, duplicate, other unit, sign, decimal or overflow on `× 1024` makes that gauge absent. It never panics and never guesses.
  - **Cost:** `status` is a small file. `smaps_rollup` makes the kernel walk every mapping, the memory-mapped index included, under the mm's read lock. That's why it runs on the 60 s sampler and never per scrape.
- **Absent means absent:** a gauge that can't be read is left out of `/metrics`, not rendered as 0, and each value is cleared on its own when it stops reading. The memory gauges don't count in `storage_sample_errors_total` and don't hold back `storage_sampled_seconds`, so the storage sizes' meaning is unchanged.
- **`search_segments{index="main"|"files"}`** is read at scrape, like `search_docs` and `search_mmap_cache_entries`, not sampled: `SearchIndex::segment_counts` loads each reader's current searcher (an `ArcSwap` load and an `Arc` clone, no lock). Sampling it would mean handing the sampler the serving index, which a rebuild swaps.
- **No merges-completed counter.** Tantivy 0.27 runs merges on its own threads and exposes no completed-merge count. Counting them would mean wrapping the merge policy, which changes the indexer. The segment count already shows whether merges keep up.

## The SQLite gauges, under owner-decisions §2.35
- **`lodestar_mem_sqlite_bytes` and `lodestar_mem_sqlite_highwater_bytes`** read `sqlite3_memory_used()` and `sqlite3_memory_highwater(0)` from `crates/store/src/memstats.rs`. The bundled build keeps `SQLITE_DEFAULT_MEMSTATUS` on, so the counters are live.
- **The `unsafe` exception (owner, 2026-11-04, §2.35, PR #428):** rusqlite 0.40 wraps neither function, and the workspace forbids `unsafe` (§1b.5). In this PR:
  - `lodestar-store` alone stops inheriting the workspace lints. It copies them with `unsafe_code = "deny"` instead of `"forbid"`, and every other lint stays the same. A comment in `crates/store/Cargo.toml` says to keep the copy in step.
  - `#[allow(unsafe_code)]` sits on exactly two statements, one call each, with a `// SAFETY:` comment. Both functions take no pointer (the high-water read takes an integer flag, 0, which only reads) and read a global counter under SQLite's own mutex. SQLite documents them as callable from any thread at any time.
  - Nothing else in the crate may use `unsafe`. `deny` still rejects any other block at compile time.
  - `/security-review` was run; the PR description has its result.
- **The history, for the runner.** The first draft asked to relax `forbid` before any approval existed. The session's safety check refused it, and the runner first decided to ship without SQLite (option b). It then took the exception to the owner, who approved it, and once #428 put §2.35 on `main` the half went in.
- **Cost:** each read takes SQLite's memory-statistics mutex for one load. Every SQLite allocation takes that mutex already, and the reads hold it for nanoseconds, once a minute. No connection, page or lock of ours is touched.
- **"Other heap"** is `RssAnon − mimalloc committed − SQLite`, labelled "other heap (zstd, other C code)". Evidence from binaries without the SQLite gauges falls back to `RssAnon − mimalloc committed`, labelled "other heap (includes SQLite, zstd, C code)", so nobody reads it as SQLite-free. The report records which in `other_heap_excludes_sqlite`.

## "Other heap" is an approximation
- **It's no bound either way.** Besides zstd and other C allocations, `RssAnon − mimalloc committed − SQLite` holds:
  - thread stacks and every other anonymous mapping;
  - transparent huge pages that fault whole 2 MiB pages into mimalloc's arenas beyond the 64 KiB slices mimalloc counts as committed. r4 had 212 MiB of `AnonHugePages`, so this matters.
- **It can also read too small:** mimalloc counts a slice as committed when it's first allocated, whether or not its pages are resident.
- **So** `docs/metrics.md`, the module docs and the analyzer say "approximates", and the report puts a line beside the table telling you to read it with the `AnonHugePages` row (fresh self-review).
- **SQLite is subtracted from every window point or none,** so a window where it appears part-way doesn't put a step into the slope.

## Shared edits outside the brief's own paths
- `crates/lodestar/src/main.rs`: the one `mod memstats;` line. The allocator and THP lines, which X-thp-off owns, are untouched.
- `crates/lodestar/src/http.rs`: one argument, `index.segment_counts()`, passed to `render_search`.
- `crates/lodestar/Cargo.toml` (the `extended` feature) and `Cargo.lock` (`cty`).
- `crates/store/src/lib.rs` (`pub mod memstats;`) and `crates/store/Cargo.toml` (the lints block, owned under §2.35).
- `runner/dashboard.yaml`: the placement line. `main` added the same line meanwhile, and the merge kept `main`'s.

## Soak tooling
- **Collector (`docs/soak/tools/linux/collect.py`):** the seven memory gauges go into `STORAGE` (kept, never required, unlabelled). `search_segments` goes into `LABELLED`, so only `index="main"` and `index="files"` survive the sanitizer.
- **Analyzer (`analyze_cr0.py`):** a new informational `memory_attribution` section. Over the gate's window, baseline hours 12–24, it gives each gauge's first, last and max (MiB) and its OLS slope (MiB/h), plus the same for "other heap" and the per-index segment count. A gauge in no baseline snapshot is `absent`. One present but with no sample in the window is `no_window_samples`. A process restart inside the window refuses the section, as it refuses the gate's fit. The memory gate itself is unchanged.

## UTC defaults (owner-decisions §2.35, PR #428)
- **The soak collectors' recorded `user_timezone` now defaults to `UTC`:** `linux/collect.py --user-timezone` and `cr0-soak.ps1`'s `run.json` field. The option stays. Records give times in UTC only (the owner's new rule, which the runner passed on).
