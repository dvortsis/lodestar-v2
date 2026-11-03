# Soak r3 verdict: memory gate failed, fix sequence and crawler baseline

Runner, 2026-11-03. The owner's answer is `docs/owner-decisions.md` §2.31. Build-plan §0.2 and §0.4.

## Verdict
- **Source:** `analyze_cr0` at `main` e528c369, run by the owner on a copy of the evidence.
- **The memory gate misses the bar.** `private_bytes` over baseline hours 12–24: OLS slope **1.88 MiB/h, 95% CI [1.43, 2.33]**, from 719 samples, with the window complete. The bar is an upper bound ≤ 2.0 MiB/h (`docs/decisions/2026-10-30-CR-0-acceptance.md`), and 2.33 is above it.
- **Everything else was clean:**
  - peak 480.74 MiB;
  - valid coverage 24.00 h (100.01%), stop reason `baseline_window_complete`;
  - clean shutdown, no evidence issues, gate evidence yes.
- **The analyzer's line:** "Baseline: collector_window_complete_review_required".

## The runner's acceptance review
That line asks for a review by the runner, not for a pass.
- **Accepted:** coverage, shutdown and evidence.
- **Memory gate fails, so R1 = FAIL.** The release isn't tagged from rc.2.
- **The early fit misled.** The fit over hours 12–16 (6.66 MiB/h) was not a verdict, and the full window came in far lower. From now on no verdict is claimed before the analyzer reports the window complete (CR-COL-4 also labels any earlier fit "provisional").

## Root cause (candidate, to be confirmed by M1-exp)
- **Tantivy's `MmapDirectory` cache.** It keeps a `HashMap<PathBuf, Weak<…>>` with one entry for every file it has ever mapped (tantivy 0.27.0, `src/directory/mmap_directory/mod.rs`). `delete()` doesn't remove the entry, despite its doc comment. Dead entries are pruned only inside `get_cache_info()`, and nothing in Lodestar calls it.
- **Why it grows so fast here.** Both indexes open on `MmapDirectory` (`crates/search/src/index.rs`). The live indexer commits every 2 s (`crates/search/src/live.rs`), about one commit per torrent at soak rates. Each commit writes a new segment (about 6 files) per index, plus merge outputs. That is about 9k dead entries an hour at about 230 B each, roughly 2 MiB/h, which matches the measured slope.
- **Why R-mem-triage missed it.** Its harness ran 20–800 torrents per commit, so it made few commits. r3's link with index bytes was a proxy: both grow with commits.
- **Second candidate, merge churn.** `LogMergePolicy` re-merges every document at each level, each merge allocates buffers sized to its inputs, and the allocator keeps the high-water mark. With `NoMergePolicy` at 400/s retention fell from 1,276 to 678 B/torrent; at 10/s the result was inconclusive. It is bounded only by `max_docs_before_merge` (10M by default), so in practice it grows with the corpus. The allocator options (`purge_delay=0`, `mi_collect`) were ruled out.
- **The fix is decided by an experiment, not a hypothesis:** prune-only against an uncontaminated default (M1-exp). Merge policy is revisited only if growth remains.

## The fix sequence (build-plan §0.2 Track R)
| Step | What | Exit |
|---|---|---|
| R1-fix (M1) | M1-exp, then X-mmap-cache-prune: call `get_cache_info()` on both directories after each successful commit, reopen and rebuild swap. Adds the gauge `lodestar_search_mmap_cache_entries{index}` (live mappings kept after the latest prune). Minimal, so it cherry-picks onto rc.2 | M1-exp Part 1 passes (the control reproduces; B ≤ 10% of the A mean per commit) and Part 2 shows B < 0.5 MiB/h; fresh Opus review, green CI, merged |
| M2 (conditional) | Commit at ≥ 30 s or 5,000 docs instead of every 2 s. Only if Part 1 passed and Part 2 still leaves B > 0.5 MiB/h; never triggered by an inconclusive control. Guardrail: discovery-to-searchable p95 ≤ 10 min; flagged to the owner as a UX change (§2.19) | Its own A/B, review, merge |
| R3′ (R-rc3-cut) | #402 and the memory fix (and M2 if shipped) onto `release/v0.5.1` after #377, which merged as 0acc5a37 | Release PR green, reviewed, merged; the owner tags `v0.5.1-rc.3` |
| R1′ | Soak r4 on rc.3: frozen image, 6 h suppression, 30 min warm-up, 24 h baseline | memory upper CI ≤ 2.0 and gate evidence yes |
| R2 | The gluetun leg (≥ 2 h) on rc.3 | as before |
| R-win | Short Windows MSI service smoke on rc.3 (≥ 1 h) | pass |
| R3b | The owner tags `v0.5.1` | — |

- **M1-exp:** Part 1 is the mechanism, about 10 one-document commits a second for 1 h, 3 runs per arm. The control (arm A) is never pruned or measured through `get_cache_info()`. Part 2 is production cadence (about 665 torrents/h), 2 runs per arm of ≥ 6 h. Part 2 is supporting evidence; **the frozen 24 h soak r4 stays the release gate.**
- **Merge policy stays out of the release path.** It reopens as its own slice only if B leaves residual growth, because lowering `max_docs_before_merge` can raise segment count and search fan-out.
- **On another FAIL:** the same loop (fix on `main`, cherry-pick, new rc, fresh frozen soak), never stitched.
- **CR-COL-4** (collector and analyzer, not in the release) adds `search_commits_total` and the mmap gauge, a slope per 1,000 commits, and "provisional (window incomplete)" labels. It merges before r4, and r4's record pins the collector and analyzer revision beside the rc.3 image digest.

## Crawler baseline (Linux, rc.2)
Taken from r3 at baseline hour 24 (end), compared with hour 12. It feeds Track C (CR-ADM / CR-AB-ADM, CR-1a, CR-1c) unchanged.
- **Stored torrents:** 8,738 at hour 12, 16,719 at hour 24, about 665 stored an hour (about 11 a minute).
- **Metadata fetch (TCP):** 358,915 attempts, 16,769 successes (about 4.7%).
- **DHT peer capacity rejections:** 106,071,693.
- **Peer command full:** 40,923,860.
- **Fetch lost, evicted:** 5,527,305.
- **uTP attempts:** 0, expected while the switch is off.

## Other findings placed
| Finding | Placement |
|---|---|
| Memory steps from 98 to 283 MiB during warm-up | The fetch pool filling to its 200k cap (about 690 B a key). Bounded and within the §1.6 budgets. **Flag for E-v06-workers:** a worker's ≤ 256 MB budget can't carry a 200k pool, so size the worker pool cap there. |
| Crawler yield: 11 stored a minute, 4.7% success, capacity rejections, command-full, unaccepted evictions | The Linux rc.2 baseline above. Command-full and unaccepted evictions are the CR-ADM hypothesis, so no new slice. |
| uTP at 0 attempts | Expected, `dht.utp` is off (CR-4). Nothing to do. |
| No search load during the soak | The official protocol stays unchanged so runs stay comparable. M1's bounded search smoke test covers searches during commits and merges. Query-time memory at scale stays in the 10M gate. |
| The misleading early fit | CR-COL-4's provisional labels, plus the runner rule above. |
| `collector_window_complete_review_required` | The acceptance review above, and again in r4's record when it passes. |
| `writer.rs` `expect` (import smoke finding 2) | Check whether SRC-14a fixed it. If not, X-writer-join-error (small, Opus) after Track R. |
| RSS after drain, glibc zstd arena | Stay in the import-pilot and gnu-build notes. Not on the release path. |
