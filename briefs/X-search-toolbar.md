# X-search-toolbar (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: helper (Svelte), Sonnet, with an Opus review. Deps merged: X-results-drawer-adaptive, X-ui-guards.

## Findings in scope
- **I2 and L4:** about 300 px of controls above the first result row on a phone and at 200% zoom.
- **I15, P7, L3.**

Read each finding in the review folder before planning. Don't take more than these; another slice owns the rest of the review.

## Step 0 (send me, then wait for "go")
1. **Per finding:** what the user sees now, what changes, and the screen widths you'll check (390, 1024 and 1440 px, plus 200% zoom at 1024).
2. **Refined owns:** the entry says "refine owns at launch". List the exact files, including your e2e specs and the en and zh message files (keys only for the search toolbar).
   - The `search_toolbar_save*` keys belong to X-alert-noun, which renames that noun everywhere. Don't own or rename them.
   - If you need `web/.size-limit.mjs`, name the entries.
3. **Size:** measure the Search route and the entries you touch with `npm run size:ci` on main first, and give the headroom. The Search route JS cap is 45 kB with about 50 B of headroom (the owner's smallest-UI priority), and it won't be raised. Say how the change stays inside it: move code out of the route, or cut it.
4. Files and an estimate.

## Rules
- The owner's design (§10 of `docs/architecture.md`) comes first, then `docs/design-tokens.md` and `docs/ui-glossary.md`. `ui-design-skills` only fills gaps. Use tokens and no hex values, and spacing only from the §10.2 ramp.
- Keyboard and screen-reader behaviour must not regress. Run axe on the specs you touch.
- No fixed sleeps in tests. Wait on conditions with deadlines of at least 10 s.
- No change to what a search sends or returns.

## Owns (to be refined at step 0)
- `web/src/lib/features/search/toolbar/**`
- `web/src/lib/features/search/live/**`
- `web/src/lib/features/search/table/**`
- `web/src/lib/features/search/cards/**`
- `docs/decisions/2026-11-22-X-search-toolbar.md`

## Branch and PR
- Branch `slice/X-search-toolbar`. PR title "X-search-toolbar: <what the user gets>", named at step 0.
- Gates: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never run `playwright install`).
- Screenshots before and after at 390 and 1440 px go in the PR description.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
