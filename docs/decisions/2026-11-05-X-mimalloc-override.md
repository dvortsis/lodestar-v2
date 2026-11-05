# X-mimalloc-override decisions (one allocator for Rust and C in the release build)

Runner brief X-mimalloc-override, 2026-11-04 (an rc.4 candidate; auto-approved under owner §2.33, which names mimalloc's `override` feature as the no-`unsafe` route, and flagged to the owner under §2.19). Architecture §11 (the binary and its allocator). Follows `2026-11-04-X-thp-off.md` and `2026-11-04-X-mem-gauges.md`.

## Why
- **Soak r4:** the worker's anonymous memory grew about 173 MiB in 4 h, in base pages. About 310 MiB sat outside mimalloc's arena in about 3,200 small mappings, 1,843 of them 128-512 KiB.
- **The C allocator in the release build** (cargo-zigbuild, zig 0.17, static musl) is zig's `lib/c/malloc.zig` over `std.heap.SmpAllocator`. Its 64 KiB slabs are never unmapped, and an allocating thread checks only its own CPU slot's freelist and one neighbour's. SQLite and zstd run on many `spawn_blocking` threads, so freed C memory piles up where no other thread reuses it. The measurement below shows that pattern under threads.

## What changed
- **mimalloc's `override` feature, for musl Linux only** (`crates/lodestar/Cargo.toml`, a `cfg(all(target_os = "linux", target_env = "musl"))` dependency entry). mimalloc then defines `malloc`, `free`, `calloc`, `realloc` and the rest. In the static link they resolve to mimalloc's, so SQLite, zstd and ring allocate from the same arenas as Rust. No `unsafe`, no code change, and `Cargo.lock` is unchanged.
  - **Not Windows:** mimalloc can't override the CRT's `malloc` in a static link, and the MSI isn't where the growth is. **Not glibc:** glibc's malloc isn't the allocator r4 ran, and dev builds stay as they were. Both builds are unchanged.
  - **Both processes:** the supervisor and its `--child` worker are the same binary, so both run with it.
- **The release workflow checks it** (`.github/workflows/release.yml`, a step at the end of `package-linux`, both arches). The shipped binary has no symbols. So after the upload the step relinks only the `lodestar` crate with `-C strip=none`, with the same dependencies and about 1.5 min of extra work. It then fails unless `nm` shows `malloc`, `free`, `calloc` and `realloc` at the addresses of `mi_malloc`, `mi_free`, `mi_calloc` and `mi_realloc`. That job runs on every tag and nightly, and on any PR touching `crates/**`, the manifests, the lockfile or the workflow.
- **The gauges' wording** (`docs/metrics.md`, memory attribution): in the Linux release builds, `mem_mimalloc_committed_bytes` includes C allocations and `mem_sqlite_bytes` is part of it. So "other heap" there is `RssAnon − mimalloc`, not `− mimalloc − SQLite`. Every other build keeps the old reading.
- **The harness** `bench/examples/mem-cheap.rs`, with a `mimalloc-override` feature in `bench/Cargo.toml` for the comparison arm. It writes torrents one transaction each, then indexes and commits one document at a time, each on a `spawn_blocking` thread. Six reader tasks meanwhile search and read each hit's row and file tree (SQLite plus zstd) on their own blocking threads.

## Deviation from the brief
- **No startup self-check in the binary.** The brief asked for a test-only or debug check behind a `cfg`. It would live in `crates/lodestar/src/memstats.rs`, which X-mem-gauges-2 (#442) owns, and it would widen the rc.4 backport. A `cfg` alone can only restate the build flag; it can't prove the link. The release workflow's `nm` check does prove it, on the shipped build's own dependencies. The runner decided to drop the self-check (2026-11-04). It will fix `memstats.rs`'s stale module docs, which say mimalloc is "not `override`", after both PRs merge.

## Interplay
- **X-thp-off still applies.** mimalloc reads `MIMALLOC_ALLOW_THP` when it initialises, at process start, before `main`, as it did without `override`. With `MIMALLOC_VERBOSE=1 MIMALLOC_ALLOW_THP=0`, the musl binary prints `option 'allow_thp': 0`. `PR_SET_THP_DISABLE` is a per-address-space flag checked at fault time, so it also covers anything mimalloc mapped before `main`.
- **`sqlite3_memory_used` still reports.** SQLite keeps its own count whatever `malloc` is: 9.2 MiB after 10k torrents in both arms below.

## Measurement (musl via cargo-zigbuild 0.24.0, zig 0.17.0, x86_64, 4 vCPUs, 2026-11-05)
`mem-cheap 10000 6`: 10k torrents at one commit each, 6 reader tasks, 34 threads at the end. "Outside" is anonymous memory outside mimalloc's arena (anonymous regions under 256 MiB, adjacent mappings merged).

MEASUREMENT_TABLE

## Follow-ups (not in this PR)
- **The soak analyzer** (`docs/soak/tools/analyze_cr0.py`, owned by X-mem-gauges-2 until #442 merges) computes other heap as `RssAnon − mimalloc − SQLite` whenever SQLite is present. With `override` that subtracts SQLite twice, so the release builds' other heap reads about `mem_sqlite_bytes` too low. It needs to subtract SQLite only for builds without `override`, or always label the musl figure "RssAnon − mimalloc". Listed for the runner.
- **`crates/lodestar/src/memstats.rs`'s module docs:** the runner's, as above.
