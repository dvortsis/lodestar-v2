# Plan: fix r3's memory growth, cut rc.3, re-soak, and fold the other soak findings into the roadmap

Runner, 2026-11-03. Owner-approved (`docs/owner-decisions.md` §2.31) after external reviews by Gemini and Codex. The decision record is `docs/decisions/2026-11-03-soak-r3-verdict.md`; this file is the working plan it points to. The "M1 decision sequence" section is authoritative and was frozen at 03:49Z, before the confirmation batch reported. Status lines are as of 2026-11-03 ~04:00Z; live status is in the build plan §0 and on the dashboard.

## Context
**Soak r3 (rc.2, Linux Docker) failed the memory gate, narrowly.**
- **The official result** (analyze_cr0 on main e528c369): `private_bytes` over baseline h12–24 grew at **1.88 MiB/h, 95% CI [1.43, 2.33]**, from 719 samples. The bar is an upper bound ≤ 2.0 (`docs/decisions/2026-10-30-CR-0-acceptance.md:9`, `MEMORY_FIT` in `analyze_cr0.py`).
- **Everything else was clean:** 24.00 h of coverage (100%), a clean shutdown, no evidence issues, 0 writer and 0 search errors, and a peak of 481 MiB.

**Owner, 2026-11-02:**
- Go with the runner's recommendation: **rc.3 = rc.2 + #357 + #402 + the memory fix**, then one fresh frozen soak.
- **The memory fix is the priority.**
- Launches stay paused (two slots, §2.30), except work done by the existing sessions.

**Root cause, found in the code and checked against tantivy 0.27.0:**
- **The map that leaks:** tantivy's `MmapDirectory` keeps a `HashMap<PathBuf, Weak<…>>` with one entry for every file it has ever mmapped (`tantivy-0.27.0/src/directory/mmap_directory/mod.rs:134-148`). Despite its doc comment, `delete()` doesn't remove the entry (`:404-417`). Dead entries are pruned only inside `get_cache_info()` (`:292-297`), and nothing in Lodestar calls it.
- **Why it grows so fast here:** Lodestar opens both indexes on `MmapDirectory` (`crates/search/src/index.rs:588-593`). The live indexer commits every 2 s (`crates/search/src/live.rs:41-43`), which at soak rates (~665 torrents/h) is about one commit per torrent. Each commit writes a new segment (~6 files) in each index, plus merge outputs.
- **The estimate:** ~9k dead entries/h × ~230 B ≈ **2 MiB/h**, which matches the measured slope.
- **Why triage missed it:** its harness ran 20–800 torrents per commit. r3's link with index bytes was a proxy: both grow with commits. Its 10/s run (~1k commits) would leak only ~1–2 MiB, inside the noise.

**A second candidate, from R-mem-triage's dhat and musl runs (23:57Z):** merge churn.
- **The mechanism:** LogMergePolicy re-merges every document at each level. Each merge allocates buffers sized to its inputs (`Vec<DocAddress>` doc maps, the remap in `write_postings_for_field`), and the allocator keeps the high-water.
- **The evidence:** with NoMergePolicy at 400/s, retention fell from 1,276 to 678 B/torrent. At 10/s the result was inconclusive.
- **Why it matters:** it is bounded only by tantivy's `max_docs_before_merge` (10M by default), so in practice it grows with the corpus.
- **Ruled out:** mimalloc `purge_delay=0` and `mi_collect`.

**So the fix is decided by an experiment, not by a hypothesis:** prune-only against an uncontaminated default (M1-exp below). Merge policy is revisited only if growth remains.

**Other soak findings to place:**
- the step from 98 to 283 MiB during warmup, which is the fetch pool filling to its 200k cap and is bounded;
- the crawler yield (665 stored/h ≈ 11/min; 4.7% fetch success; 106M capacity rejections; 40.9M peer-command-full events; 5.5M unaccepted evictions);
- uTP at 0, expected while its switch is off;
- search under load untested, because no queries ran (all caches 0);
- the misleading early fit (6.66 MiB/h over h12–16);
- the analyzer's `collector_window_complete_review_required` line;
- the earlier open items: the `writer.rs:592` expect, RSS after drain, and the glibc zstd arena.

