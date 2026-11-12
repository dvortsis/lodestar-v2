VERDICT: MERGE

# Review: PR #735, OUT-2 (Settings → Outputs), round 3, head f2c9ad5a4bad071cd92b34bc0a2c7ff6d5bb309b

Fresh Opus reviewer, 2026-11-12 (UTC). Read only. Delta `9ddcab0d..f2c9ad5a`: ee2eeed2 (fixes), 50 regenerated baselines, the marker added then removed.

## Checks (own worktree)
- `npm run check`: exit 0 (only the existing DiscoverPage and app.d.ts warnings).
- `npx vitest run src/lib/features/settings`: 56 files, 714 tests passed.
- `npm run build`, `npx size-limit`: pass. Settings section 44.98/45 kB, outputs parts 26.32/27 kB, icon 678/750 B, OutputsScreen 34.16/45 kB.
- Playwright `e2e/settings-outputs.routes.test.ts`: 2 passed.

## Round-2 findings
- **B1 (press before the mount read): fixed.** `started()` captures `after = seen`, which is null until a read answers, so that press uses the clock match. Reintroducing the old behaviour (`after = after ?? seen` in `poll`) fails "a press before the first /jobs read answers still finds its job".
- **B2 (guards without tests): fixed as stated.** Mutation results:

| Guard removed | Caught |
|---|---|
| Both reply tokens | yes |
| Success-reply token | yes |
| `seen` raised by a found job | yes |
| Restart fallback | yes |
| Press before the read (old bug) | yes |
| Mount baseline | yes (2 tests) |
| Stale line, `saving` | yes |
| Id match → time match | yes (4 tests) |
| Truncated line inside the region | yes (2 tests) |
| Error-reply token only | **no** (Important 1) |
| `clearTimeout` in `started`, `live` in replies | no. These are equivalent mutants, because `poll`'s entry check already returns. |

- **B3 (truncation announcement): fixed.** The line is inside the always-mounted `role="status"` region, with an `sr-only` "Warning:" prefix (en and zh) and the icon `aria-hidden`. Nothing is duplicated.
- **I1: fixed** for a job the press found (`Bitmagnet.svelte:132`).
- **I2: fixed** (`:129`) and recorded in the decisions file.
- **Nits:**
  - Stale and lost now end before the Jobs link.
  - The `<pre>` has `role="region"`.

## Races tried
- **Mount read answering after a press:** that press stays on the clock match, and the read's ids only raise `seen` (`Math.max`). Safe.
- **Press during an in-flight poll:** the reply is dropped by its token, and `clearTimeout` plus the entry check end the chain. Safe.
- **Unmount mid-poll:** `live` is checked on entry and in both handlers, and the timer is cleared. Safe.
- **An older export in `/jobs`:** excluded by id. Under the fallback it is excluded by the 5 s slack. Safe.
- **Id 0 or an empty list:** `seen = 0` and `0 < 0` is false, so ids at 1 or above match. Safe.
- **Residual (Nit 1):** a press before the previous press's job resolved.

## Important
1. **`Bitmagnet.svelte:138`: the error-path token is untested.**
   - Without it, an older press's failed reply raises the shared `misses` and can set `lost`.
   - The success handler never clears `lost`, so the newer press would show "couldn't find" while it polls.
   - **Fix:** add a test that holds press 1's poll, presses again, releases the held reply as a 500, and asserts the second press still gets 3 retries and shows its progress.

## Nits
1. **`logic.ts:76-83`: a later press can still adopt an earlier press's job.**
   - This happens when the earlier job hadn't resolved (`export: null`) by the time of the later press. A double-click is enough.
   - If that earlier job finishes before the new one resolves, its "Done" shows for the new press.
   - **Fix:** take only the newest export above `afterId`, and wait while its `export` is null. Or reword the decisions bullet "an earlier press's job is never a later press's".
2. **`Bitmagnet.svelte:129`: the fallback switch sticks for the whole press and also fires on an empty list.**
   - An empty list happens when old jobs were swept after 10 minutes and the new job isn't listed yet. That press then depends on the clocks agreeing.
   - **Fix:** `const top = lastExportId(r.jobs); if (after !== null && top > 0 && top < after) after = null;`

## Baselines and rules
- **Changed PNGs:** 46 `theme-snapshots/settings--*` and 4 `smoke.test.ts-snapshots/settings-bp{1024,1366}-*`, nothing else.
- **Marker:** no `.update-snapshots` file remains in the tree.
- **PIL spot-check** (bp1366 riverstone light, nord dark, rosewood light):
  - Each image is 48 px taller.
  - Rows match above the Outputs row, and rows below it match the old ones shifted 48 px, apart from 1–3 px anti-aliasing.
  - Viewed: Outputs sits between Integrations and Notifications.
- **Messages:** en and zh have the same keys.
- **Styling:** no hex or arbitrary values. `mt-0.5` and `size-5` are on the ramp, and `text-warning` is a token.
- **Tests:** no fixed sleeps; every wait uses `waitFor` with a 10 s timeout.
