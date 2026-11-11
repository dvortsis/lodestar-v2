# Runner self-check, #684 X-e2e-baseline-refresh at 1ace9e0f31f8cf9e512481c0f002b6ebf485c37c: MERGED as 59e09ec6964de16b405d07a100981aed06932fc5

Docs only (README +2 lines, decisions file +6); small, self-checked; no new review needed.
- The README line matches snapshot.ts:11 (freezeClock sets page.clock fixed time).
- No baseline regenerated: nightly E2E on main (run 43007058662 at 7e4d532) passed states.test.ts; no Insights code change since #588.
- ci-ok, web-ok and e2e-ok green; clean merge.
Nit: the decisions heading lacks "(#684)".
