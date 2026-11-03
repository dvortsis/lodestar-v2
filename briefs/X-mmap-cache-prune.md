# X-mmap-cache-prune: stop tantivy's MmapDirectory cache growing with every commit (with M1-exp first)
**Model:** Opus (search core). Fresh Opus reviewer after. Runner brief, 2026-11-03. Owner, 2026-11-02: "Go with your recommendation but the memory fix is the priority." Plan reviewed externally (Gemini, Codex); this brief is the approved version.

## Root cause candidate
- tantivy 0.27.0 `MmapDirectory` keeps `HashMap<PathBuf, Weak<..>>` with one entry per file ever mmapped (`src/directory/mmap_directory/mod.rs:134-148`).
- `delete()` (`:404-417`) does not remove the entry, despite its doc comment. Dead entries are pruned only inside `get_cache_info()` (`:292-297`, which calls `remove_weak_ref` at `:111`), and nothing in Lodestar calls it.
- Lodestar opens both indexes on `MmapDirectory` (`crates/search/src/index.rs:588-593`). The live indexer commits after each catch-up batch whenever the 2 s deadline has passed (`live.rs:301-312`). At r3's ~665 torrents/h that is roughly one commit per torrent, and each commit writes a new segment (~6 files) in each index, plus merge outputs.
- Estimate: ~9k dead entries/h x ~230 B = ~2 MiB/h, which matches r3's 1.88 MiB/h [1.43, 2.33].
- Unproven until M1-exp's control reproduces it.

### M1-exp: the deciding experiment (R-mem-triage; prune versus default only)
Revised after external reviews (Gemini, then Codex, 2026-11-03). It has two parts with different jobs.

**Arms:**
- (A) default;
- (B) MmapCache prune only.

Merge policy stays out (see "Merge policy").

**Measurement never prunes the control.**
- `get_cache_info()` is never called in arm A, or in any assertion.
- Retained live allocations come from dhat's current bytes (a gnu-dhat variant). Anonymous memory comes from cgroup or `RssAnon` on the musl + mimalloc build as shipped. They're reported separately.

**Workload control.** `commit_docs` and `commit_every` don't pace anything. A commit comes after a catch-up batch of up to `batch` documents (`live.rs:301-312`), and `commit_every` is a deadline only while documents are pending. So the harness sets these explicitly:
- `batch = 1` and `commit_docs = 1`;
- a short `poll`;
- input written to the store at a fixed pace.

Every run records the documents per commit and commits per second it actually achieved, from the indexer's stats and `search_commits_total`.

**Part 1: the mechanism (accelerated).**
- About 10 one-document commits a second, for 1 h.
- 3 independent runs per arm, each on a fresh index, plus 1 pair on independent copies of a frozen fixture near a `LogMergePolicy` threshold, with segment sizes for both indexes recorded.
- **Output:** retained bytes per commit, as a slope of dhat current bytes and of anon against the commit count, over a window fixed in advance (commits 2,000 to the end).
- **Aggregation, fixed in advance:** each run's slope is reported, with the between-run spread. The arm estimate is the mean of the 3 run slopes with a t-interval (df = 2). Time samples are never pooled as independent.
- **The control must reproduce:** arm A's per-commit retention must match the expected dead-entry cost (files per commit × ~230 B, about 1–3 KB/commit). If it doesn't, the result is **inconclusive**: stop and diagnose (heap sites, allocator stats), and report. Don't change code, and don't move to M2 on that basis.
- **Pass for B:** in every run, B's per-commit retention is ≤ 10% of the A mean, and the dhat sites under `MmapCache::get_mmap` stop *accumulating*. Live mappings still allocate there.

**Part 2: production cadence (total growth). Nothing is extrapolated from Part 1.**
- Pace the input at r3's rate: about 665 torrents an hour, which is about one torrent per commit at the 2 s deadline. Idle intervals don't commit, so r3 made roughly 665 commits an hour, not 1.8k. Real batching and reindex replays add some; record the actual counts.
- 2 runs per arm, ≥ 6 h each, on the musl build as shipped. The window, minus the first 30 min, is fixed in advance.
- **Output:** the total anon MiB/h per run, against the expected ~1.5–2 MiB/h for arm A.
- This is supporting evidence only. **The frozen 24 h soak r4 stays the release gate.**

**Decision (runner, §2.26):** ship B when Part 1 passes and Part 2 shows B's total growth clearly below A's, and below 0.5 MiB/h.
- If Part 1 passes but Part 2 still leaves growth for B, M2 can follow as its own experiment with its own A/B.
- An inconclusive control is never what triggers M2.

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
    3. the harness ingests one more document to force a commit and a reader reload, which runs production's own prune;
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
**Trigger:** only as a separately justified experiment, when M1-exp Part 1 has passed and Part 2 still leaves arm B > 0.5 MiB/h. It's never triggered by an inconclusive control: that leads to diagnosis instead.

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
  - Add `search_commits_total` and `search_mmap_cache_entries` (with its `index` label, which the `SAFE_LABELS` pattern must accept) to `KEEP` in `docs/soak/tools/linux/collect.py` and the shared sanitizer.
- **Analyzer:**
  - Parse both in `analyze_cr0.py`: today it reads only recognized counters.
  - Report the labelled gauge per index.
  - `memory_context()` gains a slope per 1,000 commits, beside the per-1,000-torrents slope.
  - Older snapshots without the new metrics still analyze (the fields are reported absent).
- **Interim fits:** any fit over an incomplete window is printed as "provisional (window incomplete)", with no pass or fail. The bar is unchanged.
- **Tests:** an end-to-end fixture in `test_analyze_cr0.py`, from emitted metric text through the sanitized snapshot to the analysis output, plus a missing-metrics case.
- **Pinning:** r4's record pins the collector and analyzer git revision next to the rc.3 image digest. This merges before R1′ starts.

## Branch and process
- Branch `slice/X-mmap-cache-prune`. PR title "X-mmap-cache-prune: prune tantivy's mmap cache after each commit".
- Report M1-exp's Part 1 and Part 2 numbers to the runner **before** opening the PR, and again in the decisions file.
- Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round.
- CR-COL-4 can be a second small PR from you after M1 (branch `slice/CR-COL-4`), or the runner hands it to a helper. Say which you prefer.
