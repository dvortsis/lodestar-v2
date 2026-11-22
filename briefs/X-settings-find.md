# X-settings-find (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: helper (Svelte), Sonnet, with an Opus review. Deps merged: X-page-frame, X-session-draft-keep, X-ui-plan-r5. X-client-handoff waits on this slice.

## Findings in scope
- I12, I18, U29, P3, P4 and P17.
- U1's non-conflicting part: the Overview lists the steps the user skipped. The three-step setup instead of the wizard stays declined (owner_queue n 28).
- Spec: `docs/architecture.md` §10.10.3.

**I18 and U32 may be fixture artefacts.** Before hiding any control "that does nothing", confirm on a real install (run the app locally with `cargo run`, or use the `run` skill) that it really does nothing. If it does something there, it stays.

## Step 0 (send me, then wait for "go")
1. **Per finding:** what the user sees now, what changes, and how you'll check it (widths 390, 1024 and 1440 px).
2. **Refined owns:** the entry says "refine owns at launch". Exact files, your e2e specs, and the en and zh message keys for Settings and the palette. `web/.size-limit.mjs` entries only if needed, by name.
3. **Size:** measure the Settings entries and the command palette with `npm run size:ci` on main, and give the headroom. No raise without measured numbers sent to me first.
4. **Field search:** what it indexes (labels, help text, section names), where the index is built (build time is preferred, so no runtime cost on the route), and what a result does: jump to the field and focus it. It must work with the keyboard alone.
5. Files and an estimate.

## Rules
- The owner's design (§10) comes first, then `docs/design-tokens.md` and `docs/ui-glossary.md`. `ui-design-skills` only fills gaps. Tokens only, no hex, spacing from the §10.2 ramp.
- No change to what any setting does or to the save pipeline. No server changes.
- The draft kept across sign-in (X-session-draft-keep) must keep working. Its tests must pass unchanged.
- No fixed sleeps. Keyboard and axe on the specs you touch.

## Owns (to be refined at step 0)
- `web/src/lib/features/settings/shell/**`
- `web/src/lib/features/settings/overview/**`
- `web/src/lib/features/settings/form/**`
- `web/src/lib/shell/CommandPalette.svelte`
- `web/src/lib/shell/commands.ts`
- `docs/decisions/2026-11-22-X-settings-find.md`

## Branch and PR
- Branch `slice/X-settings-find`. PR title "X-settings-find: <what the user gets>", named at step 0. If over about 1,500 changed lines, propose a split at step 0.
- Gates: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never run `playwright install`).
- Screenshots before and after at 390 and 1440 px go in the PR description.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
