# X-flake-boot-shell: fix the "route.fetch: Target page closed" flake in boot-shell e2e
**Model:** Sonnet (test fix following an existing pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-05T05:05Z.

## Why
`web/e2e/boot-shell.routes.test.ts` ("the error screen when its chunk cannot load" → "a failed navigation shows the error, the code and both actions") fails at random: `route.fetch: Target page closed` thrown from the `abortErrorView` route handler, which calls `route.fetch()` and then `route.fulfill`. When the test ends while a chunk request is still in flight, the page closes under the handler and Playwright reports the handler's rejection as a test error. It was seen on #454 (RP-DEF) on 2026-11-05 and has been retried since. Per CLAUDE.md, a flake gets fixed once.

## Do (branch `slice/X-flake-boot-shell` from `origin/main`)
1. In that describe block, make the route handlers safe once the page is gone, so the test's assertions are unchanged:
   - add `test.afterEach(async ({ page }) => page.unrouteAll({ behavior: 'ignoreErrors' }))`. This is Playwright's documented fix; check that the installed `@playwright/test` version supports it;
   - only if that's not enough, also guard `route.fetch()` and `route.fulfill()` in `abortErrorView` against the page having closed. Catch only that case, and rethrow any other error.
2. Grep `web/e2e/**` for other `page.route` handlers that call `route.fetch()` and could hit the same race. Apply the same fix where it fits, and list each one in the PR.
3. Prove it:
   - run the spec 30 times with `npx playwright test e2e/boot-shell.routes.test.ts --repeat-each=30`, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`, per `web/e2e/README.md`, and report the pass count;
   - no fixed sleeps, and never skip, retry or quarantine a test.

## Own
`web/e2e/boot-shell.routes.test.ts`, and the other e2e specs you list from step 2.

## Done when
In `web/`, `npm run check` and the e2e typecheck pass, and the repeated run is green. Open the PR against `main` with the title "X-flake-boot-shell: unroute boot-shell e2e handlers at test end". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
