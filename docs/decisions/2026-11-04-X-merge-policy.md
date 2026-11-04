# X-merge-policy decisions

Runner brief X-merge-policy, 2026-11-04 (Track R, the r4 contingency; auto-approved under owner §2.33). Soak r4 (rc.3, with #406's mmap prune) had a provisional slope over hours 12–14 of 23 MiB/h, 95% CI [15.6, 30.6]. The runner's static analysis (`notes/r4-retainer-analysis.md` on runner-state) ranked tantivy's default `LogMergePolicy` as the first suspect. This file records Phase 1, a measurement with no production change. **Status: Phase 1 is reported, and Phase 2 waits on the runner's ruling.**

## Phase 1: the harness
- **`bench/examples/mem-merge.rs`.** The production writers have no hook for a merge policy, and `index.rs` was off-limits until SB-6 (#415) merged. So the harness builds both writers itself, the way `SearchIndex::writer`, `Side` and `Indexer::commit` do:
  - one indexing thread and `serve`'s 256 MiB heap, split the same way;
  - the real schemas, the name tokenizer and the file index's settings;
  - files committed first, with the same payload, then a manual reader reload;
  - #406's mmap-cache prune (`get_cache_info()`) after every commit.

  The documents come from the store, built with `TorrentDoc::new` and `files::file_docs`. The generator stores no enrichment, so the production path's `doc_enrichment` would be the default too.
- **Modes:** `ingest` (memory), `latency` (search during ingest, through the real `SearchIndex` search path) and `fresh` (discovery-to-searchable at a pace, with `live.rs`'s commit rule and its 500 ms poll).
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

**dhat (gnu), 1 run per arm, 20k torrents:**

| Arm | Live heap growth (KiB per 1k torrents) | Live heap at 20k | Heap high-water, whole run | Median high-water per 250-torrent row, above the floor |
|---|---|---|---|---|
| A | 4.7 [4.1, 5.3] | 5.0 MiB | 44.7 MiB | 35.5 MiB |
| B | 12.5 [11.2, 13.8] | 5.3 MiB | 42.3 MiB | 32.7 MiB |
| C | 9.5 [8.1, 11.0] | 5.2 MiB | 43.6 MiB | 32.6 MiB |

- **The live heap is flat in every arm,** at about 5 to 13 bytes per torrent.
- **The high-water is about 40 MiB in every arm, and it doesn't grow with the corpus.** A merge peak is dominated by the writers' arenas, not by the size of the merged segments, at this corpus size.
- **The anonymous growth is the allocator keeping memory,** not live data, and the policy doesn't change it beyond the noise.
- **Caveat:** the per-row peak comes from a 2 ms sampler, which can miss short spikes.

**Search and commit latency during ingest:** 5,000 torrents, one arm at a time, musl. The corpus is small, so these show only that nothing regresses.

| Arm | Commit p50 / p95 | Search p50 / p95 | Segments at the end |
|---|---|---|---|
| A | 29.5 / 57.1 ms | 0.11 / 0.33 ms | 10 / 10 |
| B | 29.3 / 53.0 ms | 0.13 / 0.39 ms | 10 / 31 |
| C | 29.8 / 54.8 ms | 0.12 / 0.37 ms | 9 / 30 |

**Discovery-to-searchable at 665 torrents/h:** 25 min per run, musl.
- C (B with 30 s commits): p50 15.1 s, p95 28.6 s, max 30.2 s (273 torrents).
- Today (A with 2 s commits): p50 0.3 s, p95 0.5 s, max 2.0 s (278 torrents).
- Both are far inside the 10 min guardrail.

## What it means
- **At 1 document per commit, the merge policy is not r4's retainer.** All of A's ingest-path growth converts to about 1.8 MiB/h at 665 torrents/h, about 2.7 KiB per torrent. r4's provisional 23 MiB/h is roughly 35 KiB per torrent at that pace. So even removing the whole ingest-path growth would explain under a tenth of it.
- **The tuned policy and the longer cadence don't measurably cut it.** C's mean is the lowest, but under the frozen rule it isn't distinguishable from A.
- **mimalloc v2 (D1)** has the lowest point estimates among the 1-document arms, but it isn't separable either. THP off (D2) looks like A.
- **The next suspects** (`notes/r4-retainer-analysis.md`): the real-network fetch path, which no harness has exercised, and per-commit thread churn. Phase 1 points away from merging, toward those.
