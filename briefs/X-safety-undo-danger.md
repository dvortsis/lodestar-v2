# X-safety-undo-danger

Runner brief, 2026-11-21Z. Slice id `X-safety-undo-danger`, branch `slice/X-safety-undo-danger`, PR title `X-safety-undo-danger: <title>`. The plan entry is in `runner/slices.yaml` (added by runner records r7, `docs/decisions/2026-11-21-runner-records-r7.md`). It follows X-safety-toggles (#1019; read `docs/decisions/2026-11-21-X-safety-toggles.md`). Opus builds it, and it gets `/security-review`.

## Why
Since #1019, any change that loosens child safety is confirmed with the danger style: a CS rule turned off or weakened, a floor entry removed, or a CS hold released or marked a false positive. Undo is the gap. The screen sends `{op: "undo"}` (`screens/ops.ts`, `flows/changes.ts` `undoChange`) and learns what it restores only from the preview the server returns. So an Undo that puts back a weaker rule set is confirmed with the ordinary style.

## What
1. **The Undo button on the screen stays as it is** (`FiltersScreen.svelte`, `data-testid="filters-undo"`). It can't know what it restores before the preview, so it keeps the ordinary style.
2. **The confirm step takes the danger style when the preview shows the Undo loosens child safety.**
   - Find where the dialog (`flows/ChangeDialog.svelte` and what it renders) picks the apply button's style, and how #1019 passes the danger style for the other loosening flows.
   - For an `undo` change, derive it from the returned preview. Read the preview type in `web/src/lib/api/schema.d.ts` and what each field means on the server (find where the preview is computed under `crates/`, read only, don't edit `crates/`).
   - The slice entry names `core_caught_30d` and `removed_from_floor`. Confirm from the server code which preview fields mean that child-safety protection gets weaker, and use exactly those. Any non-zero loosening field means danger; zero, or a preview that only strengthens, keeps the ordinary style.
   - Before the preview arrives, or if it fails, keep the ordinary style. The dialog never shows the danger style on a guess.
   - If the preview has no field that can tell a CS loosening apart, stop and tell me before building anything.
3. **Nothing else changes (§2.39).** The flow, the requests, the password step, the shadow offer and what Undo does all stay the same. This is styling, plus the text the danger style already carries in the other flows, if any (reuse the existing messages; en and zh both).
4. **Accessibility.** If the danger style in the other flows comes with a visible or `aria` cue beyond colour, Undo's danger state gets the same one. Colour is never the only signal.
5. **Tests.**
   - Unit tests: an Undo whose preview loosens child safety gets the danger style; one that doesn't keeps the ordinary style; the style before the preview resolves and after it fails is ordinary.
   - Mutation-check them: removing the derivation, or flipping the field test, must fail a test.
   - Update the existing filters e2e specs only where they must change. Snapshots only where a screen really changed, regenerated CI's way (`web/e2e/README.md`), each listed in the PR.
6. **A decisions file** `docs/decisions/2026-11-21-X-safety-undo-danger.md`, heading with `(#<this PR>)`: which preview fields count and why, with the server file:line, and any UX call you made (a runner call under §2.26; name it so I can flag it).

## Owns
- `web/src/lib/features/filters/**`
- `docs/decisions/*-X-safety-undo-danger.md`
- the `runner/slices.yaml` entry for this slice only
- filters e2e specs and snapshots only if a change forces it; tell me the file list first

Send me the exact file list before you edit anything outside `features/filters/`, and before any size-limit change (with the measured numbers; a raise is the measured size rounded up to 0.01 kB, after the code is as lazy as it can be).

## Bars
`runner/standards.md`, CLAUDE.md's web checks (`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test --trace off` for the changed specs, `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; clear `node_modules/.vitest-cache` if a glob-based test count looks short). Design tokens only, no hex, no arbitrary values. No fixed sleeps. Push once per round, and send "ready <pr> <sha>" when CI is green. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
