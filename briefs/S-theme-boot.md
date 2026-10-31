# S-theme-boot: apply the account's saved appearance at start-up
**Model:** Sonnet: a small, well-specified shell hook that follows an existing pattern. Runner brief, 2026-10-31. This is the follow-up to #243 (S-appearance), from its review scope call. **Small PR:** aim for ≲ 400 changed lines.

## Why
Today, only the pre-paint script in `web/src/app.html` applies a theme at start, and it reads only `localStorage["lodestar.theme"]`. So on a new browser or device the user sees Riverstone, with the default density, motion and illustrations, until they open Settings → Appearance. §10.18 says the theme "follows the account across devices". `docs/decisions/2026-10-30-S-appearance.md` records this gap (about lines 36–38) and the shape of the fix.

## Read first
- `docs/decisions/2026-10-30-S-appearance.md`.
- `web/src/lib/features/appearance/{prefs.ts,themes.ts}`: `load`, `applyLook`, `applyTheme`, `readCustom`.
- `web/src/lib/features/shortcuts/boot.ts` and how `web/src/lib/shell/Chrome.svelte` calls `startShortcuts()` (about line 83). **Copy that pattern.**
- `docs/architecture.md` §10.18: the pre-paint contract and the `lodestar.theme` cache shape.
- `runner/standards.md`, and CLAUDE.md's UI rules.

## Do
1. **`startAppearance()`** in `web/src/lib/features/appearance/boot.ts`, called by the shell next to `startShortcuts()`, on the same first-load path, and returning a disposer.
   - Fetch `preferences` with `prefs.load`, then apply with `applyLook` and `applyTheme`. `applyTheme` already updates the `lodestar.theme` cache, so the next load pre-paints correctly.
   - Load it on demand (a dynamic import, as the shortcuts boot is loaded), so the first view stays small and no size-limit entry grows. If an entry does grow, report the numbers; don't raise a limit.
2. **No flash, no fight:**
   - If the saved values already match what pre-paint applied, change nothing in the DOM.
   - A failed or aborted fetch leaves the pre-paint state alone and shows no error, since it's not user-initiated.
   - If the user changes Appearance before the boot fetch resolves, the boot result must not overwrite their choice. Reuse `applyTheme`'s generation guard, or an equivalent check.
3. **A custom theme the server names but this browser lacks** (`custom-…`, kept per browser): fall back to the family's Riverstone mode exactly as `applyTheme` already does for a missing theme, with no error toast.
4. **Follow changes made elsewhere** if the shortcuts boot does, through the same mechanism. If it doesn't, don't add one.
5. **The #243 delta nit:** in `themes.ts`, move `let generation` above `applyTheme`'s JSDoc, so the doc attaches to the function.
6. **Tests:**
   - vitest for the boot: it applies the saved values; it changes nothing when they match; a fetch failure is silent; a later user choice wins the race; a missing custom theme falls back.
   - One Playwright spec: start with an empty `localStorage` and a mocked `preferences` holding a non-default theme and density. After load, `data-theme` and the density match, and `lodestar.theme` is cached. Use condition waits, never fixed sleeps, and run axe as the existing specs do.
   - **No baseline changes are expected.** If any screenshot changes, stop and tell the runner instead of regenerating.
7. **Decisions file:** a short `docs/decisions/<date>-S-theme-boot.md` saying the deferral recorded in S-appearance is closed.

## Own
`web/src/lib/features/appearance/boot.ts` (new), its tests, the one call site in `web/src/lib/shell/Chrome.svelte`, the `themes.ts` nit, one e2e spec, and the decisions file. Don't touch `app.html`'s pre-paint script.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run`
- `npm run build`
- `npx size-limit`, with no limit raised
- `npx playwright test` on the new spec (Chromium is at /opt/pw-browsers; never run `playwright install`)

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
