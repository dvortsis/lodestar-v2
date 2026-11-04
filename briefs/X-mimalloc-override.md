# X-mimalloc-override: one allocator for Rust and C in the release build
**Model:** Opus. This is a process-wide allocator change. The runner reviews it with a fresh Opus reviewer. Runner brief, 2026-11-04. Auto-approved under owner §2.33: §2.35 names mimalloc's `override` feature as the no-`unsafe` route. Flagged to the owner under §2.19. This is an rc.4 candidate.

## Why (soak r4 evidence, read-only, relayed 2026-11-04)
- **Where the growth is.** The worker's anonymous memory grew by about 173 MiB in 4 h, in base pages, not THP. mimalloc's 1 GiB arena holds about 305-317 MiB. Outside it, about 3,200 small mappings hold about 310 MiB; 1,843 of them are 128-512 KiB and hold 223 MiB. There's no `[heap]`, and there are 33 threads.
- **The code isn't leaking.** With glibc, the write, claims and index path keeps no C allocation per torrent over 10k torrents: `sqlite3_memory_used` caps at about 8.5 MB, and zstd keeps nothing.
- **The release C allocator.** In the release build (cargo-zigbuild, zig 0.17), `malloc` isn't musl's mallocng. It's zig's `lib/c/malloc.zig` over `std.heap.SmpAllocator`:
  - power-of-two classes up to 32 KiB, in 64 KiB slabs that are **never unmapped**;
  - **per-CPU-slot freelists.** An allocating thread checks only its own slot and one neighbour, then maps a fresh slab. Production calls SQLite and zstd from many `spawn_blocking` threads, so freed memory can pile up in slots that other threads never reuse. That's inferred, not yet measured.
- **What `override` did in a single-threaded musl harness.** With mimalloc's `override`, `malloc` became `mi_malloc` in the binary. The outside-arena memory went to about 0.3 MiB with no 128-512 KiB mappings, and the C memory moved into mimalloc's arena. It linked cleanly on static musl with zig.

## Do
1. **Enable mimalloc's `override` feature for the musl Linux targets only.** Use a target-specific dependency in `crates/lodestar/Cargo.toml`, `cfg(all(target_os = "linux", target_env = "musl"))`, so the Windows MSI and glibc dev builds are unchanged. If you find a reason to enable it more widely, say why in the decisions file.
2. **Prove it's in effect in the shipped binary.** Add a check in CI that builds the musl release binary (or reuses an existing musl build job) and verifies that `malloc` resolves to mimalloc, for example `nm` showing `malloc` defined by mimalloc. Also add a startup self-check behind a `cfg`: a test-only or debug log line is enough, with no new runtime cost.
3. **Measure under threads.** The harness from the investigation is a throwaway crate (described above; rebuild your own under `bench/examples/` or a test). Run 10k torrents at one commit each with SQLite and zstd called from several threads, as production does, on musl via cargo-zigbuild. Report outside-arena dirty memory and the count of mappings by size bucket, with and without `override`. Also report total RssAnon: the bytes move into the arena, and the question is whether the total stops climbing. Also report commit and search latency, p50 and p99.
4. **Gauges:** with `override`, mimalloc's stats include C allocations. Update the `docs/metrics.md` wording for the mimalloc and "other heap" rows. If the analyzer's "other heap" note in `docs/soak/tools/**` needs a change, list it for the runner and don't edit it: X-mem-gauges-2 (#442) owns that path until it merges.
5. **Interplay:** X-thp-off sets `MIMALLOC_ALLOW_THP=0` for the worker, and that must still apply. Check that the supervisor and worker both run with `override`, and that `sqlite3_memory_used` still reports.
6. **Release backport:** keep the change minimal (Cargo.toml, Cargo.lock, the CI check, docs), so it cherry-picks cleanly onto `release/v0.5.1` for rc.4.

## Own
`crates/lodestar/Cargo.toml` (the mimalloc dependency lines only), `Cargo.lock`, the harness under `bench/examples/mem-cheap.rs` (or similar, with a `bench/Cargo.toml` entry if needed), the CI check (one step or job in `.github/workflows/ci.yml` or `release.yml`; say which), the `docs/metrics.md` rows, and `docs/decisions/2026-11-05-X-mimalloc-override.md` (use the UTC date the PR opens).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the musl build and the check pass, the threaded measurement is reported, and a fresh self-review is done. A Windows build must be unaffected.

Branch `slice/X-mimalloc-override`. The PR is titled "X-mimalloc-override: One allocator for Rust and C in the release build". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round. Times in docs are UTC (§2.35).
