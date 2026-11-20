# X-results-drawer-adaptive (runner brief 2026-11-20)

This is Track U step 3: UI review findings I1 and I3, built in one slice under owner decision §2.66, items 1 to 3.

## Read first
- The slice's `runner/slices.yaml` entry. Its notes are the scope.
- docs/owner-decisions.md §2.66.
- docs/decisions/2026-11-13-runner-ui-review-decisions.md, for the drawer rule.
- docs/decisions/2026-11-14-runner-ui-review-placement.md (I3, L5).
- docs/review/2026-11-13-ui-ux/ 01-review.md and 02 §3a and §3b.
- §10.15, §10.2 and §10.4 of docs/architecture.md, as #996 just left them.
- CLAUDE.md UI rules: tokens only, no hex values, and the §10.2 spacing ramp.

## Scope
1. **I1.** While the drawer is open, the results track may shrink below `--results-min-width`, down to a 480 px floor. The drawer opens at `--drawer-min-width` until the window fits the stored width. From 1366 px the drawer sits beside the results instead of over them.
2. **I3.** Result rows adapt to their container, not the viewport, using container queries. Rows about 542 px wide must not clip.
3. Discover Newest shows the discovered time first (I3).
4. Facts wrap and never clip under WCAG 1.4.12 text spacing (L5).
5. Refresh the 768 and 1024 px baselines that still show clipped rows, and add a 1440 px cell. Generate baselines only with the repo's own Playwright snapshot workflow, and use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`. If this container's Chromium renders differently from CI, as the `tokens.test.ts` screenshots do, say so in the PR. Let CI's run be the judge rather than committing baselines from this container. Follow `web/e2e/README.md` on how baselines are made.
6. "Group versions" (GS-5) and "Identified as" (ENR-13: text, IDs and links only) stay as decided.
7. Keep the Search route within its size budget (`npx size-limit`). Don't raise any limit; if it doesn't fit, tell the runner.

## Owns
- web/src/lib/features/search/results/**
- web/src/lib/features/search/table/**
- web/src/lib/features/search/cards/**
- web/src/lib/features/detail/DetailHost.svelte, DetailHostBody.svelte and host.css
- web/src/lib/ui/overlays/Drawer.svelte
- web/e2e/search-results*.test.ts and web/e2e/detail*.test.ts, with their snapshots
- en and zh messages under those folders
- docs/decisions/2026-11-20-X-results-drawer-adaptive.md
- this slice's own yaml entries

Never touch the tokens or the theme files. If you need a new token, ask the runner. X-ui-guards owns web/src/lib/** and never runs alongside this slice.

## Checks
In web/:
- `npm run check`;
- `npx vitest run`;
- `npm run build`;
- `npx size-limit`;
- `npx playwright test` for your specs.

No fixed sleeps. Condition waits need deadlines of at least 10 s.

## Done
CI green, including e2e-ok. Send the runner "ready <pr> <full sha>". A fresh Opus review follows.
