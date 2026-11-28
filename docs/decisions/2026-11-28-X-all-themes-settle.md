# X-all-themes-settle decisions (#1182)

- **One `settle(page, route)` for both snapshot specs** (`e2e/support/settle.ts`). It waits for the route's heading, then the route's own `ready`, then for no loading placeholder (`[aria-busy="true"]` or a skeleton), each with a deadline of 10 s. `smoke.test.ts` and the nightly `all-themes.test.ts` both call it before axe and the snapshot. Why: #1180 added the wait to smoke only, while all-themes takes the same key-screen shots, torrent included, so it could still photograph a page mid-load. One helper keeps the two from drifting apart again.
- **No baseline changes.** The baselines were generated from settled pages. The wait only makes the capture later.
