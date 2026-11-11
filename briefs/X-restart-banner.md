# X-restart-banner (Sonnet, web): the restart banner, Restart now and the restart flow (§10.10.6)

Runner brief, 2026-11-11 (UTC). Found by PK-guide-3 (#708): §10.10.6's banner and Restart now aren't in `web/src`, though the backend is built. A **G-m6 dependency.** Use the steward skill; Auto mode.

## Read first
- `docs/architecture.md` §10.10.6 (about line 740), §10.3 (status pill), §10.10 (Settings).
- The backend, which is done and is the source of truth: `crates/api/src/system.rs` (`GET /system`, `POST /system/restart`, `POST /system/address/keep`, `.../revert`), `crates/api/src/dto/system.rs` (`RestartInfo`, `restart_required`), `crates/lodestar/src/system/restart.rs`. The generated client: `web/src/lib/api/schema.d.ts`.
- What exists in the web already: `web/src/lib/features/settings/overview/logic.ts` (reads `restart_required`), `OverviewScreen.svelte`, the shell and status pill, `web/src/lib/features/README.md`. Reuse, don't duplicate.
- `docs/design-tokens.md`, `docs/ui-glossary.md`, CLAUDE.md UI rules (tokens only, no hex, the §10.2 spacing ramp).

## Do
1. **Banner:** when `restart_required` is non-empty, every Settings screen and the Overview show "N changes apply after a restart · Review · **Restart now**". The status pill gets a "Restart needed" state. **Review** lists each field with its running and saved values (from the API; if the API lacks the running value, tell the runner before inventing one).
2. **Restart now** only when `GET /system` says the server can restart itself; otherwise the banner shows the server's hint ("Restart the container") and no button.
3. **The flow:**
   - a confirmation dialog warns that crawling pauses for about N seconds (the estimate the API gives);
   - a `409 restart_unavailable` or `restart_failed` shows its message and stops;
   - on `202`, a full-page "Restarting…" state polls `/healthz` with backoff, reloads, and checks that `restart_required` is empty;
   - not back within 2 minutes: say what to check (container logs, `lodestar healthcheck`).
4. **HTTP address lockout protection:** if a pending change is `network.http_bind`, the dialog shows the exact new URL. After the restart the UI goes there and asks **"Keep this address?"** (`POST /system/address/keep`); "Go back" uses revert. Show the 5-minute countdown the backend enforces. If the backend's flow differs from §10.10.6, follow the code and note it in your decisions file.
5. **Accessibility:** banner announced politely once; dialog focus trap and Escape; the Restarting page has a live status line; keyboard-only path works.
6. **Tests:** vitest for the logic (states: none, pending with/without self-restart, 409s, healthz timeout, address keep/revert). One Playwright spec with a mocked API for the happy path and the 2-minute timeout (use the test clock; no fixed sleeps; condition waits ≥ 10 s deadline).
7. **Budgets:** `npx size-limit` must pass. If the restart page needs to be lazy-loaded to fit, do that.
8. Decisions file `docs/decisions/2026-11-11-X-restart-banner.md`.

## Owns
- New `web/src/lib/features/restart/**` (component, logic, messages, tests).
- The one-line mounts of the banner in the Settings shell and `web/src/lib/features/settings/overview/OverviewScreen.svelte`, and the status pill's new state.
- `web/e2e/restart.spec.ts`, the decisions file.
- Anything else in `web/` (shared `ui/`, other features' `en.json`), or any Rust: ask the runner first.

## Bar
- runner/standards.md. In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test e2e/restart.spec.ts` (PW_CHROMIUM_PATH=/opt/pw-browsers/chromium; never `playwright install`).
- About 600–1,000 lines. Branch `slice/X-restart-banner`, PR `X-restart-banner: <title>`.
- Self-review before review; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok, web-ok and e2e-ok are green.
