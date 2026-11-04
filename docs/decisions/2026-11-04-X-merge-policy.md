# X-merge-policy decisions (#441)

Runner brief X-merge-policy, 2026-11-04 (Track R, the r4 contingency; auto-approved under owner §2.33). Soak r4 (rc.3, with #406's mmap prune) had a provisional slope over hours 12–14 of 23 MiB/h, 95% CI [15.6, 30.6] (the brief). Its later fit over hours 12–16.7 is 12.88 MiB/h, 95% CI [10.95, 14.81] (runner, 17:43Z); this file compares against that one. The runner's static analysis (`notes/r4-retainer-analysis.md` on runner-state) ranked tantivy's default `LogMergePolicy` as the first suspect. This file records Phase 1, a measurement with no production change.

**Outcome: a negative result, set down as a negative result.** Neither the merge policy nor the commit cadence passed the rule fixed in advance. The runner closed the slice after Phase 1 (2026-11-04, 18:01Z and 19:00Z): no Phase 2, no edit to `index.rs`. The commit cadence (M2) will be judged in its own slice, on disk and CPU grounds.

## Phase 1: the harness
- **`bench/examples/mem-merge.rs`.** The production writers have no hook for a merge policy, and `index.rs` was off-limits until SB-6 (#415) merged. So the harness builds both writers itself, the way `SearchIndex::writer`, `Side` and `Indexer::commit` do:
  - one indexing thread and `serve`'s 256 MiB heap, split the same way;
  - the real schemas, the name and trigram tokenizers, and the file index's settings;
  - files committed first, with the watermark payload (without the population part), then a manual reader reload;
  - #406's mmap-cache prune (`get_cache_info()`) after every commit.

  The documents come from the store, built with `TorrentDoc::new` and `files::file_docs`. The generator stores no enrichment, so the production path's `doc_enrichment` would be the default too.
- **Modes:** `ingest` (memory), `latency` (search during ingest, through the real `SearchIndex` search path) and `fresh` (discovery-to-searchable at a pace, with `live.rs`'s commit rule and its 500 ms poll).
- **Not exercised:** the extras in `live.rs`'s `follow` loop, which are the title set (`titles.committed()`), `IndexStats`, the commit hook and the population state; the `Retrying` directory wrapper; and the search-side query caches. The conclusions below cover the tantivy writers and store inserts that the harness drives.
- **Schema version:** every measurement ran on base 3d45e86, schema v5, before SB-6 (#415, #418) added the `name_trigram` field (v6). The harness was brought up to v6 when `main` was merged in, and the latency table was re-run on v6; the memory rounds weren't. A trigram field adds terms per document, so v6 segments are larger, which could shift the levels measured here.
- **Shared-file edits (`bench/Cargo.toml`):** a `tantivy` dev-dependency, at the same version and features as the search crate, and a `mimalloc-v2` feature for arm D.

## Rules, fixed before any result (15:41:01Z, sent to the runner before the first run finished)
- **Workload:** 20,000 synthetic torrents per run (about 22 files each, so about 440k file documents), real schemas, merges on, accelerated (each commit as soon as the last one finishes).
- **Arms, 3 runs each:**
  - A, B and D: 1 document per commit;
  - C: 6 documents per commit, the batching 30 s gives at 665 torrents/h.
- **Build:** static musl through `cargo zigbuild` (zig 0.17, as the image builds), mimalloc v3. One process per run, at most 3 at once on 4 cores.
- **The arms:**
  - **A:** default `LogMergePolicy`;
  - **B:** `LogMergePolicy` with `min_layer_size` 300 on both indexes, `max_docs_before_merge` 100,000 on the file index (the default 10M on the torrent index);
  - **C:** B at 6 documents per commit;
  - **D1:** A on mimalloc v2;
  - **D2:** A with `MIMALLOC_ALLOW_THP=0`.
- **Measure:** `Anonymous` from `/proc/self/smaps_rollup`, taking the least of the per-commit samples in each 250-torrent row.
- **Estimator:** per run, the OLS slope of MiB against thousands of torrents, from 2,000 torrents to the end, ± 1.96·SE (`analyze_cr0`'s `memory_fit` method). A per-hour figure is the slope × 0.665, a conversion at 665 torrents/h, not a measurement.
- **Pass rule (B over A; the same for C over A and C over B):** every B run's upper bound is below every A run's point estimate, and mean B is below mean A. Runs that conflict make the result inconclusive. D is diagnostic only.

## Phase 1 results
**Anonymous memory (musl, as shipped), MiB per 1,000 torrents.** Per run: slope [95% CI]. Arm: the mean of the 3 runs with a t-interval (df = 2).

| Arm | Runs | Arm mean | ≈ MiB/h at 665/h | Anon at 20k (MiB) |
|---|---|---|---|---|
| A default | 1.99 [1.54, 2.44] / 3.56 [3.17, 3.94] / 2.50 [2.05, 2.96] | 2.68 [0.70, 4.67] | 1.79 | 124 / 159 / 130 |
| B tuned | 1.39 [1.00, 1.78] / 2.41 [2.04, 2.78] / 3.33 [3.06, 3.59] | 2.38 [−0.03, 4.78] | 1.58 | 107 / 137 / 154 |
| C tuned, 6/commit | 2.15 [1.65, 2.65] / 1.29 [0.72, 1.87] / 1.04 [0.49, 1.59] | 1.49 [0.05, 2.93] | 0.99 | 143 / 131 / 145 |
| D1 A on mimalloc v2 | 1.46 [1.16, 1.75] / 1.32 [1.05, 1.60] / 2.05 [1.83, 2.27] | 1.61 [0.65, 2.57] | 1.07 | 114 / 100 / 115 |
| D2 A, THP off | 2.13 [1.70, 2.55] / 3.02 [2.60, 3.45] / 2.50 [2.15, 2.84] | 2.55 [1.43, 3.67] | 1.70 | 118 / 130 / 109 |

- **Under the frozen rule, B over A, C over A and C over B all fail.** The highest upper bounds (B 3.59, C 2.65) are above A's lowest point estimate (1.99).
- **Spread within an arm is larger than the gaps between arms.** The workload is deterministic, yet A's runs range from 1.99 to 3.56.
- **Merge activity, the same in every run of an arm:**
  - A at 20k ends with 8 / 15 segments, the largest 16,822 / 91,913 documents, and about 2,810 / 2,855 merges (torrent / file index);
  - B: 15 / 33 segments, the largest 2,619 / 49,040, about the same number of merges;
  - C: 16 / 30 segments, about 474 / 440 merges.
  - Merges are counted as commits after which the segment count fell. Two merges finishing between samples count once, so these are lower bounds, which matters most for B's multi-level policy.

**dhat (gnu), 1 run per arm, 20k torrents:**

| Arm | Live heap growth (KiB per 1k torrents) | Live heap at 20k | Heap high-water, whole run | Median per-row high-water above the floor |
|---|---|---|---|---|
| A | 4.7 [4.1, 5.3] | 5.0 MiB | 44.7 MiB | 35.5 MiB |
| B | 12.5 [11.2, 13.8] | 5.3 MiB | 42.3 MiB | 32.7 MiB |
| C | 9.5 [8.1, 11.0] | 5.2 MiB | 43.6 MiB | 32.6 MiB |

- **Above the floor** means a row's sampled high-water minus that row's least live heap: what the heap rose by at its peak.
- **The live heap is flat in every arm,** at about 5 to 13 bytes per torrent.
- **The high-water is about 40 MiB in every arm, and it doesn't grow with the corpus.** That fits a peak set by the writers' buffers, which are sized from the heap budget, rather than by the size of the merged segments. It's an inference: this run didn't attribute allocation sites.
- **The anonymous growth is the allocator keeping memory,** not live data, and the policy doesn't change it beyond the noise.
- **Caveat:** the per-row peak comes from a 2 ms sampler, which can miss short spikes.

**Search and commit latency during ingest:** 5,000 torrents, one arm at a time, gnu with mimalloc, schema v6. The searcher reopens the index each round of queries, because `SearchIndex::refresh` reloads only the torrent reader, and file clauses have to see the file index's current segments. (The first latency run missed this: its file clauses searched a stale snapshot, so it was discarded.) The corpus is small, so these figures show only whether anything regresses.

| Arm | Commit p50 / p95 | Search p50 / p95 | Segments at the end |
|---|---|---|---|
| A | 38.8 / 75.6 ms | 0.41 / 2.66 ms | 10 / 10 |
| B | 40.7 / 73.7 ms | 0.44 / 1.88 ms | 10 / 31 |
| C | 46.0 / 86.2 ms (6 documents a commit) | 0.45 / 2.74 ms | 9 / 30 |

- **No arm regresses search** beyond the noise, though B and C keep three times A's file segments.

**Discovery-to-searchable at 665 torrents/h:** 25 min per run, musl.
- C (B with 30 s commits): p50 15.1 s, p95 28.6 s, max 30.2 s (273 torrents).
- Today (A with 2 s commits): p50 0.3 s, p95 0.5 s, max 2.0 s (278 torrents).
- Both are far inside the 10 min guardrail.

## Round 2: a large corpus (150k)
**Why it ran.** Later, r4 showed steps of about 70–90 MiB that grew with the corpus (runner, 17:43Z). The 20k runs never build large segments, so a second round measured against a large index. The runner then reported that r4's corpus is small (12,365 documents at hour 16.8), and asked for this round to stop. The musl round had already finished, so it's reported, but it covers a regime the soak doesn't reach.

**Rules, fixed at 18:05:17Z before any result:**
- Each run seeds 150,000 torrents in 5,000-document commits, under its arm's own policy (the `--seed` flag).
- Then it drips 20,000 torrents: 1 document per commit for A and B, 6 for C.
- The fit runs from torrent 152,000 to the end.
- Arms, build, measure, estimator and pass rule are otherwise round 1's, with 3 runs per arm.

**Results, musl anonymous memory, MiB per 1,000 torrents:**

| Arm | Runs | Arm mean | Anon over the drip (MiB) | Largest step between rows (MiB) | Segments at the end |
|---|---|---|---|---|---|
| A default | 2.20 [1.83, 2.56] / 2.24 [1.83, 2.65] / 1.53 [1.12, 1.93] | 1.99 [0.99, 2.98] | 95→154 / 95→158 / 86→159 | 24 / 21 / 27 | 10 / 24 |
| B tuned | 5.05 [4.41, 5.69] / 1.01 [0.68, 1.34] / 2.03 [1.70, 2.36] | 2.70 [−2.52, 7.91] | 97→207 / 91→142 / 101→155 | 45 / 22 / 26 | 24 / 68 |
| C tuned, 6/commit | 2.75 [2.06, 3.44] / 2.05 [1.29, 2.81] / 3.64 [2.72, 4.55] | 2.81 [0.84, 4.78] | 123→180 / 99→187 / 89→210 | 34 / 42 / 34 | 25 / 58 |

- **Under the frozen rule, nothing passes.** B and C's means are above A's.
- **Every arm shows steps of 20–45 MiB.** The tuned policy leaves two to three times the segments and doesn't make the steps smaller.
- **A's slope at 150k (1.99 MiB/1k) is within round 1's interval for A (2.68 [0.70, 4.67]).** There's no sign that the ingest path's growth rises with the corpus.
- **dhat (gnu), the same 150k seed, then a 10k drip, 1 run each for A and C:**
  - The live heap is flat: A grows 1.6 KiB/1k [−1.1, 4.4] and ends at 5.1 MiB; C grows 10.1 KiB/1k [4.5, 15.7] and ends at 5.4 MiB.
  - The high-water is 44.7 MiB in A and 42.9 MiB in C. Above the floor, the median per row is 35.1 / 33.6 MiB and the largest 39.6 / 37.5 MiB.
  - The same high-water as at 20k: a merge's heap peak doesn't scale with the corpus. Index data is mapped, not heap.
  - So the 20–45 MiB anonymous steps are most likely the allocator keeping pages from that high-water after it falls. The policy doesn't change them.

## What it means
- **At r4's corpus size (round 1), the ingest path retains about 2.7 KiB per torrent.** r4 had 12,365 documents at hour 16.8, so the 20k runs cover it.
  - At r4's measured rate (about 683 stored torrents an hour, about one commit per torrent, runner 19:00Z), A's mean is about 1.8 MiB/h.
  - That point estimate is below the 2.0 MiB/h gate, but the interval doesn't rule out failing it: A's upper bound of 4.67 MiB/1k is about 3.2 MiB/h.
  - At 150k, A's slope is similar: 1.99 MiB/1k.
- **Neither the merge policy nor the commit cadence passes,** under the rule fixed in advance, in either round.
  - In round 1, C (M2's cadence, 6 documents a commit) has the lowest mean: 1.49 against A's 2.68 MiB per 1,000 torrents. Its runs overlap A's, so it fails the rule.
  - At 150k, C (2.81) doesn't improve on A (1.99), the opposite of round 1's small-corpus hint. M2 is to be judged on disk and CPU grounds, not on memory.
- **Most of r4's slope is outside what this harness drives** (the tantivy writers and store inserts). r4's 12.88 MiB/h over hours 12–16.7 is about 19 KiB per torrent at 683/h, against 2–2.7 KiB here. The `follow` loop's extras listed above weren't exercised, so this doesn't clear the title set or the commit hook.
- **The anonymous steps are a level, not a slope.**
  - dhat shows a flat live heap (about 5 MiB) and a high-water of about 40 MiB in every arm and at both corpus sizes. That fits a high-water set by the writers' heap-sized buffers rather than by the corpus or the merge size; no allocation sites were attributed.
  - The 20–45 MiB steps in anonymous memory are most likely mimalloc keeping pages from that peak.
- **The owner's soak review agrees.** `docs/code-review/2026-11-04-soak-memory-review.md` (PR #425) finds a whole-index rewrite about every seven commits below 16.8k documents under the default `LogMergePolicy`. Arm A shows exactly that: about 2,810 torrent-index merges in 20,000 one-document commits, one every 7.1 commits. Its largest segment stops at 16,822 documents, `min_layer_size` (10,000) × 2^0.75. The rewrite is real; what it leaves behind is the level above, not a growing slope.
- **mimalloc v2 (D1) is a lead, not a finding.** It has the lowest mean of the A-family runs (1.61 against 2.68) but doesn't pass the rule. THP off (D2) looks like A, though THP in production is under X-thp-off.
- **A recorded option, not taken: a 64 MiB writer heap** in place of `serve`'s 256 MiB.
  - If the buffers set the high-water (the inference above), the one-time level would fall by perhaps 20–30 MiB, at the cost of indexing throughput. That's unmeasured.
  - It doesn't touch the slope that fails r4.
  - It's worth measuring only if X-mem-gauges shows that mimalloc's retention matters at the next soak. It changes a number the owner set, so it's a §2.26 call for the runner.
- **The next leads are outside indexing,** in the runner's list: THP (X-thp-off), C heap outside mimalloc (X-mem-gauges) and the real-network fetch path, which no harness exercises.
