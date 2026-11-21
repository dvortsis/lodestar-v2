# X-grid-two-col-large decisions (#1034)

Slice session, 2026-11-21Z, from #1013. X-ui-guards banned breakpoints that aren't on the §10.2 ramp (`sm:`, `md:`, `lg:`, …) and removed the dead `sm:grid-cols-2` classes. Those classes never applied (the theme resets Tailwind's breakpoints, so `sm:` doesn't exist), but they marked where three screens meant to have two columns.

## The change
- **`large:grid-cols-2`** (640 px, `--breakpoint-large`; docs/design-tokens.md §5, the ramp's name, used across `web/src`) on three grids, one column below it:
  - the install dialog's offer list (`companions/InstallDialog.svelte`), a `<dl>` of label | value pairs;
  - the Unraid template's fields (`companions/Templates.svelte`), the same;
  - the Logging screen's per-component levels (`logging/ComponentLevels.svelte`), a label + select per component.
- No hex, no arbitrary value; the `<dt>`/`<dd>` pairs fill the two columns left to right, so each label sits beside its value. The gaps follow the repo's label | value lists (`FieldKeys.svelte`, `JobsScreen.svelte`): the stacked gap below `large:`, and `large:gap-x-4` between the columns.
- **The install dialog is capped at `max-w-lg` (512 px),** whatever the viewport, so two half-width columns would squeeze its 64-character checksum. The checksum pair therefore takes the full width under its label (`large:col-span-2`); the other five rows are label | value.

## Checked (throwaway Playwright screenshots, 390, 1024 and 1440 px, deviceScaleFactor 1, looked at)
- No horizontal scroll at any width of the three screens (`scrollWidth <= clientWidth`, 9 of 9).
- At 390 px each is one stacked column, as before. From 640 px the dialog and the Unraid fields read label | value in aligned rows; the per-component selects sit in two columns with their labels above.
- Focus order: the DOM order is unchanged, and a row-major grid reads left to right, top to bottom, so the tab order is what it was (System, DHT, Fetch, Pipeline, …).
- The screenshot specs were scratch files and aren't committed. The companions, settings-logging and logs e2e specs pass (13 of 13, core and keyboard-only).

## Left undone
`large:items-baseline` (a label's and a value's text on one baseline) and `large:items-end` (each level select level with its neighbour if a label wraps) were reviewer polish. They cost 12 B of global CSS and neither changes today's layout (short one-line labels), so against the smallest-UI priority they are not added (runner call, §2.26).

## Snapshots and size
- **No snapshot changes:** no baseline shows these screens (the Settings smoke shot is `/settings/general`).
- **One size-limit raise (runner-approved, §2.26): "CSS" 19.79 → 19.80 kB**, measured 19,792 B: the `large:` utilities of the three lists (`grid-cols-2`, `col-span-2`, `gap-x-4`), +9 B over `main` (19,783 B). The edits are otherwise class names in lazy chunks; no other entry moves. `npx size-limit` also shows "Search cards (lazy)" 2 B over locally, the build wobble the runner measured on `main` (CI is green there); this slice doesn't touch that chunk.
