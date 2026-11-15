# X-floor-renew-metrics decisions (#905)

A follow-up to `2026-11-14-X-floor-renew.md`: counters for the lapsed worker-floor keys, as the runner asked in the review of #897 (2026-11-14). This file says only what the code now does.

## Metrics (docs/metrics.md)

- **`cluster_floor_lapsed_keys`** (gauge): the keys `FloorReports` remembers with their workers, at most `LAPSED_CAP`. It lives in memory, so it reads 0 after a restart until reports lapse again.
- **`cluster_floor_lapsed_evicted_total`** (counter): lapsed keys the map forgot at `LAPSED_CAP`, oldest first. A key moving to the newest place isn't counted.
- **`cluster_floor_reports_lapsed_total`** (counter): reports refused as `Reported::Lapsed`. They're also counted in `cluster_floor_reports_total`, as before.
- **`cluster_claims_denied_excluded_total`** (counter, `FleetSnapshot::denied_excluded`): claimed keys denied as stored only because the store check excludes them for the claiming worker. A key the store denies anyway (stored or held) isn't counted, so this counts only what the exclusion itself changed; a key whose store read failed is counted. (Refined by `2026-11-15-X-floor-renew-metrics-nits.md`.) It's summed over the port's openings like every cluster counter.
- No key, worker or free-form label: each is one series.

## Clarification of `2026-11-14-X-floor-renew.md`

- **Expiry is one immediate write transaction.** That file says nothing else in the store changed. `Store::expire_worker_floor` now runs its delete, reads its rows and commits in one `BEGIN IMMEDIATE` transaction, so a row that can't be decoded rolls the whole delete back and nothing lapses that run.
