# Brief: X-sources-restructure (Settings → Sources layout)

**Owner decision:** docs/owner-decisions.md §2.66 item 4, and docs/decisions/2026-11-13-runner-ui-review-decisions.md ("Settings → Sources").
**Model:** helper work (Svelte, pattern-following), reviewed by a fresh Opus reviewer. Run `/security-review` before the PR, because the rows' switches open the consent dialogs.
**Branch / PR:** `slice/X-sources-restructure`, PR titled `X-sources-restructure: <title>`.

## Goal
Restructure Settings → Sources before the next plug-in row lands (IRC-2, NOS-3, LEM-1 and the ENR-<plugin> slices all depend on this):
1. **Collapsible groups,** each with a one-line summary: how many sources in the group are on, and any that need attention.
2. **A filter box** that narrows rows by name or kind, with an empty state when nothing matches.
3. **One-line rows that expand** to show the current detail: settings, status, help.
4. **A consent-class legend** naming the classes the rows already show (Open, Account, Restricted, Overlay, External, as the code calls them), using the existing glossary words.

## Hard limits
- **No change to the consent dialogs, or to what any switch does.** The strings test over the §2.49 texts stays and must pass unchanged.
- Keyboard: groups and rows are buttons with `aria-expanded`; the filter is labelled; focus order is sensible; axe is clean at 390, 1366 and 1440 px.
- Tokens only. No hex and no arbitrary values. Spacing from the §10.2 ramp (docs/design-tokens.md §5). Atkinson fonts and Phosphor icons at a weight, as existing screens use them.
- Strings in en and zh message files, following the existing feature pattern. Glossary words from docs/ui-glossary.md.
- Size: `web/.size-limit.mjs` may change only for the Sources entry; justify any growth.

## Owns
- `web/src/lib/features/sources/**`
- `web/e2e/settings-sources*.test.ts`
- `web/.size-limit.mjs` (its own entry only)
- **Also, one-off (runner, 2026-11-13):** `web/src/lib/features/settings/form/SettingsForm.keys.test.ts`, only to add `vi.setConfig({ testTimeout: 30_000 })` as the sibling tests do. This is rv-811 I1: vitest's 5 s default cuts the 10 s `waitFor` deadline.

## Tests
- vitest for filtering, group summaries, expand and collapse, and the legend.
- An e2e spec at 1366 and 390 px: filter, expand a row, toggle a switch, and check the consent dialog opens exactly as before.
- No fixed sleeps. Condition waits with deadlines of at least 10 s.

## Before pushing
`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and the touched Playwright specs (PW_CHROMIUM_PATH=/opt/pw-browsers/chromium). Then a self-review with a fresh reviewer, and `/security-review`.

## Decisions file
Write `docs/decisions/2026-11-13-X-sources-restructure.md` only if you make a choice the owner decision leaves open, such as the default open or closed state of the groups. Name it in the PR body for the runner to flag (§2.19).
