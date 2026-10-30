# CR-0 soak acceptance (#214)

Owner, 2026-10-30, with the runner's refinements marked as such. The owner's ruling is on #189 at 13:05Z, answering the runner-commissioned acceptance review on the same issue. The fit is the owner's, posted at 13:20Z. Build-plan §0.3 and §0.4.

## The owner's ruling
- **CR-0 is accepted.** It passes every gate of the acceptance review. Memory passed under the criterion below.
- **The memory criterion is loosened by the owner.** "Fitted slope ≈ 0 after hour 1" becomes **"no sustained growth", judged on baseline hours 12–24.**
  - This is an explicit owner loosening of the original wording, recorded as such.
  - It was decided before the full-resolution fit was known, with a pass bar set in advance: **the upper 95% CI bound of the private-bytes slope over baseline hours 12–24 is ≤ 2 MiB/h.**
  - Missing the bar would have failed the gate. It wouldn't have left the pass provisional.
- **The memory report doesn't use the earlier, withdrawn slope figure.**
- **The 24 h Linux Docker soak is the binding memory gate for v0.5.1.** It uses the same report (the slope over hours 12–24 with its 95% CI, plus the run's peak) and the same bar. This is added to build-plan §0.4.
- **Storage and cache gauges go into the rc.** DB, WAL and index sizes, plus cache sizes, are sampled periodically, so a persistent ramp can be explained. The change must not touch the crawler lifecycle, the sampler or the endpoint policy, because that would force another soak.
- **The v0.5.1 delta mapping proceeds** once the fit settles the gate. It has: see below.

## The fit
These figures come from the 1,428 full-resolution baseline rows of the frozen soak `744b84d6` (r21) on the soak host. Only the aggregates are published. The raw samples stay private.

| Window | n | OLS slope | 95% CI |
|---|---|---|---|
| Baseline hours 12–24 | 713 | −0.26 MiB/h | [−0.85, +0.32] MiB/h |
| Baseline hours 1–24 | 1,367 | +3.56 MiB/h | [+3.31, +3.81] MiB/h |

- **Peak private bytes:** 658,649,088 B (628.14 MiB), reached during the baseline.
- **Verdict:** the upper bound of +0.32 MiB/h is within the 2 MiB/h bar. **The memory pass is final, not provisional.**
- The 12–24 window shows no sustained growth. The 1–24 window includes the earlier growth that the ruling excludes.

## Runner refinements (§2.19, flagged to the owner)
- **Linux gated series: anonymous private memory, the collector's `private_bytes` field.**
  - Under Docker this is cgroup v2 `memory.stat` `anon`, or `anon` from the Docker stats API when the cgroup tree isn't mounted. On a native run it's `RssAnon` from `/proc/<pid>/status`. Total resident memory (`working_set_bytes`) is recorded beside it for context, but it isn't gated.
  - **Reason:** the bar was calibrated on Windows private bytes, which is committed anonymous memory.
  - Linux resident set and cgroup usage also count file-backed pages. The search index is memory-mapped, so those figures grow as the index grows and the page cache warms, with no heap growth. They could fail the bar spuriously, or a shrinking cache could hide real growth.
  - Swap isn't counted. If the soak host swaps the process, the report says so, and the runner takes it to the owner.
- **Gauge cadence:** the gauges are sampled every 60 s, by a background task. `/metrics` reads cached values.
- **Delta class:** the gauges are class B/C in the v0.5.1 delta map. They're metrics only, with no change to shipped crawler behaviour.
- **Which candidate the gates use: `v0.5.1-rc.2`.**
  - The gauges reach `release/v0.5.1` by cherry-pick of their main PR.
  - The Linux Docker soak and the Windows service-mode run then use rc.2.
  - `v0.5.1-rc.1` (`8b67f269`) doesn't carry the gauges, so the runner recommends gating on rc.2. The owner decides whether rc.1 is approved for anything.
- **Delta mapping:** in `2026-10-30-v051-cut.md` on `release/v0.5.1`. The runner adds the gauges to it when it carries them over.
