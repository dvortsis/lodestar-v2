# X-soak-screen: an early-warning screen before and during every soak; it can fail a candidate early, never pass one
**Model:** Opus. This is soak tooling and the analyzer, both of which decide release evidence. A fresh Opus reviewer follows. Runner brief, 2026-11-07T00:35Z.

**The owner's answer** (2026-11-07 about 00:30Z, relayed by the dashboard session): "I'll go with your recommendations. Let's write really good tests that are able to catch any issues if they become apparent." The runner records it as owner-decisions §2.50.
- Items (a)1 and (a)2 of the runner's shorter-soaks message are approved.
- (a)3, the accelerated pre-filled-store run, only if 1 and 2 miss something.
- **The 24 h gate (CR-0) and CR-SPEC are unchanged.** This slice adds evidence. It never changes the gate's inputs, its window, its bar or its verdict.

## Read first
- `docs/decisions/2026-10-30-CR-0-acceptance.md`: the gate, which you never touch.
- **The soak verdicts:** `2026-11-03-soak-r3-verdict.md`, `2026-11-05-soak-r4-verdict.md` and `2026-11-06-soak-r5-verdict.md`.
- **The memory fixes:** `2026-11-02-R-mem-triage.md`, `2026-11-03-X-mmap-cache-prune.md`, `2026-11-05-X-mimalloc-override.md`, `2026-11-04-X-mem-gauges.md` and `-2.md`.
- **Code:**
  - `docs/soak/tools/{analyze_cr0.py,test_analyze_cr0.py,linux/collect.py,linux/test_collect.py,README.md}`;
  - `bench/tests/mmap_retention.rs`;
  - `bench/examples/mem-cheap.rs`;
  - the `mmap-retention` job in `.github/workflows/ci.yml`;
  - the memory rows in `docs/metrics.md`.
- The raw soak evidence is private and never enters the repo. Build synthetic fixtures shaped by the published numbers in the verdict files.

## Do
### 1. The pre-soak sweep: CI on release candidates, about 1 h
1. **A new workflow job, `soak-screen`.** It runs on `workflow_dispatch` and on pushes to `release/**`, never on pull requests. Pull requests keep the existing `mmap-retention` job.
2. **It runs two existing harnesses, with frozen thresholds:**
   - **Per-commit retention** (dhat `curr_bytes`, one torrent per commit), extending `mmap_retention`.
   - **Small anonymous mappings outside the allocator's arena,** per 1,000 torrents (`mem-cheap`), **on the musl release target** (the build r4 ran). If CI can't build or run musl, stop and ask the runner. Don't silently fall back to glibc.
3. **The thresholds live in one checked-in file,** `docs/soak/screen-thresholds.json`. Each has a value, the reason, and the measured clean value with its margin. Both the sweep and the in-run screen read them from there. The job prints a "SCREEN" summary, separate from any gate.

### 2. The in-run screen: in the collector report and the analyzer, at baseline hours 3, 6 and 12
1. **A structural counter of the r4 kind.** Add a Linux-only gauge counting anonymous mappings by size class (at least 128–512 KiB), read from `/proc/self/maps`, which is cheaper than `smaps`. Add it to `crates/lodestar/src/memstats.rs` and `metrics.rs`, with a `docs/metrics.md` row, and keep the per-scrape cost bounded. Measure that cost with about 3,000 mappings and report it.
2. **The screen inputs**, each as growth per 1,000 stored torrents plus a bound check:
   - `search_mmap_cache_entries{index}` against the live segment-file count;
   - the segment count;
   - the new mapping gauge;
   - `mem_mimalloc_committed_bytes`;
   - the fetch pool size and held bytes against their caps.
   **The memory slope is not a screen input.** r5 read 6.58 MiB/h at 14.6 h and finished at 0.91.
3. **Output:** `screen: ok | trip(<counter>, <value>, <threshold>) | not_available(<reason>)`, printed apart from the gate lines and written to its own JSON key.
   - A trip means "stop and investigate". The owner stops the run; the collector never stops it by itself.
4. **Freezing:** the collector writes the thresholds file's hash into `run.json` at start, and the analyzer flags a mismatch. Thresholds can't be changed mid-run without it showing.
5. **The README:** add a short "Screen" section for the owner. Say what it watches, that a trip means stop, and that "ok" never means pass.

## Tests: make them thorough
The owner asked for tests that catch problems as soon as they become visible.
1. **The r3 leak is caught.** With the tantivy mmap-cache prune switched off (the existing test-only switch), the sweep fails loudly, naming the counter. With the prune on, it passes with margin.
2. **The r4 growth is caught.** Using `mem-cheap` without mimalloc's `override` on musl, the mapping counter trips; with `override` it passes. If that arm truly can't run in CI, use a synthetic series with r4's numbers instead: 1,843 → 2,085 mappings in 1.9 h, about 0.2 per stored torrent. It must trip by hour 3 or 6, and the PR must say so.
3. **A clean run doesn't trip.** An r5-shaped synthetic series doesn't trip at hours 3, 6 or 12. That includes r5's noisy early memory slope of 6.58 MiB/h at 14.6 h, which must not matter because memory isn't a screen input. Current `main` passes the sweep.
4. **The screen can never pass a run:**
   - for every analyzer fixture, the gate fields and verdict are byte-identical with the screen on or off;
   - "ok" never sets or implies a gate pass;
   - a run that fails the gate fails whatever the screen says.
5. **Boundaries:** each threshold just below, at and just above its value, with the comparison direction stated and tested.
6. **Missing or partial data:**
   - snapshots without the new metrics give `not_available`, never ok or trip;
   - older run directories still analyze;
   - one missing checkpoint doesn't shift the others.
7. **Freezing:** a changed thresholds file after start is flagged.
8. **Determinism:** the same input gives the same output. The tests use no network, no wall clock and no fixed sleeps.
9. **The Windows collector** (`soak-collector-windows`): the screen reports `not_available` for the Linux-only gauges, and the job still passes.

## Done when
- **All of the above pass.** That includes `python3 -I -m pytest docs/soak/tools`, plus `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **The decisions file** `docs/decisions/2026-11-07-X-soak-screen.md` covers:
  - every threshold with its measured clean value and margin;
  - what the screen can and can't catch;
  - **what else it would test, as a list for the runner:**
    - a query-load arm during the sweep (search under load is untested in soaks);
    - a bounded-structures test over every cache and map with its cap;
    - the v0.5.1 → v0.5.2 upgrade under the sweep.
- **Size:** about 1,500 lines or fewer. Ask the runner before editing outside the owns: the soak tools, the bench harnesses, `ci.yml`'s new job, `memstats.rs`, `metrics.rs`, `docs/metrics.md`, the thresholds file and your decisions file.
- **No tokio `test-util` and no `testing` features on the `lodestar` package.**
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `X-soak-screen: early-warning screen before and during soaks`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA when CI is green.
