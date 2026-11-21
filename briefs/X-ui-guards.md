# X-ui-guards

Runner brief, 2026-11-21Z. Slice id `X-ui-guards`, branch `slice/X-ui-guards`, PR title `X-ui-guards: <title>`. The plan entry is in `runner/slices.yaml`, and the spec is §10.2 and §10.17. Track U's later slices (X-page-frame, X-labels, X-ui-checks, X-safety-toggles and more) all wait on this one.

## What
1. **Weighted icons.** On main, 124 `~icons/ph/*` imports in `web/src` have no weight suffix. Migrate each to `-bold`, or to `-fill` where the design uses the filled glyph (the §10 icon rules; follow what the same icon uses elsewhere).
   - This is a no-visual-change migration apart from the weight itself.
   - Then add a lint ban (eslint and/or `scripts/bans.mjs`) that fails on any unweighted `~icons/ph/` import, with tests in `bans.test.ts`.
2. **Dead breakpoint classes.** The theme removed Tailwind's `sm:`/`md:`/`lg:`/`xl:`/`2xl:` screens, so those classes do nothing. On main there are about four, for example `sm:grid-cols-2` in the companions dialogs and logging levels, and `xl:p-6` in SettingsShell.
   - Remove them, which changes nothing visually. If one plainly meant a layout the screen now lacks, keep it out of this slice and send me the file and line, and I'll queue it.
   - Then ban the prefixes, with tests. Make sure the ban doesn't trip on non-class text such as a JS parameter named `md`.
3. **Axe on fixture data in CI** (`web/e2e/axe.fixture.test.ts`). It covers /insights with fixture data (review B3) and the wizard, with no serious or critical violations, and runs in the e2e workflow. Use the existing fixtures (X-insights-funnel-fixture). Wait for conditions with deadlines of at least 10 s, and never use fixed sleeps.
4. A decisions file, and this slice's own `slices.yaml` gate or placement entries if needed.

## Owns
The plan entry: `web/eslint.config.js`, `web/scripts/{bans.mjs,bans.test.ts,check-bans.mjs}`, `web/e2e/axe.fixture.test.ts`, `.github/workflows/e2e.yml`, `web/src/dev-routes/ui/controls/+page.svelte`, and `web/src/lib/**`, where the only edits are the import lines and the dead classes.
- The runner is holding every other web slice while this runs.
- Icon imports under `web/src/routes/**` are in scope too, for the same import-line edits only.
- Send me the list before you edit anything else.

## Bars
`runner/standards.md`. Run all of `web/`'s gates from CLAUDE.md: check, vitest, build, size-limit and playwright. Weighted icons may change chunk sizes by a few bytes. If a limit trips, send me the numbers; never raise a limit yourself. Bring main in and re-run `npm run check` just before you send "ready". Push once per round of fixes, and send "ready <pr> <sha>" when CI is green.
