# X-flake-e2e-expect-timeout (runner brief 2026-11-20)

## Why
On #992, `playwright (core)` failed 1 of 1,378 tests: `web/e2e/shell.routes.test.ts:439`, "401 hand-off > signed-out visit goes to /login". The page was still on `/alerts?x=1` when `toHaveURL` ran out of time at Playwright's default 5 s `expect` timeout. CLAUDE.md asks for condition waits with deadlines of at least 10 s, so the slowest CI runner still passes. A re-run only hands the flake to the next PR, so it gets fixed once, here.

## Scope
1. Set the default `expect` timeout in `web/playwright.config.ts` to 10_000 ms: `expect: { timeout: 10_000 }` at the top level, and in any project that overrides it.
2. Find every assertion in `web/e2e/**` that sets its own timeout under 10 s, including `toHaveURL`, `toBeVisible`, `waitForURL`, `waitForSelector` and `poll`. Raise each one to at least 10 s, or remove the override so the new default applies.
3. Find any fixed wait (`waitForTimeout` or similar) in `web/e2e/**`. A fixed wait is a blocking finding. Replace it with a condition wait, or list it in the PR if replacing it would be large.
4. Don't change the test's intent. If the 401 hand-off itself looks slow for a real reason, such as a redirect chain, say so in the PR. Don't paper over it.
5. Add a decisions file for the default timeout: `docs/decisions/2026-11-20-X-flake-e2e-expect-timeout.md`.

## Owns
- `web/playwright.config.ts`
- `web/e2e/**`
- the decisions file
- this slice's own entries in `slices.yaml` and `dashboard.yaml`

## Checks
In `web/`, run:
- `npm run check`;
- `npx vitest run`;
- `npm run build`;
- `npx playwright test`, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`. Never run `playwright install`.

Then run `tools/status-connector`'s `npm test` if you touched the yaml.

## Done
CI green. Send the runner "ready <pr> <full sha>".
