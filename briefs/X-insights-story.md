# X-insights-story (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: helper (Svelte), Sonnet, with an Opus review. Deps merged: X-ui-guards, X-labels, X-page-frame. X-charts waits on this slice.

## Findings in scope
- I25, U25, I9 (the rest after #823), P14 (order), P16, U26 (Undo for 10 s, a runner call already recorded), the cohort copy of P10, 03-n and 03-q.
- Analytics-validation cases per the G-v1-ui gate.

Read each finding in the review folder before planning. The charts' own findings (I8, I20, I26, P1, I24, L5, I27, 03-p) are X-charts', not yours.

## Step 0 (send me, then wait for "go")
1. **Per finding:** what the user sees now, what changes, and how you'll check it. Use the analytics-validation skill's idea: values computed independently from the fixture and compared with what's shown.
2. **Refined owns:** list the exact files, including your e2e specs, the Insights en and zh messages, and `web/.size-limit.mjs` entries if needed (the Insights stage pages entry only).
   - The open X-pseudo-text-gaps work is merged (#1056). Check its Insights message keys and don't undo them.
3. **Size:** measure the Insights entries with `npm run size:ci` on main first, and give the headroom. No raises without measured numbers sent to me first.
4. **Any server change** (a new field, or a changed query) is out of scope. Name it and stop.
5. Files and an estimate.

## Rules
- The owner's design (§10) comes first, then `docs/design-tokens.md`, `docs/ui-glossary.md` and `docs/metrics.md` for what each number means. `ui-design-skills` only fills gaps. Use tokens and no hex values, and spacing from the §10.2 ramp.
- The words are for someone who may not know what a DHT is: plain and matter-of-fact. Any metric name shown to a user follows `docs/ui-glossary.md`.
- No fixed sleeps in tests. Keyboard and axe on the specs you touch.

## Owns (to be refined at step 0)
- `web/src/lib/features/insights/overview/**`
- `web/src/lib/features/insights/stages/**`
- `web/src/routes/insights/**`
- `docs/decisions/2026-11-22-X-insights-story.md`

## Branch and PR
- Branch `slice/X-insights-story`. PR title "X-insights-story: <what the user gets>", named at step 0.
- Gates: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never run `playwright install`).
- Screenshots before and after at 390 and 1440 px go in the PR description.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