## Status (03:40Z)
**Done**
- Step 0, #377 merged, and #403 merged.
- The #402 patch still applies cleanly on `release/v0.5.1` at 0acc5a37.

**Part 1 first batch on a fresh index** (dhat `curr_bytes` on gnu; bytes kept per commit over commit 2,000 to the end; 3 runs per arm; mean with a df = 2 t-interval):
- **A, no fix:** 3,016, 2,246 and 2,875 B. Mean **2,712 [1,694, 3,731]**. The control reproduces.
- **B, with the prune:** 668, 941 and 498 B. Mean **702 [147, 1,257]**, about 74% less than A. Its residual isn't explained yet.
- **Achieved:** exactly 1 document per commit, at about 3.1–3.5 commits/s. The runs shared 4 cores.

**The regression test:** 2,501 B/commit with pruning off (fails); under 300 B/commit with it on (passes).

**The branch** `slice/X-mmap-cache-prune` is at ee12b407: fix, gauge, harness, regression test and lifecycle tests. No PR yet.

**R-mem-triage's container restarted at about 03:11Z.** It relaunched Part 1 (ends about 04:15Z) and Part 2 (ends about 09:45Z), with a check-in every 25 min.

**Done after approval (03:49Z)**
- The decision sequence is frozen and published to runner-state as `briefs/X-mmap-cache-prune-decision-sequence.md` (6edacd0b).
- R-mem-triage has the sequence: the confirmation batch, arm L, configs and isolation, the Step 3 branches, and the smoke-test prune wait.

