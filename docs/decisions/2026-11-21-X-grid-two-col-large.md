# X-grid-two-col-large decisions (#TBD)

Slice session, 2026-11-21Z, from #1013. X-ui-guards banned breakpoints that aren't on the §10.2 ramp (`sm:`, `md:`, `lg:`, …) and removed the dead `sm:grid-cols-2` classes. Those classes never applied (the theme resets Tailwind's breakpoints, so `sm:` doesn't exist), but they marked where three screens meant to have two columns.

## The change
- **`large:grid-cols-2`** (640 px, `--breakpoint-large`; docs/design-tokens.md §5, the ramp's name, used across `web/src`) on three grids, one column below it:
  - the install dialog's offer list (`companions/InstallDialog.svelte`), a `<dl>` of label | value pairs;
  - the Unraid template's fields (`companions/Templates.svelte`), the same;
  - the Logging screen's per-component levels (`logging/ComponentLevels.svelte`), a label + select per component.
- No hex, no arbitrary value; the `<dt>`/`<dd>` pairs fill the two columns left to right, so each label sits beside its value.

## Checked (throwaway Playwright screenshots, 390, 1024 and 1440 px, deviceScaleFactor 1, looked at)
- No horizontal scroll at any width of the three screens (`scrollWidth <= clientWidth`, 9 of 9).
- At 390 px each is one stacked column, as before. From 640 px the dialog and the Unraid fields read label | value in aligned rows; the per-component selects sit in two columns with their labels above.
- Focus order: the DOM order is unchanged, and a row-major grid reads left to right, top to bottom, so the tab order is what it was (System, DHT, Fetch, Pipeline, …).
- The screenshot specs were scratch files and aren't committed. The companions, settings-logging and logs e2e specs pass (13 of 13, core and keyboard-only).

## Snapshots and size
- **No snapshot changes:** no baseline shows these screens (the Settings smoke shot is `/settings/general`).
- **No size change:** the edits are three class names in lazy chunks. `npx size-limit` shows only the two known edge-sitters, "Filters packs list (on demand)" (1,562 B against 1,560 B) and "Search cards (lazy)" (8,012 B against 8,010 B), the build wobble the runner measured on `main`; neither chunk is touched here, and no limit is raised.
