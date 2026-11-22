# X-mmap-cache-prune decisions

Runner brief X-mmap-cache-prune, 2026-11-03 (owner: "the memory fix is the priority"). Soak r3 of rc.2 missed the memory gate narrowly: `private_bytes` over baseline hours 12–24 grew 1.88 MiB/h, 95% CI [1.43, 2.33], against an upper bound of ≤ 2.0. This PR fixes the cause M1-exp confirmed.

## Root cause
- **What leaks:** tantivy 0.27's `MmapDirectory` caches a `Weak` for every file it maps, keyed by path (`src/directory/mmap_directory/mod.rs`, `MmapCache::get_mmap`).
- **Why the entries outlive their files:** `delete()` removes the file but not its cache entry, although its doc comment says it does. Only `get_cache_info()` drops dead entries (`remove_weak_ref`), and nothing in Lodestar called it.
- **What each dead entry holds:** the full path, a map slot, and the `Arc`'s inner allocation, which the `Weak` keeps alive.
- **Why the soak hits it:** every commit writes a new segment, about six files in each of the two indexes, and merges write more. At a live crawl's pace that's about one commit per torrent.
- **Why the triage harnesses missed it:** they committed every 20–800 torrents, so they made too few commits.

## The fix
- **Where:** `SearchIndex` keeps a clone of each `MmapDirectory`; the clones share the caches the indexes use.
  - `Indexer::commit` prunes both caches after both reloads, so files the reload replaced are already unmapped unless a searcher still holds them.
  - Opening an index (`open`, `open_existing`) prunes once.
  - A rebuild's swap needs no call of its own: the new index was opened and pruned, and its commits prune; the old one's directories are dropped whole.
- **What a prune costs:** each takes the cache's write lock and scans its entries, a few hundred live files. Commit and search latency are measured below.
- **Scope:** no tantivy fork or upgrade. It cherry-picks onto rc.2.
- **A hidden switch, `SearchIndex::set_mmap_cache_pruning(bool)`, turns pruning off.** Only the regression test and M1-exp's control arm use it.
- **The gauge `lodestar_search_mmap_cache_entries{index="main|files"}`** reports the live mappings each cache kept at its latest prune (`docs/metrics.md`).
  - It can't count dead entries, because a prune drops them.
  - It is judged against the live segment files. It grows with legitimate segments.

## M1-exp: the deciding experiment
**How it ran:**
- `bench/examples/mmap-drip.rs` writes synthetic torrents to the store at a fixed pace, and the real `LiveIndexer` follows with `batch = 1`, `commit_docs = 1` and a 10 ms poll.
- Every run had 1.000 documents per commit.
- Arm A (default) never prunes during the run. Arm B is the production prune.

