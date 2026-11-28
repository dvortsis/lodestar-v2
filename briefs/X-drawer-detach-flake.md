# X-drawer-detach-flake

The runner's brief, 2026-11-28Z. It's given to the session that built X-upload-screen-flake (#1174).

## Why
1. **The e2e flake.** On #1167's run, `web/e2e/search-results-adaptive.routes.test.ts:191` ("the drawer beside the results > the 1440 px cell with the drawer open") failed with `locator.scrollIntoViewIfNeeded: Element is not attached to the DOM`. It's a race with the lazily loaded Technical details section re-rendering the drawer. #1167 doesn't touch the search screen.
2. **The unit-test teardown risk.** `web/src/lib/features/search/results/ResultsPanel.test.ts` has the same teardown risk #1174 fixed in `ResultsPanel.group.test.ts`: a row's focus or hover starts `import('$lib/features/detail/open')`, and there's no `afterAll` waiting for it.

## Scope (owns)
- `web/e2e/search-results-adaptive.routes.test.ts` and its helpers.
- `web/src/lib/features/search/results/ResultsPanel.test.ts`.
- Product code only if the cause is a real UI bug, where the drawer replaces a node a user could be acting on. Tell the runner before you change it.

## How
1. **e2e:** wait on the settled drawer (the lazy section rendered), with Playwright locators and condition waits only. No `waitForTimeout`. Every assertion stays unchanged.
2. **Unit test:** add an `afterAll` that awaits the lazy imports the panel starts (`$lib/features/detail/open`, `$lib/features/detail/commands`, and the rest `.group` and `.query` already await).
3. **Proof:**
   - before/after under load: the spec with `--repeat-each` 30 or more, `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and `--trace off`;
   - the unit file run 30 or more times under 16 hogs.
4. No sleeps and no skips.
5. Don't edit `runner/slices.yaml`; the runner adds the entry.

## Done
- A PR titled `X-drawer-detach-flake: <title>` from `slice/X-drawer-detach-flake`.
- The full fast web checks are green.
- Send `ready <full sha>` to the runner.
