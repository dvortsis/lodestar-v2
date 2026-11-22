# X-flake-metainfo-bound-rss decisions

## The failure
On the test VM (4 cores, 15 GB, `main` at 8fa35bdb), `crates/metainfo/tests/bound.rs::tree_at_the_full_budget_parses_within_bounds` failed 1 of 5 full `cargo test --workspace` runs: "peak RSS 523 MiB is over 512 MiB". Alone it passed 30 of 30 at 348 MiB, and as its own binary 40 of 40 at 348–384 MiB.

## Cause
The four measured tests shared one process and ran one at a time. Each reset Linux's peak resident set (`/proc/self/clear_refs`) before its parse, but a reset only goes down to what the process holds now. Heap an earlier test freed isn't always returned to the system, so it counted toward the next test's peak. The test measured its predecessors as well as itself.

## Calls
- **Each measured test runs in a process of its own** (runner's preferred option). It runs this test binary again, filtered to its own name with `--exact`, with `LODESTAR_METAINFO_BOUND_CHILD` set to that name. The child runs the checks, and the parent passes on its output and fails if it fails. The parent also fails if the child didn't run exactly one test, so a name that matched nothing can't pass. It uses only `std` (`current_exe`, `Command`), so it works the same on Linux, macOS and Windows. The children still run one at a time, so they don't compete for memory.
- **The 512 MiB bound is unchanged.** It's the product's budget (`2026-11-14-X-metainfo01-fix.md`). So are the 60 s time ceiling and the inputs.
- **The peak is still read only on Linux** (`VmHWM`, no new dependency). Elsewhere only time and results are checked, as before. A counterpart test, `the_peak_is_read_where_the_platform_reports_it`, pins this on every platform: the peak is read on Linux and nowhere else.

## Numbers
On the build VM, an Intel Xeon at 2.10 GHz with 4 vCPUs:

| Run | full budget | realistic | v1 depth 64 |
|---|---|---|---|
| `main`, the bound binary 20 times | 348–384 MiB, depending on what ran before it | 309–315 MiB | 281–294 MiB |
| This change, the bound binary 20 times | 348 MiB every time | 309–310 MiB | 271 MiB |
| This change, `cargo test --workspace` 3 times | 348, 348, 348 MiB | 310, 309, 310 MiB | 271, 271, 271 MiB |

All three workspace runs passed: 6,871 tests each, none failed. A test's peak no longer depends on what ran before it.
