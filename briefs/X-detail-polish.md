# X-detail-polish: Block and Send follow-ups in the drawer, /t and the result row
**Model:** Sonnet (small UX fixes in existing patterns). The runner reviews. Runner brief, 2026-11-01. Follow-ups from #316 (S-filters-2c) and #321 (S-send).

## Items (do each that fits the size budget; skip any that doesn't and say why)
1. **A hidden torrent stays listed after Block** (#316). After "Block this…" applies a *hide* (or quarantine/drop) rule successfully, the torrent stays in the results until the next refetch. Make the list drop it (or refetch the current page) right after a successful apply, through the existing results store; no extra request when the apply is cancelled or refused. Keep it in on-demand code (the flows/door path), not first views.
2. **Silent failure when the flows door fails to load** (#316). If the dynamic import behind "Block this…" fails, show the existing error toast (the same one magnet copy/open use, see `detail/commands.ts`) instead of doing nothing.
3. **Stale Send label** (#321). `detail/send.ts` caches the client name for the session, so a renamed or removed client shows the old name until reload. Invalidate the cache when the integrations settings section is saved (use the existing settings-saved event or store if one exists; otherwise a cheap re-check when the drawer opens). Don't add a request to first paint.
4. **Stale-press guard on Send** (#321). Mirror the Block entry's guard: if the shown torrent changes between the press and the send module loading, drop the press (A → B → A included). A vitest that fails with the guard removed.
5. **`s` on /t before the torrent loads** (#321): make `s` a no-op until the torrent is loaded (the API refuses safely today, but the toast is confusing).

## Read first
- `docs/decisions/2026-11-01-S-filters-2c.md`, `docs/decisions/2026-11-01-S-send.md` (their "left as notes" sections).
- `web/src/lib/features/detail/{DetailActions.svelte,send.ts,commands.ts,target.ts}`, `web/src/lib/features/search/results/RowActions.svelte`, `web/src/lib/features/filters/flows-door.ts`, the results store, `web/.size-limit.mjs`.

## Rules
- **Safety (blocking):** Block still only adds a rule. Cancel or refusal still sends nothing, and `filters/block-entry.test.ts` stays as strict as it is (no `/filters` request and no non-GET request on Cancel or refusal). Nothing adult shows unless 18+ is on.
- **Sizes:** drawer (27,186/27,400 B), /t (20,828/21,000) and Search results (43,115/43,300) may each grow by at most **+0.1 KB** in total; Alert, Alerts, Results toolbar, Discover and CSS must not grow. Never raise a limit. Put new code on demand where you can.
- Tests per item (vitest; condition waits only). Baselines must not change; if any do, stop and tell the runner.
- Don't run formatters over files you didn't change.

## Own
`features/detail/{DetailActions.svelte,send.ts,commands.ts}` and their tests, the Block/Send lines in `search/results/RowActions.svelte`, the hide-after-apply hook (on-demand side), your decisions file `docs/decisions/2026-11-01-X-detail-polish.md`.
**Don't edit** `features/filters/flows/**` internals, `features/appearance/**`, `features/wizard/**`, `features/filters/flows-door.ts` (X-flows-door-2 is in it; if you need a new export there, tell the runner), `settings/security/**`, `vite.config.*`.

## Size
≲ 350 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for detail*, search-results*, filters*, keyboard-only and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, which items were done or skipped, and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
