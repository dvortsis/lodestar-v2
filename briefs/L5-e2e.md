# L5-e2e: welcome and settings key screens on the L3–L5 landing
**Model:** Opus. Runner brief, 2026-10-30. Web/E2E only.

## Base and hand-off
- Branch `fix/L5-e2e` from `origin/land/L5` (head `51aeddb0` or later). `land/L5` is the combined L3–L5 landing PR #184, which the runner owns. **Don't push `land/L5`.** Push `fix/L5-e2e` and message the runner. The runner merges it into `land/L5`.
- `main` was rewritten on 2026-10-30. Never use an older clone.

## Background
CI regenerated the welcome and settings baselines on `land/L5` (commit `51aeddb0`, "Update E2E snapshot baselines from CI run 41018370431"). `web/e2e/.update-snapshots` is still present. The runner reviewed the images and found two problems:
1. **Welcome isn't covered any more.**
   - `/welcome` now renders `WelcomePage.svelte`, which checks auth and redirects to sign-in when not signed in.
   - The smoke route `{ name: 'welcome', path: '/welcome', key: true }` in `web/e2e/support/routes.ts` mocks no auth, so the "welcome" baseline is a second sign-in screenshot.
   - Give the route the mocks it needs to render the real welcome page (signed in, setup done; look at how other routes mock `getAuthStatus`), plus a `heading` assertion.
2. **The settings key screen lost the top bar's right-hand controls.** At 1366 px light, the new `settings-bp1366-riverstone-light-comfortable-linux.png` shows the nav, but no status pill, palette button or More button. The old baseline had "Connecting", ⌘ and ⋯.
   - `TopBar.svelte` renders them unconditionally, and other key screens' baselines (search and others) still match with them present.
   - Find out why, for example a render error in `statusRef.Pill` for the state the settings route's default mocks produce, or something in `UnavailablePage`/routing.
   - Check the browser console in the test. If it's a product bug, fix it in the owning file with a unit test. If it's expected, explain exactly why in your report.

## Do
- Reproduce with the repo's Playwright (Chromium is at `/opt/pw-browsers`; never run `playwright install`; see `web/e2e/README.md`). Linux baselines are generated only in CI's pinned container, so don't commit locally generated PNGs.
- Keep `web/e2e/.update-snapshots` in place, so the runner's next push regenerates the baselines with your fix. Don't delete the regenerated PNGs by hand.
- Checks in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npm run e2e:typecheck`, and the Playwright `core` project for the touched specs.
- Fresh-reviewer self-review (steward §1).

## Own
`web/e2e/support/routes.ts` and the mocks it uses. If a product bug is proven: the owning shell or feature file plus its unit test. Also `docs/decisions/2026-10-30-L3-L5-landing.md` (append a short section).

## Report
Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the head SHA of `fix/L5-e2e`, the root cause of item 2 with evidence, and the checks you ran.
