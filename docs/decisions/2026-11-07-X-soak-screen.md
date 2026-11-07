# X-soak-screen decisions (an early-warning screen before and during soaks)

The runner's brief X-soak-screen, 2026-11-07, comes from the owner's answer recorded as owner-decisions §2.50: "Let's write really good tests that are able to catch any issues if they become apparent." Items (a)1 and (a)2 are approved; (a)3, the accelerated pre-filled-store run, waits until 1 and 2 miss something.

**The screen adds evidence and never decides.** The CR-0 gate (`2026-10-30-CR-0-acceptance.md`) and CR-SPEC are unchanged: their inputs, window, bar and verdict all stay as they were. A trip means "stop and investigate". `ok` never means pass.

## What was built
- **The pre-soak sweep:** the `soak-screen` job in its own workflow, `.github/workflows/soak-screen.yml`. It runs on `workflow_dispatch` and on pushes to `release/**`, never on pull requests. It's in no required check and isn't part of `ci-ok`. Its actions are pinned by SHA. It runs two existing harnesses, each with a **control arm that must still trip**. A sweep that can no longer see the known leak therefore fails as well.
  - **Per-commit retention:** `bench/tests/mmap_retention.rs`, dhat `curr_bytes`, one torrent per commit. It runs with tantivy's mmap-cache prune on, then off (`MMAP_RETENTION_PRUNE=off`, the existing test-only switch).
  - **Small anonymous mappings outside the arena:** `bench/examples/mem-cheap`, built for **`x86_64-unknown-linux-musl` with cargo-zigbuild and zig 0.17**, the r4 build. It runs with mimalloc's `override`, then without it. CI builds musl the way `release.yml` already does, so nothing falls back to glibc.
  - `docs/soak/tools/screen_sweep.py` judges both outputs against the thresholds file. It prints one `SCREEN sweep` line per counter and appends them to the job summary.
- **The in-run screen** at baseline hours 3, 6 and 12:
  - `analyze_cr0.py` computes it under the JSON key `screen`, and prints a "Screen" section after everything else, with its own `screen:` line.
  - The Linux collector writes `screen.json` and a `screen baseline hour N:` line in `events.log`.
  - Both run the same code. Its "screen core" block is byte-identical in the two files, and `test_screen.py` fails if the copies drift. Each tool still runs alone under `python3 -I`, so neither can import the other: the collector is mounted on its own in Docker.
- **The new gauge** `lodestar_mem_anon_mappings{size="lt_128k"|"128k_512k"|"512k_2m"|"ge_2m"}` (`crates/lodestar/src/memstats.rs`, `docs/metrics.md`). It's read from `/proc/self/maps` on Linux, on the existing 60 s storage sampler. A scrape reads four atomics.
  - It counts anonymous mappings, meaning no path and inode 0, that aren't `---p`. A run of adjacent anonymous mappings is an arena, and isn't counted, only when it spans 256 MiB or more **and** holds one mapping of 64 MiB or more.
  - **That's stricter than `mem-cheap`'s rule, which uses the span alone.** By span alone, r4's 2,000-odd 128–512 KiB mappings (about 500 MiB) would read as an arena whenever they sat side by side, and the gauge would show 0 exactly when r4's growth came back. A test covers that shape, with and without guard pages (fresh self-review). `mem-cheap` keeps its own rule, because its measurements here and in X-mimalloc-override were made with it. The sweep's control arm shows it still sees the growth.
  - It reads one line at a time and gives up past 131,072 lines. A line that doesn't parse makes all four series absent.
- **CR-0 runs only.** The collector runs no screen for CR-SPEC kinds (`ab`, `comb`, `comparator`). Under rule 6.11, CR-SPEC shows no counts or rates while its window is open, and its evidence stays unchanged (fresh self-review). Without `--screen-thresholds`, a CR-0 run notes "screen off" in `events.log`.
- **Freezing:**
  - At start the collector copies the thresholds file into the run directory and records its SHA-256 in `run.json` (`screen_thresholds_sha256`).
  - The analyzer judges only by that copy, and refuses one whose hash no longer matches (`not_available(thresholds_modified)`).
  - It also flags a checked-in file that has changed since the run started (`changed_since_start`).
  - The collector re-hashes both files at each checkpoint (`thresholds_changed`).
  - A run with no recorded hash (Windows, or older runs) is judged by the checked-in file and labelled "not frozen".

## The thresholds (`docs/soak/screen-thresholds.json`)
Every counter trips when its value is **strictly above** its threshold; at the threshold it's ok. Growth is the OLS slope of the input against stored torrents (`lodestar_torrents`) over baseline hour 0 to the checkpoint, per 1,000. A growth figure needs at least 30 samples and 500 stored torrents of spread. A bound is the value at the checkpoint sample.

