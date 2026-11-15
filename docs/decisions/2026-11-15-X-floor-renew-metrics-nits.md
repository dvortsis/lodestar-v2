# X-floor-renew-metrics-nits decisions

A follow-up to `2026-11-15-X-floor-renew-metrics.md` (#905), from its review. This file says only what the code now does.

## Metrics (docs/metrics.md)

- **`cluster_claims_denied_excluded_total` counts only keys answered `Stored`.** The claim path now passes the claim's answer a per-key mark: excluded for the claiming worker, where the store alone found the key free or couldn't read it. The answer counts a marked key only on its stored branch. A draining or paused worker's claim, answered `Draining` before any store answer, isn't counted. This supersedes the counting point of the `cluster_claims_denied_excluded_total` bullet in `2026-11-15-X-floor-renew-metrics.md`: before, the key was counted when the store check finished, whatever the answer.
- **A key whose store read failed is counted**, as the exclusion alone decides its answer.
