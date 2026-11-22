# X-narrow-states (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: UI. Sonnet is fine; every result gets a fresh Opus review before merge. Deps merged: X-ui-guards, X-page-frame.

## Goal
The findings I7, I17, I28 (Logs), P9, P12, P13, and the About copy of P10. Read each in `01-review.md`, `02-skills-pass-and-future-modules.md` and `04-plan-and-flow-review-moderate-user.md`, and their spec sections (§10.7, §10.10.12, §10.10.13, §10.15):
- Logs readable on a phone.
- Discover hides sections that aren't built, instead of showing empty shells.
- Filled empty states: a sentence, what to do next, and a link where it helps.
- About links.
- The narrow-layout fixes those findings name.

The zone part of I28 is already done (runner, 2026-11-21, #1023): Logs local times go through `displayTimeZone()` in `features/logs/text.ts`.

## Step 0 (send me, then wait for "go")
1. **Each finding:** what it asks, what the screen does now (file:line), and your change. Add a before screenshot at 390 px for each screen you'll change (local only; don't commit them).
2. **Every message key** you add or change, in en and zh.
3. **Size:** the size-limit entries your screens fall under, with the main figures. Your estimate per entry.
   - `.size-limit.mjs` isn't yours yet. The queue is X-insights-story-stages (now), then X-detail-depth, then you. Plan to pay for size with removals first.
   - No raise is allowed for the global CSS (19.8 kB), Alert, Welcome or the Search route.
4. **Files outside owns,** with why. Plus files and an estimate.

## Rules
- The design decided with the owner comes first (§10; CLAUDE.md "UI rules"). Tokens only, no hex, no arbitrary values, spacing on the §10.2 ramp.
- Copy follows `docs/ui-glossary.md`. It's plain, for a moderate user (§2.14).
- Discover hides an unbuilt section entirely. No "coming soon".
- Accessibility at WCAG 2.2 AA. Targets meet the §10 minimums.
- No fixed sleeps in tests; condition waits get deadlines of at least 10 s.
- Never beside X-detail-depth (detail/**, compare/**, routes/t/**), X-insights-story-stages (insights/**) or X-hit-resolution (search/table/**, the API's search DTO). Ask before touching any file outside owns.

## Owns
- `web/src/lib/features/logs/**`
- `web/src/lib/features/discover/**`
  - X-hit-resolution adds one `resolution: null` line to `discover/testing.ts`. Merge main before you edit that file.
- `web/src/lib/features/settings/about/**`
- Your own e2e specs. Their en and zh messages, inside those folders.
- `docs/decisions/2026-11-22-X-narrow-states.md`
- Refine at step 0.

## Branch and PR
- Branch `slice/X-narrow-states`.
- PR title "X-narrow-states: Logs on a phone, Discover hides unbuilt sections, filled empty states, About links, narrow-layout fixes".
- Gates, in `web/`:
  - `npm run check`, `npx vitest run`, `npm run build` and `npm run size:ci`;
  - the e2e specs for the screens you touch. Set `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and never run `playwright install`.
- A screenshot-changing edit needs the update-snapshots marker. Keep only your screens' PNGs from the regenerated set, then remove the marker.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
