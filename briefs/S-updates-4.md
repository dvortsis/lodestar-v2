# S-updates-4: "Roll back" on the Updates screen, and plain words for install failures
**Model:** Sonnet (Svelte screen work in the S-updates pattern; the server enforces every rule). The runner reviews. Runner brief, 2026-11-01. It uses B-update-5b (#324).

## Why
§12.2: "Manual rollback is always there: **Roll back** in Settings → Updates (the previous version, while its snapshot is kept), and `lodestar rollback`. Both go through the same journal." #324 added `POST /updates/rollback {confirm: true}` and made `GET /updates` → `rollback.available` truthful, with `rollback.reason` when it's false. Also open from #318: `install_failed` shows a raw stable code.

## Read first
- `docs/architecture.md` §12.2 (manual rollback, the restored-version notice "after a rollback, the card says what failed, what was restored, and what to do next", around l.1044), §10.10.14 (Updates).
- `docs/decisions/2026-11-01-B-update-5b.md` (the route's answers: `202`; `428 confirmation_required`; `409 rollback_unavailable` with `error.reason` ∈ `not_supervised`, `image_mode`, `busy`, `no_previous_version`, `snapshot_missing`; `409 updates_unavailable`), `docs/decisions/2026-11-01-S-updates-3.md`.
- `web/src/lib/features/settings/updates/**`, `web/src/lib/api/schema.d.ts` (`/updates/rollback`, the `rollback` object), `web/src/lib/api/client.ts` (`ApiError.reason`).

## Do
1. **The Roll back button** on the Updates screen, where §10.10.14 / §12.2 put it (with the previous version's name, e.g. "Roll back to 0.5.0"). Shown only when `rollback.available` is true; when false, nothing, or a muted line with the plain reason only if the screen already shows unavailable reasons that way (follow its pattern).
2. **Confirmation first** in the existing confirm dialog: what will happen (the app restarts on the previous version, the store returns to its snapshot from before the update, settings or data changed since then are lost; use the §12.2 facts, don't invent). Only an explicit confirm sends `{confirm: true}`; Cancel sends nothing.
3. **Answers:** `202` → the screen shows the restart/installing state it already uses for a switch (the SSE/status path). Map `409 rollback_unavailable` reasons and `409 updates_unavailable` to plain messages in the existing error area, reusing labels where they exist. No retries.
4. **Plain words for `install_failed`** (#318 follow-up): replace the raw stable code in the failed-install message with a short plain sentence per known code (from the B-update-5 decisions file and the SSE `error` codes), falling back to a generic sentence for unknown codes. Never show URLs or hosts.
5. Messages in en **and zh** (the zh completeness test requires both).
6. **Tests:** vitest for: hidden when unavailable; Cancel sends nothing; Confirm sends `{confirm: true}`; each refusal shows its message; the install_failed mapping (a known code and an unknown one). Extend the updates keyboard e2e spec to reach Roll back if it's simple; condition waits only.
7. **Sizes:** Updates entries stay under their limits; never raise one. Report deltas.
8. **Decisions file:** `docs/decisions/2026-11-01-S-updates-4.md`.

## Own
`web/src/lib/features/settings/updates/**` (incl. messages), the updates e2e spec, your decisions file.
**Don't edit** `crates/**`, `web/src/lib/api/**`, other features, or `vite.config.*`. Don't run formatters over files you didn't change.

## Size
≲ 350 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit`, and `npx playwright test` for settings*/updates* and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
