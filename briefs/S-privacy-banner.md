# S-privacy-banner: say when identifying data is being recorded
**Model:** Opus (privacy invariant across the API and the shell; small web part). `/security-review` runs on the PR. Runner brief, 2026-11-01. It completes the #277 review item that X-web-polish-1101c (#331) couldn't build.

## Why
§10.10.7 ("Privacy" row) and §10.10.12 promise a banner while a log data kind that identifies torrents or people is on, and while query logging (`insights.query_logging`) is on. Today:
- Settings → Logging (`features/settings/logging/Banners.svelte`) and `/logs` already show the identifying-kinds banner.
- **The Overview doesn't,** and nothing shows that query logging is on outside Settings → Insights.
- #331 found that nothing the app loads on first paint carries this state; the only source is `GET /logs/catalog`.

Runner decision (§2.26): carry it on a response or event the app **already** loads, never a new first-paint request. The banner code loads on demand only when the flag is set.

## Read first
- `docs/architecture.md` §10.10.7 (Privacy row), §10.10.12 (the banner: time remaining, **Turn off now**), §12.1 (data kinds; which identify torrents or people), and §10.1 (budgets).
- `docs/decisions/2026-11-01-X-web-polish-1101c.md` (what #331 checked).
- `crates/lodestar/src/settings/logging.rs` (`identifying()`, the `until` encoding: 0 off, -1 until turned off), `crates/lodestar/src/settings/insights.rs` (`query_logging`).
- `crates/api/src/dto/events.rs` (the SSE `Event` enum) and whatever the shell and the Overview fetch on first paint (find it; don't guess).
- `web/src/lib/features/settings/logging/{Banners.svelte,privacy.ts,state.ts}` and `web/src/lib/features/logs/status.ts`: reuse their wording and time-remaining logic; don't fork it.
- `web/.size-limit.mjs` and `web/scripts/first-load.mjs`.

## Do
1. **Server: one small privacy summary** on the cheapest carrier the shell or Overview already loads (a status or preferences response, or an existing SSE event that is re-sent when settings change; you pick and say why in the decisions file).
   - Shape, e.g. `privacy: { query_logging: bool, identifying_until: i64 | null }` (`null` off, `-1` until turned off, else a Unix time). Booleans and a time only: **never the kinds' values, a path, a query or a name.**
   - It updates without a reload when either setting changes or the time runs out (the existing settings or logging events, or the expiry switch's required event).
   - It follows the endpoint's existing auth: a client that can't read settings must not learn more than it could before. Say in the PR what each auth state sees.
   - Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling; re-bless API goldens.
2. **Web: the banner on the Overview** (and on every surface §10.10.12 names that lacks it), loaded on demand only when the summary says something is on.
   - It names what is recorded in plain words ("Search queries are being saved", "Torrent names and hashes are written to the log for 52 more minutes"), with **Turn off now** for the log kinds (same action as Settings → Logging) and a link to Settings → Insights for query logging.
   - A working-surface notice: no illustration, not dismissible except by turning it off, `role="status"`, keyboard-reachable. en **and** zh.
   - First views grow by at most **+0.05 KB** each (only the flag check stays eager). Add a size entry for the banner chunk at measured + 10%. Never raise a limit; don't touch `vite.config.*`.
3. **Safety rules (blocking):**
   - The summary never exposes more than booleans and the expiry time.
   - "Turn off now" only turns things off; nothing on this path turns logging on or extends it.
   - With the flag off, no banner code loads (assert with the chunk graph, `modulesIn`).
4. **Tests:**
   - Rust: the summary for off / on-for-an-hour / until-off / expired, and that it changes with the setting; auth states.
   - vitest: the banner shows, counts down, hides at expiry, and Turn off now sends only the off change; each fails with its guard removed. Condition waits only.
   - Playwright: the Overview with logging on (mock), keyboard reach, axe. Baselines must not change for the off state.
5. **Decisions file:** `docs/decisions/<date>-S-privacy-banner.md`: the carrier and why, the shape, auth behaviour, and sizes before → after.

## Own
- The chosen carrier's DTO and handler in `crates/api` (only that field), the summary builder in `crates/lodestar/src/settings/` (new fn), the regenerated API files and goldens.
- A new `web/src/lib/features/shell/privacy/**` (or beside the Overview), the hook line where it mounts, its messages, size entry, specs, decisions file.

**Don't edit** `features/settings/logging/**` internals beyond exporting a helper you reuse. Detail drawer and /t are at their limits: don't import into them.

## Size
≲ 600 changed lines (goldens excluded).

## Done when
- Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit` (twice, identical), `npx playwright test` for the new spec, insights*, logs*, settings-logging* and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the carrier chosen and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
