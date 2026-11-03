# X-mmap-cache-prune: the authoritative M1 decision sequence (runner, 2026-11-03 ~03:55Z)
This replaces every M1-exp rule in briefs/X-mmap-cache-prune.md, including the <=10% rule and the earlier Part 2 thresholds. Approved by the owner after two external reviews.

### M1 decision sequence (authoritative; replaces every earlier M1-exp rule, amendment and threshold)
Revised after external reviews by Gemini and then Codex (2026-11-03, twice). Every number and rule in this section was set before the remaining results arrived. The first Part 1 batch was seen before revision 2; that is stated in the decisions file.

**Run configurations.** Each run records its full configuration and what it achieved: documents per commit, commits/s, input backlog, and resource limits. A commit fires when `pending ≥ commit_docs` OR (`pending > 0` and `commit_every` has elapsed) (`live.rs:309`), after a catch-up of up to `batch` documents. So each config is chosen to give its intended batching, and the achieved batching is checked.

| Run | batch | commit_docs | commit_every | poll | Input |
|---|---|---|---|---|---|
| P1-A / P1-B (accelerated mechanism) | 1 | 1 | 2 s (irrelevant at 1 doc) | 10 ms | 1 torrent written per tick, as fast as commits complete |
| L (diagnostic only) | 1000 | 1000 | 1 h (so the timer never flushes early) | 500 ms | same torrent stream as P1, written in blocks of 1000 |
| P2-A / P2-B (production) | `serve`'s own `LiveConfig` (1000 / 5000 / 2 s / 500 ms) | — | — | — | ~665 torrents/h, paced |
| M2 runs (if any) | `serve`'s config with `commit_every` 30 s | — | — | — | as P2 |

**Measurement isolation**
- One measured workload per process.
- Anon comes from per-process `RssAnon`, or a dedicated cgroup per run, never a cgroup shared by arms or profilers.
- dhat runs are separate gnu processes and never share a process with anon runs.
- `get_cache_info()` is never called in arm A, and never in an assertion.

**Step 1: mechanism (Part 1, accelerated).**
- The estimator: OLS of dhat `curr_bytes` against the commit count, over commits 2,000 to the end, per run. The arm estimate is the mean of 3 run slopes with a df = 2 t-interval. Per-run slopes and their spread are always reported.
- **Control check:** if A's mean is not within 1–3.5 KB/commit, the result is **inconclusive**. Stop, diagnose and report; don't change code.
- **Pass:** the two arms' intervals don't overlap (B's upper bound is below A's lower bound), **and** the dhat sites under `MmapCache::get_mmap` stop accumulating in B. The site evidence is kept alongside the interval test.
  - This rule was **chosen after seeing the first batch.** It is labelled that way in the decisions file. It is **frozen now, at 03:5xZ, with the endpoint (commits 2,000 to the end of each 1 h run), before the confirmation batch relaunched at 03:13Z reports.** That batch is the confirmation; the first batch is exploratory.
  - A pass shows the prune removes the leak. It says nothing about the residual (Step 2) or about total growth (Step 3).
- **The old ≤ 10% rule is withdrawn.** It compared B to zero, but every commit adds a torrent, so it wasn't a valid test of the prune. That's recorded as made after seeing the results.

