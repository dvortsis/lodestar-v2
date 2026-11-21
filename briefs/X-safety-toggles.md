# X-safety-toggles

Runner brief, 2026-11-21Z. Slice id `X-safety-toggles`, branch `slice/X-safety-toggles`, PR title `X-safety-toggles: <title>`. The plan entry is in `runner/slices.yaml`; the findings are I4, I5, U21 and U32 in `docs/review/2026-11-13-ui-ux/01-review.md` and `04-plan-and-flow-review-moderate-user.md`, placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Opus builds it, and it gets `/security-review`.

## Why
Filters & Safety is the screen that keeps a user safe, and it's the hardest to read. Rules show only as codes (`CS-2a`, `MW-4@1`), packs as lower-case mono ids, the confirm dialog never says what the rule checks, and turning off a child-safety check uses the ordinary primary button. Switches elsewhere read as a state ("Paused" off on an active alert; "On" shown off next to "Off").

## What
1. **Rule and pack names with descriptions (I5).** Each rule row and each pack shows a plain name and a one-line description, with the code kept as secondary mono text (it's what the docs and logs name). The API has no description field (`PackRuleState`, `CoreRuleState`), so the names and descriptions are en and zh messages keyed by rule id; reuse the pack texts that already exist (`filters_pack_info_*`, and the wizard's `wizard_pack_*_text` for tone). A rule id with no message falls back to the code alone, never to an empty label. Add a unit test that every built-in rule id has a name and description: read the ids from the repo's built-in pack source (find where the packs are defined under `crates/`) at test time, or from a fixture that a test proves matches it. Don't edit `crates/`.
2. **The confirm dialog** (`flows/ChangeDialog.svelte` and the other dialogs that name a rule) is titled with the rule's name, not its code, and says what the rule checks.
3. **Danger style for loosening a child-safety rule.** The button that turns off or weakens a CS rule uses the danger style (design tokens, `docs/design-tokens.md`; no hex). Strengthening keeps the ordinary style. Nothing about what the flow does changes: the child-safety core stays present, and its rules change only through the weakening flow (§2.39). Read `docs/owner-decisions.md` §2.39 and the filters decisions before touching the flow.
4. **Packs collapsed by default,** with a disclosure per pack (a real `<button aria-expanded>`), so the screen is short on a phone; the "Version" row that lists rule ids gets a plain label. Measure the 390 px page height before and after and put both in the decisions file.
5. **Switches say what on means (I4, U21).** The alert switch is labelled for its on state ("Active", or whatever `docs/ui-glossary.md` uses) and kept true; the Sources switch likewise ("Enabled"), with the state word beside it only if the glossary calls for it. Text and `aria` only in `features/alerts/PauseToggle.svelte` and `features/sources/SourceRow.svelte`; their behaviour doesn't change. Update `docs/ui-glossary.md` only if it has to change, and tell me first (it's a contract doc).
6. **Tests:** unit tests for the labels and fallback, and the existing e2e specs (`web/e2e/filters-*.test.ts`, the alerts and sources specs) updated for the new names. Snapshots only where a screen really changed; regenerate them in CI's way (see `web/e2e/README.md`), and list each one in the PR.
7. **A decisions file** `docs/decisions/2026-11-21-X-safety-toggles.md`, heading with `(#<this PR>)`: the label choices, the page heights, and any UX call you made (a runner call under §2.26; name it so I can flag it).

## Owns (refined at launch)
- `web/src/lib/features/filters/**`
- `web/src/lib/features/alerts/PauseToggle.svelte`, `web/src/lib/features/alerts/messages/en.json`, `web/src/lib/features/alerts/messages/zh.json`
- `web/src/lib/features/sources/SourceRow.svelte`, `web/src/lib/features/sources/messages/en.json`, `web/src/lib/features/sources/messages/zh.json`
- `web/e2e/filters-*.test.ts` and the alerts and sources e2e specs and snapshots the label change touches
- `docs/decisions/2026-11-21-X-safety-toggles.md`

Send me the exact file list before you edit anything else, and before any size-limit change (with the measured numbers).

## Bars
`runner/standards.md`, CLAUDE.md's web checks (`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test` for the changed specs, `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`). Phosphor icons are `-bold` (or `-fill` for an on state) since X-ui-guards. Push once per round, and send "ready <pr> <sha>" when CI is green.
