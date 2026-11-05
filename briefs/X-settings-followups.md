# X-settings-followups: the small follow-ups from #496's review
**Model:** Sonnet (test and docs, following an existing pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-05T14:35Z.

## Why
X-settings-trim (#496, merged at 05ae7814) moved the field menu's choice handler into `web/src/lib/features/settings/form/menu.ts`. Its review found two things to tidy up:
- no test covers "Copy link". Only Reset and Show keys are tested, in `SettingField.test.ts:186-241`;
- `docs/decisions/2026-11-05-X-settings-trim.md` misstates some figures.

## Do (branch `slice/X-settings-followups` from `origin/main`)
1. **Add a vitest test for "Copy link".** Choosing it calls `navigator.clipboard.writeText` exactly once, inside the click, with the field's absolute URL: `new URL(form.href(key), location.href).href`. Follow the existing menu tests' pattern. Waits are on conditions (the `waitFor` 10 s deadline), never fixed sleeps.
2. **Correct the X-settings-trim decisions file** (lines ~24-27):
   - The NotificationsScreen limit *was* lowered, from 45.14 to 45 KB.
   - Other entries did move: controls (on demand) +67 B, edit chunk (on demand) +84 B, Wizard steps −107 B, and four other Settings screens about −110 B each.
   - The "before" values that read "not recorded" are 33,393 B (controls) and 11,011 B (edit chunk).
   - Add a note on headroom: NotificationsScreen has 12 B left and Settings section 48 B under 45 KB. Each new registry entry adds about 65–85 B to both, so the next Settings screen needs a trim or a §2.26 raise.
3. **Wrap the comment at `FieldSaved.svelte:3`** to the file's ~100-column wrap. No code change.
4. **Add your `runner/slices.yaml` entry** with a placement under ws-settings, and run `cd tools/status-connector && npm test`.

## Own
`web/src/lib/features/settings/form/SettingField.test.ts` (or a new `menu.test.ts`), the comment in `FieldSaved.svelte`, `docs/decisions/2026-11-05-X-settings-trim.md`, and your slices.yaml entry. Change no other code. Size limits must stay as they are.

## Done when
In `web/`, all of these pass: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npm run e2e:typecheck`. Then open the PR "X-settings-followups: Copy-link test and #496 record corrections". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round.
