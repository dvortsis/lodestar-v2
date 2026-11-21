# X-grid-two-col-large (brief, runner 2026-11-21)

Slice entry: runner/slices.yaml `X-grid-two-col-large`. Owns: web/src/lib/features/settings/companions/InstallDialog.svelte,
web/src/lib/features/settings/companions/Templates.svelte,
web/src/lib/features/settings/logging/ComponentLevels.svelte, plus your decisions file (add it to owns).

## Problem (from #1013, X-ui-guards)
X-ui-guards banned breakpoints that aren't on the §10.2 ramp and removed the dead `sm:grid-cols-2`
classes, so these three screens lost their two-column layout at wide widths.

## Do
1. Restore two columns on the §10.2 `large:` breakpoint (docs/design-tokens.md §5; check the exact
   variant name used elsewhere in web/src, e.g. grep for `large:`). One column below it.
2. Check each screen at 390, 1024 and 1440 px (Playwright screenshots you look at; deviceScaleFactor 1):
   no horizontal scroll, labels and controls aligned, focus order still top-to-bottom, left-to-right.
3. Run the e2e specs that cover these screens and list any snapshot that changes in the PR. Regenerate
   snapshots only the CI way (the marker-file flow).
4. Bans: no hex, no arbitrary values; npm run check passes. Size: report any size-limit entry that
   moves; don't raise a limit without sending the runner numbers first.
5. No fixed sleeps; condition waits ≥10 s.
