# CR-0 soak acceptance (#214)

Owner, 2026-10-30. The ruling is on #189, comment of 13:05Z, answering the runner-commissioned acceptance review on the same issue. The fit is the owner's, posted at 13:20Z. The runner recorded both. Build-plan §0.3 and §0.4.

## The ruling
- **CR-0 is accepted.** It passes every gate of the acceptance review. Memory passed under the criterion below.
- **The memory criterion is loosened by the owner.** "Fitted slope ≈ 0 after hour 1" becomes **"no sustained growth", judged on baseline hours 12–24.**
  - This is an explicit owner loosening of the original wording, recorded as such.
  - It was decided before the full-resolution fit was known, with a pass bar set in advance: **the upper 95% CI bound of the private-bytes slope over baseline hours 12–24 is ≤ 2 MiB/h.**
  - Missing the bar would have failed the gate. It wouldn't have left the pass provisional.
- **The withdrawn −0.17 MB/h figure isn't used anywhere.**

## The fit
These figures come from the 1,428 full-resolution baseline rows of the frozen soak `744b84d6` (r21) on the soak host. Only the aggregates are published. The raw samples stay private.

| Window | n | OLS slope | 95% CI |
|---|---|---|---|
| Baseline hours 12–24 | 713 | −0.26 MiB/h | [−0.85, +0.32] MiB/h |
| Baseline hours 1–24 | 1,367 | +3.56 MiB/h | [+3.31, +3.81] MiB/h |

- **Peak private bytes:** 658,649,088 B (628.14 MiB), reached during the baseline.
- **Verdict:** the upper bound of +0.32 MiB/h is within the 2 MiB/h bar. **The memory pass is final, not provisional.**
- The hours 1–24 slope shows the warm-up growth that the ruling excludes. It flattens by hour 12.

## What follows
1. **The v0.5.1 delta mapping is cleared to proceed.** It's in `2026-10-30-v051-cut.md` on `release/v0.5.1`.
2. **The 24 h Linux Docker soak is the binding memory gate for v0.5.1.**
   - It reports the same thing: the OLS slope of process memory over hours 12–24 of the measured window, with its 95% CI, and the run's peak.
   - **The bar is the same:** the upper 95% bound is ≤ 2 MiB/h.
   - On Linux, the memory series is the collector's resident set or private memory, whichever CR-COL records as its memory series. The report names which one.
   - It's added to build-plan §0.4.
3. **R-gauges:** the rc gains storage and cache size gauges. These are DB, WAL, index, and the in-process caches, sampled every 60 s. They let a ramp in the Linux soak be explained.
   - The slice doesn't touch the crawler lifecycle, the sampler or the endpoint policy, so it doesn't force another soak (§0.4). It's class B/C in the delta map.
   - It reaches the release branch by cherry-pick, so the Linux soak and the Windows service-mode run use **`v0.5.1-rc.2`**.
   - `v0.5.1-rc.1` (`8b67f269`) doesn't carry the gauges.
