# X-web-hygiene-1107: FieldMenu keyboard focus, #594 review nits, the search-facets uncheck flake

**Runner brief, 2026-11-07. Model: Opus.** This is the same session as X-menu-keyboard-focus (#594, merged 46e5c803). Follow the steward skill. Use branch `slice/X-web-hygiene-1107` from origin/main, and title the PR `X-web-hygiene-1107: <title>`.

## Items
1. **FieldMenu's lazy-open keyboard focus** (the gap recorded in `docs/decisions/2026-11-07-X-menu-keyboard-focus.md`). Opening it from the keyboard focuses the first item; opening it with the pointer doesn't. Use the same pattern as #594: `autofocusFirst` through `Menu.svelte`, with `detail === 0` detection and a flag reset on close. Add a component test, plus an e2e test if a keyboard spec already reaches FieldMenu.
2. **#594 review nit 1:** in `web/e2e/search-results.keyboard.test.ts`, the pointer-after-keyboard test ends with `expect(first).not.toBeFocused()`, a negative assertion that can pass too early. Add a positive check before it, for example `await expect(page.getByRole('menu', { name: /More actions/ })).toBeFocused()`.
3. **#594 review nit 2:** in `web/src/lib/ui/overlays/overlays.test.ts`, add the negative case: with `autofocusFirst` false, the first item is not focused.
4. **The search-facets uncheck flake:** `web/e2e/search-facets.fixture.test.ts:52`. `uncheck()` on `facet-type-movie` reported "Clicking the checkbox did not change its state" once (E2E run 42404920557, attempt 1, on release/v0.5.2); the re-run passed. The likely cause is the facet rail re-rendering from the new `/facets` right after the query change.
   - Wait for the rail to settle before unchecking: for example, wait on the `/facets` response for the new query, or on a stable `data-` attribute the rail sets once loaded.
   - Then use `uncheck()` and assert the new state.
   - No fixed sleeps. Run the spec with `--repeat-each=20` and report the result.

## Owns
- `web/src/lib/features/**/FieldMenu*`, wherever it lives, plus its test;
- `web/src/lib/ui/overlays/overlays.test.ts`;
- `web/e2e/search-results.keyboard.test.ts`;
- `web/e2e/search-facets.fixture.test.ts`;
- `docs/decisions/2026-11-07-X-web-hygiene-1107.md`.

If item 1 needs `Menu.svelte` again, a minimal edit is approved.

## Gates
- In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, plus the keyboard-only, core and fixture Playwright projects for the specs you touch (with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`).
- Never run `playwright install`.
- About 200 lines.

Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green. The runner merges.
