# X-zero-rescue (runner brief, 2026-11-28)

**Slice:** X-zero-rescue (runner/slices.yaml). Sonnet, helper. An Opus reviewer checks it.
**Branch:** `slice/X-zero-rescue` from current `main`.
**PR title (exact):** `X-zero-rescue: Zero-result rescue: Did you mean, natural-phrasing chips only when asked, query-bar copy, no error on an unfinished token`
**Owns:** `web/src/lib/features/search/query/**`, `web/src/lib/features/search/assist/**`, its own
e2e specs, and its en and zh message keys (refine at step 0). Spec: §10.4, `docs/query-language.md`.

## Source
UI review 2026-11-13, `docs/review/2026-11-13-ui-ux/`, placed by
`docs/decisions/2026-11-14-runner-ui-review-placement.md`: **U8, U9** and the query-bar items of
**P10**. Read those items first.

## Do
1. **Step 0.** Read the review items and the current code, then send the runner:
   - the exact behaviour you'll build for each item;
   - the files (owns, refined);
   - any message keys;
   - expected size changes (`web/.size-limit.mjs` is not in owns; ask if a raise is needed).

   Wait for the reply.
2. **Never run alongside X-alert-noun** (search messages) or X-units-typed (`clauses.ts`). Neither
   is running now.
3. Follow the owner's design rules (CLAUDE.md "UI rules"):
   - tokens only, no hex;
   - the spacing ramp;
   - Phosphor -bold icons;
   - minimalist-visual-style;
   - a11y: axe-clean, keyboard paths, announced changes.
4. **Tests:**
   - vitest unit tests for the logic: "Did you mean", and a token being typed is not an error;
   - an e2e spec for the zero-result rescue;
   - condition waits only, no fixed sleeps.
5. Before pushing, run all of these in `web/`:
   - `npm run check`
   - `npx vitest run`
   - `npm run build`
   - `npm run size:ci`
   - Playwright, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never run `playwright install`.
6. Decisions file: `docs/decisions/<date>-X-zero-rescue.md`.
7. Follow `.claude/skills/steward/SKILL.md`. Report `ready <PR> <sha>` to the runner session.
