# X-menu-keyboard-focus: a keyboard-opened row menu focuses its first item
**Model:** Sonnet (a small UI fix that follows the existing pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-07T19:20Z. Follows X-flakes-1107 (#590) in the same session.

## Why
A row's "More actions" menu opened with Shift+F10 focuses the menu container, not its first item, unless some other Bits UI component is already mounted. That breaks the WAI-ARIA menu-button pattern, and a screen reader announces the menu with no item. The arrow keys and Home/End still work.

**Cause (from #590's review):**
- Bits UI 2.20.0 focuses the first item only when `isUsingKeyboard` is true (`node_modules/bits-ui/dist/bits/menu/menu.svelte.js` ~756 and ~900).
- That flag is set by a capture `keydown` listener on `document`, which exists only while a Bits component is mounted (`utilities/is-using-keyboard/is-using-keyboard.svelte.js` ~21–27).
- The lazy Menu in `web/src/lib/features/search/results/RowActions.svelte` ~30–35 mounts after the Shift+F10 keydown, so the flag is still false.

## Do (branch `slice/X-menu-keyboard-focus` from `origin/main`, after #590 merges)
1. **Detect a keyboard open.** In `RowActions.svelte`'s `more()` (reached from `ResultsPanel.svelte` ~392–399 through `more.click()`), a synthetic `.click()` has `event.detail === 0`. Also treat Enter or Space on the button as a keyboard open. Pass `autofocusFirst` to the Menu.
2. **Focus the first item.** In `web/src/lib/ui/overlays/Menu.svelte` (~37–42, `DropdownMenu.Content`), when `autofocusFirst` is set, use Content's `onOpenAutoFocus` to call `preventDefault()` and focus the first enabled `[role=menuitem]`. A pointer open behaves as it does today.
3. **Tests:**
   - Tighten `web/e2e/filters-block.keyboard.test.ts`: after Shift+F10, assert the **first** item is focused, then End moves to "Block this…". Use the menu name `/More actions/` (it isn't the shell's "More" menu).
   - Add a vitest component test for `autofocusFirst` if the Menu has a test file.
   - In `web/e2e/shell.routes.test.ts`, wrap the `page.evaluate` inside `expect.poll` with `.catch(() => false)`, so a navigation mid-poll retries instead of failing (#590 nit).
4. **Check other lazy menus.** If any other menu is opened from the keyboard through a synthetic click, apply the same flag, or list them in the decisions file.
5. **Decisions file:** `docs/decisions/2026-11-07-X-menu-keyboard-focus.md` (short).

## Own
- `web/src/lib/ui/overlays/Menu.svelte`, `web/src/lib/features/search/results/RowActions.svelte`, and the menu's component test;
- the two e2e specs above;
- the decisions file.

Ask the runner before touching anything else. Tokens only; no hex values.

## Done when
- In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.
- The two specs and the keyboard e2e project pass. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`.
- Repeat the filters-block spec with `--repeat-each=20` and report the count.
- No fixed sleeps.
- Open the PR against `main` titled "X-menu-keyboard-focus: a keyboard-opened row menu focuses its first item". Follow the steward skill, push once per round, never merge. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head.
