# X-settings-leave-guard-split

Runner brief (2026-11-21). Branch `slice/X-settings-leave-guard-split`; PR title `X-settings-leave-guard-split: keep the leave guard out of the wizard's steps`.

## Why (UX and size, runner's call under §2.26)
X-session-draft-keep (#999) added a leave guard to the settings form: a prompt before leaving with unsaved changes, plus the draft keep. The first-run wizard embeds the same form in `FieldsStep` and `UpdatesStep`, so the wizard's steps now load the guard too, although the wizard has its own flow. The runner then raised "Wizard steps (on demand)" in `web/.size-limit.mjs` to 54.4 kB, the measured overage rounded up (2026-11-20T22:1xZ). That raise is to be given back: about 0.67 kB.

## Do
1. **Measure first.** Run `npm run build && npx size-limit` on fresh main, and record "Wizard steps (on demand)" and every Settings entry.
2. **Split the leave guard out of `SettingsForm`'s static graph.** Settings routes keep it; the wizard's embed doesn't load it. Choose whichever fits the code: a prop or slot the settings route passes in, a separate wrapper component the settings pages use, or a lazy import that only the settings pages trigger.
   - **Behaviour must not change on Settings:** the same prompt, the same draft keep and the same block epoch (`web/src/lib/features/auth/session.svelte.ts`).
   - **The wizard gets exactly what it has today, minus the guard.** Check what the wizard does when you leave mid-step, and confirm in the decisions file that nothing it relied on came from the guard.
3. **Lower the limit.** Set "Wizard steps (on demand)" to the new measured size, rounded up to the next 0.01 kB, so it gives back what the split saved. No other entry may grow. If one does, make it lazier, or send the runner its numbers before touching its limit.
4. **Tests.**
   - Existing settings-form, draft and leave-guard unit tests and e2e stay green with no edits to their expectations.
   - Add a unit or e2e check that the wizard's step modules don't import the guard. For example, a size-limit entry, or a manifest check like the other `ownChunk` rules in `.size-limit.mjs`, so the split can't silently regress.
   - Run `npx playwright test` for the settings and wizard specs, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`.
5. Add `docs/decisions/2026-11-21-X-settings-leave-guard-split.md` with the before and after sizes.

## Owns
- `web/src/lib/features/settings/form/**`
- the wizard's embed users (`web/src/lib/features/wizard/FieldsStep.svelte`, `UpdatesStep.svelte`) and the settings route files that mount the form, only where the guard is passed in
- `web/.size-limit.mjs` (the Wizard steps line, plus a new guard check if you add one)
- their unit tests, any new e2e file, the decisions file, `runner/slices.yaml` (your entry) and `runner/dashboard.yaml` (your placement line, only if no rule covers `X-`)

Send the runner the exact list of route files before you edit outside `settings/form/**` and the two wizard steps.

## Not
- No change to `web/src/lib/features/auth/**` or `web/src/lib/api/**`. No hex values; use tokens. No new dependency. No fixed sleeps in tests.

## Checks
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test` (settings and wizard specs). `cd tools/status-connector && npm test`.

When CI is green on your head, send the runner `ready <pr> <sha>`.
