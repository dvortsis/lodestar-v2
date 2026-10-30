# S-appearance decisions

Settings → Preferences → Appearance (§10.10.8, §10.18). Items marked **(§2.19)** refine an owner decision; the runner flags them to the owner.

## What the screen does

- **A hand-built screen** (`features/appearance/screen.ts`, `load`), registered through the screen registry from #217. Its first view is the screen's body: mode, density, motion, illustrations and Show adult content.
- **Preferences apply at once, with Undo (§10.10.1).** There is no Save. Every change is applied, then saved with the section's `if_revision`. A failed save puts the old value back and says so. A stale revision reloads. Undo saves the previous value.
- **Applying** sets `data-theme` and the theme's custom properties on `<html>` and writes `lodestar.theme` for the pre-paint script (§10.18). Density, motion and illustrations are applied as `data-density`, `data-motion` and `data-illustrations` on `<html>`, cached in `lodestar.appearance`. A forced motion setting sets the duration tokens inline, so it wins over `prefers-reduced-motion`.
- **Fields set by the environment or read-only** (`env`, `read_only`) are locked and the screen says so.
- **Show adult content** is the server-wide `content.show_adult` (B17). It uses M-filters-dialogs' flow (`createAdultConfirm`, `setShowAdult` and `AdultConfirmDialog` from `features/filters/flows`), which sends `confirm_adult: true` with the turn-on and nothing to turn off. The dialog loads with the first turn-on (Bits UI overlays), gated by its own size entry. A value set by `LODESTAR_SHOW_ADULT` or read-only is shown locked. If the save fails or is cancelled the switch shows what the server has.
- **Saves go one at a time.** Each change applies at once, then queues behind the save in flight and sends the revision the last one returned, so arrowing through options never trips `stale_revision`. A failed save puts back only the fields it set (and only if nothing newer replaced them). Locked fields are disabled and named in a banner.
- **"System" follows the device live:** the screen listens to `prefers-color-scheme` and re-applies while the mode is System.
- **Theme picker:** "Browse themes" loads the picker and `themes.json` (§10.18: it loads only when Appearance opens). Cards are a radio group; each is drawn with that theme's own tokens, its name is text and it shows a Contrast AA/AAA badge. A card is a family; its preview follows the mode that is showing. Riverstone's card uses the critical CSS (`data-theme`), so it needs no file.

## Refinements

- **Custom themes live in this browser (§2.19).**
  - The `preferences` API stores only a theme id, so a custom palette is kept in `localStorage["lodestar.customThemes"]` (every read and write in try/catch). The server keeps the id (`custom-<name>`), which follows the account.
  - On a device that lacks that custom theme the screen shows Riverstone and a note that custom themes are kept per browser and move with export and import.
  - It fits the pre-paint contract: the chosen theme's compiled variables are cached in `lodestar.theme` as `{mode, light: {id, vars}, dark: {id, vars}}`.
  - Approved by the runner (2026-10-30).
- **The picker has no "Light-first / Dark-first" filters (§2.19; the runner flags this to the owner).** Every family has both modes, so the filters would sort on nothing. All, Accessibility, Originals, Community and Custom are kept.
- **Stored custom themes are untrusted input.** `readCustom` keeps only entries with a `custom-…-light|dark` id, hex values and the token names they were saved against; a theme saved against other token names is treated as missing (Riverstone plus the note) rather than painting colours onto the wrong properties.
- **The contrast table's row labels are the compiler's English labels** (`themes-core.ts`); translating them waits for a label-key pass over the compiler.
- **The custom theme editor uses the build's compiler.** `web/scripts/themes.ts` was split into `themes-core.ts` (schema, `derive`, `evaluate`, the gates; no `node:` imports, nothing runs at load) and `themes.ts` (the driver, which re-exports the core). It is a mechanical move: the generated `themes.json`, `critical.css`, `prepaint.js` and `app.html` are byte-identical before and after, and `scripts/themes.test.ts` passes unchanged. Approved by the runner (2026-10-30; the file is outside this slice's `owns`). The editor imports the core only in its on-demand chunk.
- **Duplicating a theme rebuilds its palette from the compiled tokens.** `themes.json` holds no base palette, so "Start from" reads the background, surface, text, brand primary and accent, and the six data colours in slot order. Light-mode data colours are the derived variants, which are valid inputs. A custom theme keeps its own base palette, so editing and export are exact.
- **Saving a theme that fails a gate asks for "Use anyway"** and marks it with a "Fails contrast" badge on its picker card (§10.18).

## Budgets (§10.1)

No limit was raised. The measured sizes are in the PR. `.size-limit.mjs` marks the picker and the editor as on demand and gates each with a new entry, so Appearance's first view stays well under the 45 KB route cap, and the Alert route's headroom is untouched.

## Not in this slice

- **The "Change theme" command-palette action** (§10.18) needs the shell's command registry; it follows as a small shell change.
- **Applying the saved preferences when the app starts on a device that never opened Appearance** needs a start-up hook in the shell (like `startShortcuts()`). The pre-paint script already applies a cached theme and mode. The shell can call `applyTheme` and `applyLook` from `features/appearance/prefs.ts` and `themes.ts` after fetching `preferences`.
- **Result rows and illustrations read `data-density` and `data-illustrations`** from `<html>` once their slices opt in; today the rows still take density from their own prop.
