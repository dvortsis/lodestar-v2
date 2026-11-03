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
- **A second measure.** Point samples of the heap catch merges and the writer's arena in flight, MBs at a time. So each row also records the least value in its 100-commit window, and both measures are reported.

**Part 1: the mechanism.** Bytes retained per commit, over commit 2,000 to the end. Each run's OLS slope is given; the arm's figure is the mean of its runs with a t-interval (df = 2).

| Measure | A: default | B: prune |
|---|---|---|
| dhat, least per 100 commits | 2,322 / 2,287 / 2,306; mean **2,305** [2,261, 2,349] | 2.8 / 4.7 / 6.3; mean **4.6** [0.3, 8.9] |
| dhat, point samples | 2,008 / 2,494 / 2,497; mean 2,333 [1,634, 3,032] | 231 / −781 / 701; mean 51 [−1,831, 1,932] |
| dhat, fixture pair (least per 100) | 2,331 | 1.2 |
| musl anon (as shipped), least per 100; about 36,000 commits a run at 10/s | 3,215 / 2,808 / 2,846; mean **2,956** [2,398, 3,515] | 736 / 575 / 1,498; mean **936** [−288, 2,160] |

- **The control reproduces the expected 1–3 KB per commit.**
- **B passes on the windowed minimum:** each run is at most 0.3% of A's mean, against the ≤ 10% bar.
- **On point samples B's runs are too noisy to judge.** Two dhat snapshots of a live arm-B index, at 1,500 and 4,500 commits, put that difference in two or three large writer and merge blocks, with nothing under `MmapCache` accumulating.
- **Anonymous memory (musl, mimalloc) on its own measure:** B cuts the growth by about 68% but doesn't reach 10% of A. Its runs are 25%, 19% and 51% of A's mean.
  - From commit 12,000 on, two of the three B runs are flat (308 and −113 B/commit). The third still grows about 790 B/commit. A stays at 2.3–3.9 KB/commit.
  - The live heap (dhat) is flat in B, so the remainder is the allocator keeping memory, not data.
  - At r3's ~665 commits an hour, A's 2,956 B/commit projects to about 1.9 MiB/h, which matches r3's 1.88. B's 936 projects to about 0.6 MiB/h (about 0.2 MiB/h from commit 12,000 on). This is a projection, not a measurement; the soak is the gate.
- **The fixture** is a frozen copy of a 9,900-torrent index whose segments sit near `LogMergePolicy`'s thresholds: main [8,000, 1,000, 900] documents; files [220,643, 33,003, 29,845].

**Part 2: production cadence.** `serve`'s own `LiveConfig`, about 665 torrents an hour, on the musl build. The figure is total anonymous MiB/h, after the first 30 min.

- **Each run:** about 1,300 commits in 2 h, at 665 an hour with 1.00 document per commit.
- **Slopes:** A −3.6 and −9.9 MiB/h; B +1.3 and −0.4 MiB/h.
- **Inconclusive.** Two hours give only ten samples after the first 30 min, and ±10 MiB of anonymous-memory noise swamps the expected 1–2 MiB/h. It shows nothing either way; a 6 h run needs a host that stays up.

**The regression test** (`bench/tests/mmap_retention.rs`; `cargo test --release -p lodestar-bench --test mmap_retention -- --test-threads=1`):
- It drives the `Indexer` one document per commit, with merges on.
- It samples dhat's current bytes at commits 500 and 2,500, each sample being the least over its 100 commits, with the index alive.
- With pruning off it retains **2,501 B per commit** and fails. With pruning on it passes, under the 300 B bound.
- It never calls the pruning API.

## Lifecycle and performance
- **Lifecycle tests:**
  - one-document commits through the live indexer leave the gauge within a small slack of the live segment files, across merges;
  - opening an index starts the gauge at exactly its live files;
  - a RAM index reports none.
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
