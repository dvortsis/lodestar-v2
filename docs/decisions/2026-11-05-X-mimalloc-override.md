# X-mimalloc-override decisions (one allocator for Rust and C in the release build)

Runner brief X-mimalloc-override, 2026-11-04 (an rc.4 candidate; auto-approved under owner §2.33, which names mimalloc's `override` feature as the no-`unsafe` route, and flagged to the owner under §2.19). Architecture §11 (the binary and its allocator). Follows `2026-11-04-X-thp-off.md` and `2026-11-04-X-mem-gauges.md`.

## Why
- **Soak r4:** the worker's anonymous memory grew about 173 MiB in 4 h, in base pages. About 310 MiB sat outside mimalloc's arena in about 3,200 small mappings, 1,843 of them 128-512 KiB.
- **The C allocator in the release build** (cargo-zigbuild, zig 0.17, static musl) is zig's `lib/c/malloc.zig` over `std.heap.SmpAllocator`. Its 64 KiB slabs are never unmapped, and an allocating thread checks only its own CPU slot's freelist and one neighbour's. SQLite and zstd run on many `spawn_blocking` threads, so freed C memory piles up where no other thread reuses it. The measurement below shows that pattern under threads.

## What changed
- **mimalloc's `override` feature, for musl Linux only** (`crates/lodestar/Cargo.toml`, a `cfg(all(target_os = "linux", target_env = "musl"))` dependency entry that adds only `override`; `extended` still comes from the unconditional entry, and features unify). mimalloc then defines `malloc`, `free`, `calloc`, `realloc` and the rest. In the static link they resolve to mimalloc's, so SQLite, zstd and ring allocate from the same arenas as Rust. No `unsafe`, no code change, and `Cargo.lock` is unchanged.
  - **Not Windows:** mimalloc can't override the CRT's `malloc` in a static link, and the MSI isn't where the growth is. **Not glibc:** glibc's malloc isn't the allocator r4 ran, and dev builds stay as they were. Both builds are unchanged.
  - **Both processes:** the supervisor and its `--child` worker are the same binary, so both run with it.
- **The release workflow checks it** (`.github/workflows/release.yml`, a step at the end of `package-linux`, both arches). The shipped binary has no symbols. So after the upload the step relinks only the `lodestar` crate with `-C strip=none`, with the same dependencies and about 1.5 min of extra work. It then fails unless `nm` shows `malloc`, `free`, `calloc` and `realloc` at the addresses of `mi_malloc`, `mi_free`, `mi_calloc` and `mi_realloc`. That job runs on every tag and nightly run. On a pull request it runs only when `packaging/`, the workflow, a `Cargo.toml`, `Cargo.lock` or `rust-toolchain.toml` changes, or the PR has `full-ci` (`package-gate`).
- **The gauges' wording** (`docs/metrics.md`, memory attribution): in musl builds (the release builds: the Docker image and the tarballs), `mem_mimalloc_committed_bytes` includes C allocations and `mem_sqlite_bytes` is part of it. So "other heap" there is `RssAnon − mimalloc`, not `− mimalloc − SQLite`. Every other build keeps the old reading.
- **The harness** `bench/examples/mem-cheap.rs`, with a `mimalloc-override` feature in `bench/Cargo.toml` for the comparison arm. It writes torrents one transaction each, then indexes and commits one document at a time, each on a `spawn_blocking` thread. Six reader tasks meanwhile search and read each hit's row and file tree (SQLite plus zstd) on their own blocking threads.

## Deviation from the brief
- **No startup self-check in the binary.** The brief asked for a test-only or debug check behind a `cfg`. It would live in `crates/lodestar/src/memstats.rs`, which X-mem-gauges-2 (#442) owns, and it would widen the rc.4 backport. A `cfg` alone can only restate the build flag; it can't prove the link. The release workflow's `nm` check does prove it, on the shipped build's own dependencies. The runner decided to drop the self-check (2026-11-04). It will fix `memstats.rs`'s stale module docs, which say mimalloc is "not `override`", after both PRs merge.

## Interplay
- **X-thp-off still applies.** mimalloc reads `MIMALLOC_ALLOW_THP` when it initialises, at process start, before `main`, as it did without `override`. With `MIMALLOC_VERBOSE=1 MIMALLOC_ALLOW_THP=0`, the musl binary prints `option 'allow_thp': 0`. `PR_SET_THP_DISABLE` is a per-address-space flag checked at fault time, so it also covers anything mimalloc mapped before `main`.
- **`sqlite3_memory_used` still reports.** SQLite keeps its own count whatever `malloc` is: 9.2 MiB after 10k torrents in both arms below.

## Measurement (musl via cargo-zigbuild 0.24.0, zig 0.17.0, x86_64, 4 vCPUs, 2026-11-05)
`mem-cheap 10000 6`: 10k torrents at one commit each, 6 reader tasks, 34 threads at the end. "Outside" is anonymous memory outside mimalloc's arena (anonymous regions under 256 MiB, adjacent mappings merged).

Pair 2 ran on an otherwise idle machine, `override` on first. Pair 1 ran `override` off first, and a workspace clippy build ran alongside the first half of its off arm, so only pair 2's latency is quoted.

| After 10k torrents | Off, pair 2 | On, pair 2 | Off, pair 1 | On, pair 1 |
| --- | --- | --- | --- | --- |
| Outside the arena, start → end | 5.2 → 18.2 MiB | 0.3 → 0.8 MiB | 5.2 → 35.0 MiB | 0.3 → 0.8 MiB |
| Outside mappings < 128 KiB, start → end | 1 → 150 | 1 → 1 | 2 → 232 | 1 → 1 |
| Outside mappings 128-512 KiB, start → end | 5 → 44 | 0 → 0 | 6 → 131 | 0 → 0 |
| `RssAnon` at 5k / 10k | 266.9 / 302.5 MiB | 298.0 / 300.7 MiB | 254.8 / 290.4 MiB | 310.6 / 307.8 MiB |
| mimalloc committed at 10k | 370.8 MiB | 384.2 MiB | 320.2 MiB | 384.3 MiB |
| SQLite's own count at 10k | 9.2 MiB | 9.2 MiB | 9.2 MiB | 9.2 MiB |
| Store write (one transaction) p50 / p99 | 0.27 / 6.51 ms | 0.26 / 6.54 ms | | |
| Index commit p50 / p99 | 28.13 / 59.82 ms | 27.37 / 59.59 ms | | |
| Search plus row and file reads p50 / p99 | 0.54 / 3.45 ms | 0.56 / 3.53 ms | | |

- **The outside-arena climb is gone.** Without `override`, the C heap's mappings keep multiplying in r4's 128-512 KiB pattern, and their memory grows the whole run. That's 13 MiB over 10k torrents idle, and 30 MiB with more threads busy. With it, nothing is left outside the arena but thread stacks: no 128-512 KiB mapping, and 0.8 MiB flat from the first thousand torrents on.
- **The bytes move into the arena,** as expected: mimalloc's committed bytes end 13-64 MiB higher. In this harness the total `RssAnon` is dominated by the search indexer's working memory inside the arena, which swings 270-325 MiB from sample to sample. So 10k torrents can't show whether the total stops climbing. Its outside-arena part does. The soak (r5) is the test of the total.
- **No latency cost:** every p50 and p99 is within 4% between the arms.

## Follow-ups (not in this PR)
- **The soak analyzer** (`docs/soak/tools/analyze_cr0.py`, owned by X-mem-gauges-2 until #442 merges) computes other heap as `RssAnon − mimalloc − SQLite` whenever SQLite is present. With `override` that subtracts SQLite twice, so the release builds' other heap reads about `mem_sqlite_bytes` too low. It needs to subtract SQLite only for builds without `override`, or always label the musl figure "RssAnon − mimalloc". Listed for the runner.
- **`crates/lodestar/src/memstats.rs`'s module docs:** the runner's, as above.
- **`crates/lodestar/src/thp.rs`'s module docs** (lines 22-23) say `MIMALLOC_ALLOW_THP` "covers only mimalloc, not the C heaps (SQLite, zstd, ring)". In musl builds it now covers them too. That file isn't owned here, so it's listed for the runner.

## Backport to `release/v0.5.1` (rc.4)
Cherry-pick only the `crates/lodestar/Cargo.toml` hunk (the musl-only entry, `features = ["override"]`) and the `.github/workflows/release.yml` step. `Cargo.lock` doesn't change. Leave out the rest: `docs/metrics.md`'s memory-attribution section, `bench/`'s mimalloc dependency and the store's `memstats` don't exist on 0.5.1. The PR's commits mix these files, so take the hunks, not whole commits.
