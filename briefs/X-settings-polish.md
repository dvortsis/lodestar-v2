# X-settings-polish: small Settings engine follow-ups from earlier reviews
**Model:** Sonnet (well-specified polish in existing patterns). The runner reviews the PR. Runner brief, 2026-11-01. These are review follow-ups the runner logged in `state.json` → `followups` on `runner-state`. No behaviour change beyond what each item names.

## Items (do each; if one turns out bigger than stated or wrong, skip it and say why in the PR)
1. **Stale `editing` flag** (#264 review). `web/src/lib/features/settings/form/SettingField.svelte` sets `editing` on focusin and clears it on focusout. When a field turns automatic while its box has focus, the box is removed without a focusout, so `editing` stays true. Reset it when automatic turns true (in `setAutomatic` or an effect on that state). A vitest that fails without the fix.
2. **Size-limit map pick** (#264 review). In `web/.size-limit.mjs`, where a section's message maps are measured (search `settingsMessages` and the comment after it), the "largest map" is picked by raw size. Pick it by gzip size, which is what the limits measure. No limit changes; report whether any measured number moves.
3. **Lift the reveal + hash-focus logic** (#277 review). `settings/shell/SectionForm.svelte` and `settings/screens2/GroupedForm.svelte` each carry the same reveal-a-field-and-focus-from-`#hash` logic. Move it into one module (`settings/form/reveal.svelte.ts`) used by both. Same behaviour; the existing tests must pass unchanged. Add the missing unit test for SectionForm's `#field` link wait (#264 review).
4. **Lazy load-failure message** (#277 review). `settings/screens2/Lazy.svelte`: the load-failure message has no live role, so screen readers miss it. Give it `role="alert"` (or `status` if it isn't urgent; follow how `LazyControl.svelte` does it). A vitest.
5. **Purpose line layout shift** (#293 optional 2). On generated Settings screens the purpose line arrives with the ~0.9 KB about chunk, so the "Loading settings" text can shift down once. Reserve the line's height while it loads (a token height from the §10.2 ramp, no arbitrary values). Settings baselines must not change; if any image moves, stop and tell the runner.

## Read first
CLAUDE.md, `docs/architecture.md` §10.10 (Settings), `docs/design-tokens.md` §5 (spacing), `web/src/lib/features/settings/form/README.md`, and the files named above.

## Own
`web/src/lib/features/settings/form/**`, `settings/shell/SectionForm.svelte`, `settings/screens2/{GroupedForm,Lazy}.svelte`, the `settingsMessages` lines in `web/.size-limit.mjs`, their tests.
**Don't edit** `features/filters/**`, `features/appearance/**`, `features/settings/updates/**`, `lib/shell/**`, `features/search/**`, `features/detail/**` (other slices are in them), or `vite.config.*`.

## Sizes
Every size-limit entry stays under its limit; never raise a limit. Settings first views must not grow by more than 0.05 KB in total, and CSS must not grow. Report every entry that moves.

## Size
≲ 300 changed lines. No decisions file unless an item changes a recorded decision.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for settings* and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, which items were done or skipped, and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