| Counter | Threshold | Measured clean value | Margin |
| --- | ---: | --- | --- |
| Sweep: retention, B per commit | 300 | 32 (pruning on, release, 2026-11-07); r3's arm 2,513 (pruning off) | 268 B above clean (9.4 ×); the control arm reads 8.4 × the threshold |
| Sweep: mappings < 512 KiB outside the arena per 1,000 torrents (musl) | 2 | 0.00 with `override` (4 mappings from 1,000 to 10,000 torrents) | the control arm read 17.28, 8.6 × the threshold |
| mmap-cache entries per 1,000 stored, either index | 40 | r5-shaped fixture: 15 at hour 3, 4 at 6, 1 at 12 (merge sawtooth) | 2.6 × the fixture's worst; one leaked mapping per commit reads about 1,000 |
| mmap-cache entries beyond 8 per segment (a stand-in for the live segment-file count, which isn't a metric) | 32 | r5's end: files 72 − 8 × 15 = −48 | 80 entries |
| Segments per 1,000 stored, either index | 10 | fixture: 3 at hour 3, 0.8 at 6 | 3.3 ×; stopped merges read about 1,000 |
| Segments, either index | 64 | r5 peak 21 | 3 × |
| 128–512 KiB mappings per 1,000 stored | 20 | 0 (harness with `override`) | r4 read about 200: 10 × |
| 128–512 KiB mappings | 512 | 0 (harness) | r4 held 1,843–2,085: 3.6 × |
| mimalloc committed, MiB per 1,000 stored | 20 | r5 flat at 568 MiB over hours 12–24; the fixture assumes 11.7 early | 1.7 × the assumed early fill |
| mimalloc committed, MiB | 1,536 | r5: 568 | 2.7 × |
| Fetch pool keys | 200,000 | the cap; r3 and r5 sat at it | none: at the cap is backpressure, above it is a broken cap |
| Fetch pool held, MiB | 128 | the 64 MiB budget, by design | 2 × the budget, since fetches already running can pass it |

- **Why the sweep's mapping counter isn't r4's class alone.** Without `override`, this run of the harness grew the 128–512 KiB class by only 2.64 per 1,000 torrents, 1.3 times a threshold of 2, too close for a control arm that must always trip. The same mechanism grows the class under 128 KiB about five times as fast, so the sweep sums the two: 17.28 against 0.00. The in-run gauge keeps r4's own class, where production's rate (about 200 per 1,000) is far above any threshold.
- **Synthetic clean values.** The raw soak evidence is private, and r5 had none of the new gauges. Wherever r5 didn't publish a figure, the clean value is a fixture shaped by the published ones (`test_screen.py`'s `r5`), and the table says so. The mimalloc growth threshold rests the most on that assumption. Recalibrate every in-run threshold from the first screened soak's checkpoint figures, as a new decision with a new hash.
- **Before relying on the in-run thresholds** (fresh self-review): r4's and r5's private run directories already hold `mem_mimalloc_committed_bytes`, `search_segments` and `search_mmap_cache_entries`. Run the new analyzer over them on the owner's host and relay only the `screen:` lines and the per-checkpoint values. Then r5 should read ok and r4 should trip (on these counters; r4 has no mapping gauge). Retune any growth threshold that r5 comes within 1.5 times of.
  - **The in-run thresholds are provisional until then** (runner, 2026-11-07T03:15Z). After this merges, the runner asks the owner for that run. If the measured values move the mimalloc or segment thresholds, a small follow-up changes only `screen-thresholds.json` and its notes, under a new hash.
- **The sweep's first CI run** happens after merge: `workflow_dispatch` only offers a workflow that's on the default branch, and no `release/**` push has happened. Every figure above is a local measurement on the same toolchain (cargo-zigbuild 0.24.0, zig 0.17.0). The runner dispatches `soak-screen.yml` on `main` by hand after the merge and treats that run as its proof (2026-11-07T03:15Z). A fix for a failure there comes from this slice, or from a follow-up slice.
- **Mapping gauge cost:** with 3,105 mappings, a read takes 0.66 ms median and 1.13 ms max, measured in the test profile on 4 vCPUs (`anon_mappings_cost_at_3000_mappings`, which the sweep job reruns and prints). At 6,105 mappings, under load from a parallel build, it took 1.5 ms. It runs once a minute and never in a scrape.

## What the screen can and can't catch
- **It catches:**
  - r3's leak in the sweep. With the prune off, retention read 2,513 B per commit and the sweep trips naming `retention_bytes_per_commit`.
  - r4's growth in the sweep (the harness without `override`), and in a run by hour 3. The r4-shaped series (about 0.2 mappings per stored torrent) reads about 200 per 1,000 against 20, and the level bound trips too.
  - Live mappings that are never released, and merges that stop, from the mmap-cache and segment counters.
  - A heap growing with the corpus in mimalloc. In musl builds that's the Rust and C heaps both.
  - A fetch pool past its caps.