**Disclosed deviations from the brief:**
- **Commit rate.** The machine has 4 cores shared by up to 12 runs, so Part 1 reached about 2.5–2.7 commits/s under dhat (about 9,400–9,700 commits a run) and the full 10/s on the musl build. The brief asked for 10/s.
- **One prune in arm A.** It prunes once at open, before the drip starts.
- **Shorter Part 2.** The cloud container is reclaimed a few minutes after a turn ends, so Part 2 ran for 2 h, not the brief's ≥ 6 h.
- **A second measure, chosen after the decision sequence was frozen.** Point samples of the heap catch merges and the writer's arena in flight, MBs at a time (the dhat site diff below shows this). So each row also records the least value in its 100-commit window, and both measures are reported. The regression test already sampled least-of-100 before the freeze.
- **Two batches.** The first Part 1 batch (00:54Z) counts as exploratory; the frozen sequence's Step 1 rule was set after it (03:49Z). The confirmation batch was launched at 03:13Z, killed when the container was reclaimed at about 03:35Z, and relaunched unchanged at 03:39Z; the 03:39Z runs are the ones reported.
- **Arm L (Step 2's supporting evidence) wasn't run.** The runner made it optional (04:10Z: "diagnostic: report if finished, don't wait"); Step 2's attribution rests on the dhat site diff alone.

**Part 1: the mechanism.** Bytes retained per commit, over commit 2,000 to the end. Each run's OLS slope is given; the arm's figure is the mean of its runs with a t-interval (df = 2).

| Measure | A: default | B: prune |
|---|---|---|
| dhat, least per 100 commits | 2,322 / 2,287 / 2,306; mean **2,305** [2,261, 2,349] | 2.8 / 4.7 / 6.3; mean **4.6** [0.3, 8.9] |
| dhat, point samples | 2,008 / 2,494 / 2,497; mean 2,333 [1,634, 3,032] | 231 / −781 / 701; mean 51 [−1,831, 1,932] |
| dhat, fixture pair (least per 100) | 2,331 | 1.2 |
| musl anon (as shipped), least per 100; about 36,000 commits a run at 10/s | 3,215 / 2,808 / 2,846; mean **2,956** [2,398, 3,515] | 736 / 575 / 1,498; mean **936** [−288, 2,160] |

- **The control reproduces:** A's mean is within the frozen 1–3.5 KB/commit on both measures.
- **Step 1 under the frozen rule is inconclusive.** On the frozen estimator (point samples) the arms' intervals overlap: A [1,634, 3,032], B [−1,831, 1,932]. The site criterion is met: nothing under `MmapCache::get_mmap` accumulates in B.
- **On the windowed minimum (the post-freeze deviation)** the intervals are far apart: A 2,305 [2,261, 2,349], B 4.6 [0.3, 8.9]; the fixture pair gives 2,331 against 1.2.
- **The runner's ruling (§2.26, 2026-11-03 05:00Z and 05:50Z; the runner flags it to the owner under §2.19):** "Under the frozen Step 1 rule and estimator, Step 1 is inconclusive, not passed. The windowed-minimum estimator is a deviation chosen after the freeze. On that basis the runner accepts the mechanism as shown," conditional on the musl anon Part 1 and the shortened Part 2 showing no adverse result under the frozen Step 3 rules. Then: "go. Ship B as M1, alone, without M2." The withdrawn ≤ 10% rule isn't used anywhere.
- **Why point samples are noisy.** Two dhat snapshots of a live arm-B index, at 1,500 and 4,500 commits, put that difference in two or three large writer and merge blocks, with nothing under `MmapCache` accumulating.
- **Anonymous memory (musl, mimalloc), as measured:** A 2,956 [2,398, 3,515], B 936 [−288, 2,160] B/commit. As point estimates that's about a 68% cut; B's interval includes 0.
  - From commit 12,000 on, two of the three B runs are flat (308 and −113 B/commit). The third still grows about 790 B/commit. A stays at 2.3–3.9 KB/commit.
  - The live heap (dhat) is flat in B, so the remainder is the allocator keeping memory, not data.
- **The fixture** is a frozen copy of a 9,900-torrent index whose segments sit near `LogMergePolicy`'s thresholds: main [8,000, 1,000, 900] documents; files [220,643, 33,003, 29,845].

**Part 2 (Step 3): production cadence, shortened.** `serve`'s own `LiveConfig`, about 665 torrents an hour, on the musl build, `RssAnon`-equivalent per process (`Anonymous` from smaps_rollup). Each run's slope is `analyze_cr0`'s `memory_fit` (OLS ± 1.96·SE) of anonymous MiB/h after the first 30 min. Shortened to 2 h per run (the frozen sequence allows ≥ 2 h) because the container is reclaimed a few minutes after each turn.

| Run | Slope, MiB/h [95% CI] |
|---|---|
| A1 | −3.56 [−8.97, +1.86] |
| A2 | −9.87 [−18.29, −1.45] |
| B1 | +1.32 [−5.82, +8.47] |
| B2 | −0.41 [−8.30, +7.48] |

- **Each run:** about 1,300 commits in 2 h, at 665 an hour with 1.00 document per commit; ten samples after the first 30 min.
- **The frozen branch: A flat and B flat, inconclusive.** M1 goes ahead on Step 1's proof; r4 decides. Neither arm shows growth: B's intervals include 0, A1's includes 0 and A2's is entirely negative. B isn't "clearly below" A: its upper bounds exceed A's points, and its mean sits above A's only because both A runs came out negative.
- **M2 isn't triggered.** Under the inconclusive branch it doesn't arise; r4 decides whether the allocator retention needs it. Nothing from Part 1 is extrapolated to MiB/h.

**The regression test** (`bench/tests/mmap_retention.rs`; `cargo test --release -p lodestar-bench --features dhat-heap --test mmap_retention -- --test-threads=1`):
- It drives the `Indexer` one document per commit, with merges on.
- It samples dhat's current bytes at commits 500 and 2,500, each sample being the least over its 100 commits, with the index alive.
- With pruning off it retains **2,501 B per commit** and fails. With pruning on it retains **34 B per commit** and passes, under the 300 B bound.
- It never calls the pruning API.
- **Its own CI job, `mmap-retention`**, path-filtered to `crates/search/**` and `bench/**` and required by `ci-ok`: it takes **193 s** on a quiet 4-core machine. `required-features = ["dhat-heap"]` keeps `cargo test --workspace` from building it in debug; that's the one change to the brief's exact command.
- **Why it retained < 300 B/commit while B's first-batch runs showed 498–941:** those were point-sample slopes, which include the transient writer and merge blocks; on the test's own least-of-100 estimator, the experiment's B is 4.6 B/commit. No second retainer: the snapshot diff found none.

## Lifecycle and performance
- **Lifecycle tests (`crates/search/src/live.rs`):**
  - one-document commits through the live indexer, merges and their garbage collection included, leave the gauge within a small slack of the live segment files;
  - a failed commit, before or after it reaches the disk: the indexer recovers without a panic, and the next commit's prune leaves the caches within the live files;
  - opening an index starts the gauge at no more than its live files;
  - a RAM index reports none.
- **Not tested separately:** a rebuild's swap. The new index is opened through `SearchIndex::open`, which prunes, and the old index's directories go when its last clone (and lease) does, so there's nothing left to prune.
- **No prune counter:** the prune runs synchronously inside each commit, unthrottled, so the smoke test can read the gauge right after the final commit. A throttled prune would need one.
- **Latency:** 3,000 one-document commits through the `Indexer`, with title and file searches running continuously on another thread:

| | Commit p50 / p99 | Search p50 / p99 (~600k searches) |
|---|---|---|
| Without pruning | 18.4 / 42.4 ms | 0.07 / 0.95 ms |
| With pruning | 19.2 / 44.7 ms | 0.07 / 1.01 ms |

Both differences are within run-to-run noise.
- **Reclamation**, in the brief's order: searches stop, merges finish and are collected, then one more commit runs production's prune. After that the gauge reads 30 (main) and 30 (files) against 30 and 30 live segment files: an exact match.

## Delta and soak
- **Delta:** search-side only (`crates/search`, plus the gauge in `crates/lodestar`). It touches no crawler lifecycle, sampler or endpoint policy line.
- **Re-soak:** #402 already requires one, and rc.3 = rc.2 + #357 + #402 + this fix goes to the frozen soak r4, which stays the gate.
- **Upstream:** tantivy's `delete()` doc comment promises what it doesn't do. The runner drafts the issue and the owner files it.
