# S-updates-5: the Roll back dialog says whether your data goes back too
**Model:** Sonnet (Updates screen, the S-updates pattern; the server enforces every rule). The runner reviews. Runner brief, 2026-11-01. It uses B-update-6 (#330).

## Why
S-updates-4 (#326) put **Roll back** behind a confirm dialog that always says the store returns to its pre-update snapshot and later changes are lost. That's only true when the update ran a migration and the snapshot is kept. #330 added `rollback.restores_store` (optional `bool`) to `GET /updates`:
- `true`: the rollback restores the pre-migration snapshot, so data and settings changed since the update are lost.
- `false`: the store is kept as it is; nothing is lost.
- **absent**: the check didn't run (e.g. a pending restart answers `busy` first). Don't claim either.

## Read first
- `docs/architecture.md` §12.2 (manual rollback; what's restored), §10.10.14 (Updates).
- `docs/decisions/2026-11-01-B-update-6.md` (the field and when it's absent), `docs/decisions/2026-11-01-S-updates-4.md`.
- `web/src/lib/features/settings/updates/**` (the Roll back button and dialog), `web/src/lib/api/schema.d.ts` (`rollback.restores_store`).

## Do
1. **The dialog's text follows the field:**
   - `true`: as today (the app restarts on the previous version; your data returns to its state from before the update; changes since are lost).
   - `false`: the app restarts on the previous version; your data and settings stay as they are.
   - absent: the app restarts on the previous version, with no claim about the data. Keep Roll back available exactly as `rollback.available` says; this field never hides or shows the button.
   - Plain words, en **and** zh. Reuse the existing strings where one fits.
2. If the field changes while the dialog is open (an `updates` event refreshes the status), the text follows it; the confirm still sends only `{confirm: true}`.
3. **Tests:** vitest for each of the three cases (each fails if the text ignores the field), and that Cancel still sends nothing. Condition waits only.
4. **Sizes:** Updates entries stay under their limits; never raise one. Report deltas.
5. **Decisions file:** `docs/decisions/<date>-S-updates-5.md` (short).

## Own
`web/src/lib/features/settings/updates/**` (incl. messages), the updates e2e spec if a case needs it, your decisions file.
**Don't edit** `crates/**`, `web/src/lib/api/**`, other features, or `vite.config.*`. Don't run formatters over files you didn't change; check each commit's file list (`git show --stat`) before pushing.

## Size
≲ 200 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit`, and `npx playwright test` for settings*/updates* and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
