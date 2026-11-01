# S-e2e-2: snapshot every illustrated state, and give the fixture specs a home
**Model:** Sonnet (test scaffolding following `web/e2e/states.test.ts`). The runner reviews. Runner brief, 2026-11-01. It closes the S-e2e (#314) follow-ups for the §10.17/§10.19 brand gate (M5 exit gate).

## Why
`docs/decisions/2026-11-01-S-e2e.md`:
- Only one illustrated state is snapshotted (`state-not-found`). §10.17 asks for "a visual snapshot of every illustrated state in light/dark at each breakpoint".
- The `*.fixture.test.ts` specs (`detail`, `search-facets`, `search-keyboard`, `search-query`, `shortcuts`) match no Playwright project, so they never run, not even by hand through a project name.

## Read first
- `docs/architecture.md` §10.19 (the scene table: which screen shows which scene), §10.17.
- `docs/decisions/2026-11-01-S-e2e.md`, `docs/decisions/2026-11-01-X-illustrations.md`.
- `web/e2e/states.test.ts`, `web/e2e/support/**` (the smoke mock: how routes get their API answers), `web/playwright.config.ts`, `web/e2e/README.md`, `.github/workflows/e2e.yml`.

## Do
1. **Scene snapshots.** In `states.test.ts`, following its pattern (wait for the image to decode; axe; snapshot only at comfortable density), add one test per illustrated state the app can reach with the smoke mock. Candidates from §10.19:
   - `searching_crawling` (Search's empty-index state and/or Insights "Starting up");
   - `offline_idle` (crawler paused);
   - `connection_error` (API unreachable full-page state);
   - `empty_state_explore` (empty Discover, Alerts or Collections).

   Drive each state through the mock's API answers, not app code changes. If a state can't be reached without an app change, list it in the PR as a gap and skip it. **Don't edit `web/src/**`.**
2. **Baselines.** New images only: add the `web/e2e/.update-snapshots` marker in the push that adds the tests. After CI commits them, check `git diff origin/main <branch> -- '*.png'` shows **only new files** (no existing image modified), remove the marker in your next push, and tell the runner.
3. **The fixture specs.** Add a `fixture` project to `playwright.config.ts` that matches `*.fixture.test.ts`, opt-in like `all-themes`: it must not run in the default or `core` runs, because it needs a real server. Document in `web/e2e/README.md` how to run it locally (the server it expects, the command). **Don't add a CI job** in this slice. Name that as a follow-up in your decisions file, with the CI cost estimate (building the Rust binary in the e2e workflow).
4. **Don't touch** `smoke.test.ts` or the torrent/detail specs: another PR (#316) is regenerating the torrent baselines.
5. Condition waits only (≥ 10 s), no fixed sleeps. Each new test runs 3× clean locally. Say which project each new test joins.
6. **Decisions file:** `docs/decisions/2026-11-01-S-e2e-2.md`: the scenes covered, any unreachable ones, the fixture project and the CI follow-up.

## Own
`web/e2e/states.test.ts`, `web/e2e/support/**` (mock answers for these states only), the new baselines, `web/playwright.config.ts` (the `fixture` project only), `web/e2e/README.md`, your decisions file.

## Size
≲ 400 changed lines (excluding PNGs).

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx playwright test e2e/states.test.ts` for the `bp*` projects (3×)
- `npx playwright test --project=core`

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Local snapshot pixels can differ from CI's pinned Chromium: let CI generate the baselines via the marker.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the scenes covered. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
