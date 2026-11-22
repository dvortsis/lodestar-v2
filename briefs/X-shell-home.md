# X-shell-home (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: UI. Sonnet is fine; every result gets a fresh Opus review before merge. Deps merged: X-page-frame, X-ui-plan-r5 (and X-ui-guards).

## Goal
The findings P5, P6, U27, D11 and 02 §4d. Read each in `01-review.md`, `02-skills-pass-and-future-modules.md`, `03-*` and `04-plan-and-flow-review-moderate-user.md`, and their spec sections (§10.3, §10.4):
- The status pill says what Lodestar is doing in plain words (U27). WK-17 later adds a worker count to the pill, so leave room for it; don't build it.
- The top bar fixes P5 names: the pill, the ⌘ glyph, the overflow menu's items, and the page shift.
- The Search empty state becomes the home screen (P6, D11): what Lodestar is, what to type, and the user's recent searches (02 §4d).
- Recent searches stay in this browser only and Sign out clears them. `web/src/lib/features/search/assist/recent.ts` already keeps a list; reuse it, don't add a second store.

## Step 0 (send me, then wait for "go")
1. **Each finding:** what it asks, what the screen does now (file:line), and your change. Add a before screenshot at 1440 px and at 390 px for each screen you'll change (local only; don't commit them).
2. **Every message key** you add or change, in en and zh.
3. **Size:** the size-limit entries your change falls under, with the main figures. Your estimate per entry.
   - `.size-limit.mjs` isn't yours yet. The queue is X-detail-depth (now), then you on request. Plan to pay for size with removals first.
   - No raise is allowed for the global CSS (19.8 kB), Alert, Welcome or the Search route (45 kB). The shell and the empty state are on the Search route's first view, so the home screen must fit in it: lazy-load what isn't needed at first paint, or pay for it with removals.
4. **Files outside owns,** with why. Plus files and an estimate.
5. **Where the Search empty state lives** today (it is not in a `search/empty/` folder), so the owns can name it exactly.

## Rules
- The design decided with the owner comes first (§10; CLAUDE.md "UI rules"). Tokens only, no hex, no arbitrary values, spacing on the §10.2 ramp.
- Copy follows `docs/ui-glossary.md`. It's plain, for a moderate user (§2.14).
- Accessibility at WCAG 2.2 AA. Targets meet the §10 minimums. The pill's text is its accessible name; any change it announces goes to one polite live region, never on every poll.
- No layout shift: the top bar and the home screen render at their final size in the first frame (measure CLS in an e2e, as X-search-toolbar did).
- No fixed sleeps in tests; condition waits get deadlines of at least 10 s.
- Never beside X-detail-depth (detail/**, compare/**, routes/t/**), X-narrow-states (logs/**, discover/**, settings/about/**) or X-hit-resolution (search/table/**, the search messages' `search_table_col_res` key, the API's search DTO). Ask before touching any file outside owns.
- `web/src/lib/features/search/messages/{en,zh}.json`: edit them only after #1079 (X-hit-resolution) merges; merge main first.
- Screenshots: the top bar is in every baseline. Don't push the update-snapshots marker until #1080 (X-narrow-states) has merged and I say so; then keep only the PNGs your change moves, and list them.

## Owns
- `web/src/lib/shell/StatusPill*`, `web/src/lib/shell/TopBar.svelte`, `web/src/lib/shell/OverflowMenu*`, and the shell's en and zh messages for those parts.
- The Search empty state's component and `web/src/lib/features/search/assist/recent*` (name them at step 0).
- Your own e2e specs, plus `web/e2e/shell.routes.test.ts` and `web/e2e/shell.keyboard.test.ts` where the pill or top bar text changes.
- `docs/decisions/2026-11-22-X-shell-home.md`
- Refine at step 0.

## Branch and PR
- Branch `slice/X-shell-home`.
- PR title "X-shell-home: Status pill in plain words, top bar fixes, the Search empty state as home with recent searches".
- Gates, in `web/`:
  - `npm run check`, `npx vitest run`, `npm run build` and `npm run size:ci`;
  - the e2e specs for the screens you touch. Set `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and never run `playwright install`.
- Add a `runner/slices.yaml` change only if the runner asks; the entry exists.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
