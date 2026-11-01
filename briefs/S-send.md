# S-send: "Send to client" in the result row, the detail drawer and /t, plus the keyboard flow
**Model:** Sonnet (UI wiring to an existing API in existing patterns). The runner reviews. Runner brief, 2026-11-01. It closes the S-e2e (#314) app gap: §10.17's keyboard flow "search → open → send" stops at copy because the UI has no Send. M5 exit gate.

## Why
- The API has `POST /clients/send` (`sendToClient` in `web/src/lib/api/schema.d.ts`). It sends hashes to the default saved download client (or a named one), refuses hidden ones, and returns a per-hash result. Errors: `no_client` (404), `invalid_hashes`, `cross_origin`.
- §10.5: the drawer header's **one primary action** is "Send to qBittorrent" (the client's name) **if a client is configured**, otherwise "Copy magnet". §10 labels: "Send to qBittorrent". Shortcut table (§10, around l.1140): `s` = Send to client.

## Read first
- `docs/architecture.md` §10.4 (results, row actions), §10.5 (drawer header), the shortcut table (search "Send to client"), §10.12/§10.13 if they cover clients, and the UX-writing rules (l.~1113).
- `web/src/lib/features/search/results/RowActions.svelte`, `web/src/lib/features/detail/DetailActions.svelte`, `web/src/routes/t/[hash]/**`. **#316 just landed "Block this…" in these files: read its door pattern and size guard (`.size-limit.mjs`, `docs/decisions/2026-11-01-S-filters-2c.md`) and follow it.**
- How the app knows a client is configured: the Settings clients section / any existing client store (search `clients` under `web/src/lib/features/settings/`). Reuse it; don't add a new fetch on first paint if one already exists. If none exists, the cheapest truthful signal (e.g. a lazily fetched flag) is fine; say what you chose.
- The shortcuts registry (`web/src/lib/features/shortcuts/`), the existing magnet copy (`detail/commands.ts`), the toast pattern for failures.
- `web/e2e/search-results.keyboard.test.ts` (the flow to extend).

## Do
1. **Drawer and /t header:** when a client is configured, the primary button is "Send to <client name>" and Copy magnet becomes secondary. With no client, nothing changes (Copy magnet stays primary). Don't show Send for a torrent the user can't see in full (the API refuses hidden ones anyway).
2. **Result row menu:** add "Send to <client name>" when a client is configured.
3. **`s` shortcut:** sends the focused row (results) or the open torrent (drawer), only when a client is configured; otherwise it does nothing visible beyond what the shortcuts help says.
4. **Feedback:** on success a polite toast ("Sent to qBittorrent"). On a per-hash refusal or an error (`no_client`, network), an error toast with plain words. No retries.
5. **Load the send code on demand** (dynamic import through one module), so first views grow only by the button and a tiny import. **Sizes:** Search results, drawer and /t at most +0.3 KB each, and each keeps ≥ 0.1 KB headroom. Alert, Alerts, Results toolbar, Discover and CSS must not grow. Never raise a limit; if it can't fit, stop and tell the runner with the numbers.
6. **Tests:**
   - vitest: the button only with a client; the right body (`{hashes:[hash]}`, no client id for the default); success and each error toast; `s` does nothing without a client.
   - Playwright: extend the keyboard flow in `search-results.keyboard.test.ts` (or a new keyboard spec) to **search → open → send** with the smoke mock answering `/clients/send`, with axe. Condition waits only.
   - Baselines: the drawer and /t headers change only where the mock has a client configured. If the smoke mock has no client, baselines shouldn't change; if any do, use the marker and check only the expected images changed.
7. **Decisions file:** `docs/decisions/2026-11-01-S-send.md`: how "client configured" is known, the load path, byte deltas.

## Own
The Send lines in `RowActions.svelte`, `DetailActions.svelte` and the /t route, a new `features/detail/send.ts` (or similar) module, the shortcut registration for `s`, its messages (en **and zh**), the smoke mock's `/clients/send` answer, the keyboard spec, the size entries' comments, your decisions file.
**Don't edit** `features/filters/**`, `features/settings/**` (read-only use of an existing client store is fine), `lib/shell/**`, `vite.config.*`.

## Size
≲ 500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for search-results*, detail*, keyboard-only and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