- **It can't catch:**
  - **Dead mmap-cache entries in a run.** The gauge counts live mappings after a prune, so it can't see an entry nobody prunes: r3's mechanism. Only the sweep sees that.
  - Memory growth through any path not listed above: thread stacks, page tables, THP fill-in, or a C heap other than mimalloc's in non-musl builds.
  - Slow growth below the thresholds. r3's 2 MiB/h is about 3 MiB per 1,000 stored, which no in-run threshold here resolves. The gate does.
  - Anything that starts after hour 12.
  - The mapping counters on Windows, which has no `/proc`. They report `not_available`.
  - Load the soak doesn't apply: there's no search load in the soak protocol.
- **Missing data is never ok or trip.** A snapshot without an input, or with a value no float holds, gives `absent`. So do too few samples, too little stored spread, a checkpoint not reached, no sample within 180 s of the checkpoint, a restart before it (the gate's identity check), and unusable thresholds (a missing, edited or malformed hash included). Each checkpoint stands at its own hour, so a gap at hour 6 leaves hours 3 and 12 as they were.

## Tests
`docs/soak/tools/test_screen.py` (33), plus `linux/test_collect.py`'s `ScreenTests` (4) and the Rust gauge's tests.
- **The r3 leak and the r4 growth are caught,** both through the sweep script and its control arms. In a run, r4's series trips at hours 3, 6 and 12.
- **A clean run doesn't trip:** an r5-shaped run is ok at all three checkpoints. Its noisy early memory slope (6.58 MiB/h), a flat memory series and a gate-failing 29 MiB/h series all give an identical screen.
- **The screen never passes a run:**
  - For every fixture in `test_analyze_cr0.py`, the whole report minus `screen` is identical with the screen on or off, and the Markdown with it off is a prefix of the Markdown with it on. The test runs that suite with every analysis done both ways.
  - `ok` with a failing gate stays a failing gate.
  - The screen's JSON never says pass, `meets_bar` or verdict.
- **Boundaries:** every counter is tested just below, at and just above its threshold, plus far below. The sweep's two counters are tested the same way.
- **Missing or partial data:** snapshots without the new gauges, r3-era run directories, a missing hour-6 checkpoint, unreached checkpoints, failed metrics reads, a restart, and a Windows-shaped run, which gives `not_available` for the Linux-only gauges and ok for the rest.
- **Freezing:** each of these is flagged: an edited copy in the run directory, a missing copy, a changed checked-in file, and a file changed mid-run in the collector.
- **Determinism:** the same input gives the same JSON and Markdown. No test uses the network, the wall clock or a fixed sleep. The collector's tests use the virtual clock.

## Shared files
- `crates/lodestar/src/metrics.rs` and `docs/metrics.md`: the gauge's render, one row, and the existing documentation test extended to it. X-fetch-meta-reserve also adds rows; whichever PR merges later brings `main` in.
- **A workflow of its own, not a job in `ci.yml`** (runner, 2026-11-07T01:45Z, replacing the brief's "new job in `ci.yml`"). `ci.yml` runs only on pushes to `main`. Giving it a `release/**` trigger would run the full matrix on every release-branch push, which changes CI cost for every release and isn't this slice's call. `ci.yml` is untouched; the runner updates the slice's owns in `runner/slices.yaml`.
- **No cherry-pick.** `release/v0.5.1` is tagged. The v0.5.2 cut carries the workflow from `main`, and GitHub reads a push's workflow from the pushed branch.
- The analyzer's file hash changes, so the next soak pins the new `analyze_cr0.py` and `collect.py` hashes, as for any tools change. The gate's code paths are unchanged, and the test above proves their output is.

## For the runner: what else to test
- **A query-load arm during the sweep.** Search under load is untested in soaks (r3's placement table). Run `mem-cheap`'s readers at a production-like query rate against a store of millions, and screen search p99 and the mmap and segment counters while commits and merges run.
- **A bounded-structures test over every cache and map with its cap:** the result, browse and file caches, the fetch pool, the sampler states, the recent-keys set, the DHT tables and the peer queues. Drive each past its cap and assert entries and bytes stay at or under it. Some can't be driven from outside the crate; that test would need a crate-level seam.
- **The v0.5.1 → v0.5.2 upgrade under the sweep:** open a v0.5.1 store and index, upgrade, then run the retention and mapping arms on the upgraded data rather than a fresh one.
- **Recalibration:** after the first screened soak, replace the fixture-based clean values with that run's checkpoint figures.