**Next**
- **CR-COL-4 to a Sonnet helper** (R-mem-triage asked; it's a runner subagent, not a new session). It works on `slice/CR-COL-4` in its own worktree with the gauge exactly as built, `lodestar_search_mmap_cache_entries{index="main|files"}`, and opens a PR. Then a fresh Opus review, merged before R1′. It also trims build-plan §0.9's stale "X-strip-format-chars is already running".

## Progress (2026-11-03)
- **Step 0.1 done:**
  - the brief is on runner-state at `briefs/X-mmap-cache-prune.md` (4e64872b);
  - R-mem-triage has the MmapCache finding, the revised M1-exp and the M1 brief;
  - its two 4/s runs finish as a screen only.
- **Steps 0.2–0.4 done:** #377 merged (0acc5a37), #403 merged (c8c39914), runner-state updated.

## Step 0: records (runner, right after approval)
1. **Message R-mem-triage** (session_01MMjhnD9Jd88Vzmq6W9Bxro, an existing session). Its current two runs at 4/s can't tell the two causes apart: arm (b) also cuts the MmapCache leak, and they don't count dead entries. Let them finish as a screen, then hand over:
   - the MmapCache finding (tantivy `mod.rs` lines above);
   - the revised M1-exp below;
   - the M1 brief.
2. **Merge #377** (#357 onto `release/v0.5.1`, head 84821101) after rechecking that it's clean and ci-ok is green. Its contents are now owner-approved as part of rc.3.
3. **Runner docs PR** on `claude/busy-allen-aptli2`:
   - **`docs/owner-decisions.md` §2.31:** the owner's answer quoted: rc.3 contents, the memory fix as priority, launches stay paused apart from the existing sessions, and the soak containers stopped at the owner's convenience.
   - **`docs/decisions/2026-11-03-soak-r3-verdict.md`:** the official lines, the review the analyzer asks for (its `review_required`), the root cause, and the finding placements below.
   - **`docs/build-plan.md` §0 Track R table:** R1 = FAIL (memory), plus the new steps R1-fix, R3′ and R1′ below. §2.29's "rc.3 = rc.2 + #357" is superseded by §2.31.
   - **`runner/slices.yaml`:** add X-mmap-cache-prune, R-rc3-cut and CR-COL-4 (one-line flow style), and update G-v051's notes.
   - **`runner/dashboard.yaml`:** the soak step, the owner_queue n 1 and n 5 items (#367 stays a separate question), the notes, and fix the stale "Four slots" note.
   - Run `cd tools/status-connector && npm test`.
4. **runner-state:** slices, soak_notes, and the owner checklist (mark rc-next-include-402 answered).

## Track R, the new sequence (priority 1)
| Step | What | Who | Exit |
|---|---|---|---|
| **R1-fix (M1)** | M1-exp, then X-mmap-cache-prune on `main` | R-mem-triage (existing) | Steps 1–4 of the M1 decision sequence: mechanism passed, residual attributed, production growth reported; tests, a fresh Opus review and green CI; merged |
| **M2** (only if needed) | Commit cadence, through the same PR, backport, rc and soak sequence | the same session | Only under Step 5 (Step 3 leaves B's upper bound > 1.0 MiB/h). The combined M1 + M2 is validated at production cadence before R3′. |
| **R3′ (R-rc3-cut)** | Bring #402 (the staged `patches/rc3-402-on-rc2.patch`), M1 and M2 if it shipped onto `release/v0.5.1` | R-rc3-backport (session_01MMjhn8jpt1tX7E6SGauJH8, existing) | Release PR green, Opus review, merged. The owner tags `v0.5.1-rc.3`. |
| **R1′** | Soak r4 on rc.3: Linux main leg, frozen image, 6 h suppression + 30 min warmup + 24 h baseline | owner | analyze_cr0 gate evidence: yes, memory upper CI ≤ 2.0 |
| **R2** | The gluetun leg (≥ 2 h) on rc.3 | owner | as before |
| **R-win** | Short Windows MSI service smoke on rc.3 (≥ 1 h: start, stop, clean restart, non-zero stored rate). #402 and M1 are platform-independent, so a full re-soak isn't needed (runner, delta rule). | owner | pass |
| **R3b** | The owner tags `v0.5.1` | owner | — |

**On another FAIL:** the same loop (fix on `main`, cherry-pick, new rc, fresh frozen soak), never stitched.

### M1 decision sequence (authoritative; replaces every earlier M1-exp rule, amendment and threshold)
Revised after external reviews by Gemini and then Codex (2026-11-03, twice). Every number and rule in this section was set before the remaining results arrived. The first Part 1 batch was seen before revision 2; the fix PR's decisions file (`docs/decisions/2026-11-03-X-mmap-cache-prune.md`) states that.

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
  - This rule was **chosen after seeing the first batch.** It is labelled that way in `docs/decisions/2026-11-03-X-mmap-cache-prune.md`, the fix PR's decisions file. It is **frozen now, at 03:49Z, with the endpoint (commits 2,000 to the end of each 1 h run), before the confirmation batch relaunched at 03:13Z reports.** That batch is the confirmation; the first batch is exploratory.
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

### Merge policy (closed unless reopened)
**Out of the release path.**

**Reopened only if B leaves residual growth.** Then it's a separate slice with its own decision, because `max_docs_before_merge` limits which *input* segments qualify, not the output size. Lowering it can raise segment count, search fan-out and retained deleted documents. The file index counts file documents, so it needs its own number.

**Before any change ships:** query latency at 10M, disk growth, and reindex and delete behaviour on the release schema.

### M2: commit cadence (conditional; the runner decides under §2.26)
**Trigger:** Step 5 of the M1 decision sequence only. It's never triggered by an inconclusive control.

**Change:** commit at ≥ 30 s or 5,000 docs, instead of every 2 s (`live.rs` `LiveConfig`).

**Guardrail:** discovery-to-searchable p95 ≤ 10 min. Flag the UX change to the owner (§2.19).

**If shipped, it goes through the same sequence as M1:**
- a PR on `main` with its own M1-exp-style A/B;
- a fresh Opus review;
- the release backport (R3′ carries it);
- the rc tag;
- soak r4.

**Fallback:** the allocator options were already ruled out by triage (`purge_delay=0`, `mi_collect`). Report to the owner before going further.

### CR-COL-4: collector and analyzer, end to end (same session or a helper; not in the release)
- **Collector:**
  - Add `search_commits_total` and `search_mmap_cache_entries` (with its `index` label, which the `SAFE_LABELS` pattern must accept) to the `STORAGE` tuple (which feeds `KEEP`) in `docs/soak/tools/linux/collect.py` and the shared sanitizer.
- **Analyzer:**
  - Parse both in `analyze_cr0.py`: today it reads only recognized counters.
  - Report the labelled gauge per index.
  - `memory_context()` gains a slope per 1,000 commits, beside the per-1,000-torrents slope.
  - Older snapshots without the new metrics still analyze (the fields are reported absent).
- **Interim fits:** any fit over an incomplete window is printed as "provisional (window incomplete)", with no pass or fail. The bar is unchanged.
- **Tests:** an end-to-end fixture in `test_analyze_cr0.py`, from emitted metric text through the sanitized snapshot to the analysis output, plus a missing-metrics case.
- **Pinning:** r4's record pins the collector and analyzer git revision next to the rc.3 image digest. This merges before R1′ starts.

## Other soak findings: where they go (no new launches)
| Finding | Placement |
|---|---|
| **Pool step to ~283 MiB at warmup** (200k cap × ~690 B/key) | Bounded and within §1.6.3. **Flag for E-v06-workers:** a worker's ≤ 256 MB budget (§1.6.5) can't carry a 200k pool, so size the worker pool cap there. A note on the epic. |
| **Yield of 11 stored/min**, 4.7% success, 106M capacity rejections, 40.9M command-full, 5.5M unaccepted evictions | Record r3 as the **Linux rc.2 crawler baseline** in the r3 verdict file. It feeds Track C as is (CR-ADM / CR-AB-ADM, CR-1a, CR-1c). Command-full and unaccepted evictions are the CR-ADM hypothesis, so no new slice. |
| **uTP 0 attempts** | Expected: `dht.utp` is off (CR-4). Nothing to do. |
| **No search load during the soak** | Keep the official protocol unchanged, so runs stay comparable. Concurrent search during commits and merges is covered by M1's bounded search smoke test (searches overlapping commits, mappings reclaimable after, prune lock against search p99). Query-time RSS at scale stays in the 10M gate. |
| **The early 6.66 fit misled** | CR-COL-4's "provisional" labelling, plus a runner rule: no verdict claims before the analyzer says the window is complete. |
| **`collector_window_complete_review_required`** | The runner's acceptance review is recorded in the r3 verdict file (and in r4's when it passes). |
| **`writer.rs:592` `expect`** (import smoke finding 2) | Check whether SRC-14a fixed it; if not, X-writer-join-error (small, Opus) after Track R. |
| **RSS after drain, glibc zstd arena** | Stay in the import-pilot and gnu-build notes. Not on the release path. |

## Roadmap order after approval
1. Step 0 records, and #377 merged.
2. **M1-exp, then M1** (R-mem-triage) → review → merge. CR-COL-4 alongside it.
   - If M2 is needed: M2's experiment and PR, then validate the combined M1 + M2 at production cadence → review → merge.
3. **R3′** (R-rc3-backport): #402 + M1, plus M2 if it shipped, onto the release branch → the owner tags rc.3.
4. **R1′** soak r4, then R2 gluetun and R-win → the owner tags v0.5.1.
5. Launches stay paused until the owner says. When they resume (2 slots), the ladder in §0.2 is unchanged: Track C (CR-AB-ADM with the r3 baseline), then Track S, and so on.

## Verification
- **M1 decision sequence:**
  - every run's full configuration is recorded, with its achieved batching, backlog and resource limits;
  - measurement is isolated per process or cgroup;
  - Step 1's control check and pass;
  - Step 2's site attribution, the regression-test gap explained, and L reported in bytes per torrent;
  - Step 3's per-run MiB/h with CIs, marked shortened if it was.
- **M1 PR:**
  - the isolated release-mode dhat test (the exact command above) fails with pruning off and passes with it on;
  - the lifecycle tests pass;
  - commit and search latency are reported;
  - the concurrent-search smoke test passes;
  - `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass;
  - a fresh Opus review, and green ci-ok.
- **CR-COL-4:** the end-to-end analyzer test passes, including snapshots without the new metrics.
- **R3′:**
  - the release branch builds;
  - pipeline 17/17 and the search tests pass on release;
  - all workflows dispatched on the rc.3 SHA (named checks green, no regenerated baselines).
- **R1′:**
  - analyze_cr0 reports memory upper CI ≤ 2.0;
  - the mmap gauge tracks live segment files;
  - coverage ≥ 24 h, gate evidence yes;
  - the collector and analyzer revision are pinned with the image digest.
- **Docs PR:** status-connector `npm test` passes.