**Step 2: explaining B's residual. Diagnostic, not a gate.**
- **Primary evidence: allocation sites.** A dhat diff of arm B with the index alive at about 1,500 and about 4,500 commits, attributing the residual to sites: the title FST, hot columns, per-segment readers, or another per-commit retainer.
- **Supporting evidence: arm L,** measured in **bytes per torrent** over the same torrent-count range and document stream as P1, from per-block samples. With about 11 commits it can't use the commit estimator. L changes segment count, merge timing and readers too, so **it is never subtracted from A or B as an acceptance rule.**
- **Outcomes:**
  - The residual is attributed to sites that grow per torrent and are bounded or budgeted: record it, and continue.
  - A second per-commit retainer is found: it becomes its own finding and decision (fix it in M1 if it's small and in scope, else a follow-up). It never becomes M2 automatically.
- **The regression test's gap.** The test keeps < 300 B/commit, while B's experiment runs keep 498–941. The difference must be explained (window, workload, background threads), with site attribution. If the test misses a retainer the experiment shows, the test is extended to catch it.

**Step 3: total growth at production cadence (Part 2). Supporting evidence; the gate is r4.**
- 2 runs per arm, ≥ 6.5 h; or, if the container can't sustain that, ≥ 2 h per run, reported as shortened.
- The window starts after 30 min. The output is anon MiB/h per run, with a CI.
- **Per-run estimate:** each run's anon slope over the fixed window uses analyze_cr0's `memory_fit` method (OLS, slope ± 1.96·SE), the same as the soak gate, so the numbers are comparable.
- **"Clearly below" (fixed in advance):** every B run's upper bound is below every A run's point estimate, **and** the mean of the B slopes is below the mean of the A slopes.
- **Conflicting runs** (one pair meets this, the other doesn't): **inconclusive**. Report it, and the runner decides whether a third run per arm is worth it.
- **The M2 screen:** the highest B upper bound across the runs. The 1.0 MiB/h line is a heuristic screen, not acceptance: it leaves headroom under r4's 2.0 bar (r3's CI half-width was about 0.45). r4 is the acceptance.
- **Outcomes:**
  - **B clearly below A, screen ≤ 1.0:** M1 goes ahead alone.
  - **B clearly below A, screen > 1.0:** M1 goes ahead, because it removes a confirmed leak. M2 is then evaluated as its own experiment on top of M1 (Step 5), before R3′.
  - **B shows an unexplained regression** (B above A, or A flat while B grows): hold the candidate, diagnose, and report. Nothing moves on, and M2 is not triggered.
  - **A flat and B flat:** inconclusive at production cadence. Report it explicitly. M1 may go ahead on Step 1's proof alone, and r4 decides; the runner says so in the PR.
  - **A grows and B is no better** (not clearly below, no regression): inconclusive. Report it, and decide whether more measurement is needed before M1 merges. M2 is not triggered automatically.
- **Nothing from Part 1 is extrapolated to MiB/h.**

**Step 4: merge M1.** It merges when all of these hold:
- Step 1 passed;
- Step 2's attribution is reported (not necessarily resolved);
- Step 3 is reported, with **no unexplained adverse result** (a regression, or "no better" with no explanation);
- the PR's tests pass (below), a fresh Opus review approves, and ci-ok is green.

**Step 5: M2, only when Step 3's second outcome applies.**
- Its own A/B: M1 against M1 + M2, at production cadence, with the same estimator.
- The guardrail: discovery-to-searchable p95 ≤ 10 min, flagged to the owner (§2.19).
- If it helps, the combined M1 + M2 is validated at production cadence, reviewed and merged before R3′.

**Step 6: release.**
- R3′ carries #402, M1, and M2 if it merged.
- Then the rc.3 tag, then soak r4: the frozen 24 h run with the unchanged ≤ 2.0 upper-bound bar. **This is the only memory acceptance.**

### M1: X-mmap-cache-prune (Opus, core: search)
**Fix:**
- In `crates/search/src/index.rs`, keep a clone of each `MmapDirectory` (an `Arc` inside) in `SearchIndex` before wrapping it in `Retrying`.
- After each successful commit of both indexes (~1590), call `dir.get_cache_info()` on both. Also call it after `open`, `open_existing` and a rebuild swap.
- If commit latency shows it costs anything, throttle it to at most once every few seconds.
- It takes the cache write lock, scans, and clones paths. The HashMap keeps its peak capacity, which is bounded once the growth stops.
- Keep it minimal so it cherry-picks onto rc.2. No tantivy fork or upgrade.

**Observability:**
- A gauge `lodestar_search_mmap_cache_entries{index="main|files"}`, defined as **live mappings retained after the latest prune**, in `crates/lodestar/src/metrics.rs`, with a row in `docs/metrics.md`. It cannot count accumulated dead entries.
- It is judged against the live segment-file count, not "stays flat". It may grow with legitimate live files.

**Tests:**
- **A regression test that detects removing the fix.** It lives in its own test binary in the dev-only bench crate (one test per binary), under dhat's testing mode (`dhat::Profiler::builder().testing()`). dhat's guidance is that parallel tests and the runner itself interfere, hence:
  - the exact CI command: `cargo test --release -p lodestar-bench --test mmap_retention -- --test-threads=1`;
  - with the index **kept alive**, sample `dhat::HeapStats::get().curr_bytes` at commit 500 and at commit 2,500, under paced one-document commits with merges on;
  - assert that the retained growth per commit between those samples stays under a threshold. The threshold is set from measured data, well below the leak's ~1–3 KB/commit and above what the legitimate live index grows;
  - it never asserts cumulative allocations (that's churn, not retention), and it never samples after the index is dropped;
  - a test-only switch turns production pruning off. The PR shows the test fails with it off and passes with it on;
  - the assertion never calls `get_cache_info()`.
- **Lifecycle:**
  - a failed or partial commit and reload (prune skipped, no panic);
  - the end of a merge, while segments are still being garbage-collected;
  - the reopen and rebuild paths (old directories dropped whole).
- **Performance:** commit latency p50 and p99 with and without pruning, and search latency while commits run. Report both.
- **A concurrent-search smoke test**, bounded, in the harness and not the soak:
  - title and file queries (file queries hit the 1 MiB zstd doc-store blocks) running during drip commits;
  - the reclamation check follows an explicit sequence, with production code doing all the pruning:
    1. queries finish and their leases expire;
    2. a merge finishes and its garbage collection runs;
    3. the harness ingests one more document to force a commit and a reader reload, then waits (a condition with a ≥ 10 s deadline) until a production prune has actually run **after that reload**: a prune counter or timestamp, exposed for tests, has moved on. That matters if pruning is throttled, when one extra commit may not trigger one;
    4. only then read the gauge, which holds the value that prune set, and the live segment-file count, and check that they match within a small slack.

    The test never calls the pruning API itself, and arm A never runs this check;
  - check that the prune lock doesn't move search p99 beyond noise.
- No fixed sleeps. Waits are on conditions with ≥ 10 s deadlines.

**Decisions file:** `docs/decisions/2026-11-03-X-mmap-cache-prune.md`, with the root cause, M1-exp's numbers, the lifecycle and performance results, and the delta note. It is search-side, not the crawler lifecycle; #402 already requires the re-soak.

**Size:** ≲ 400 lines, the harness included.

**Optional owner action:** an upstream tantivy issue (`delete`'s doc comment says it removes the cache entry, and it doesn't). The runner drafts it; the owner files it.

