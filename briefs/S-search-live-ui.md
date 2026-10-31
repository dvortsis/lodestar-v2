# S-search-live-ui: the "N new results" banner uses the live feed instead of polling
**Model:** Sonnet: a small UI change following B-search-live (#272). Runner brief, 2026-10-31. No `/security-review`; the server applies visibility to every count.

## Why
S-search-6's live banner polls `/search` with `limit: 1` every 30 s and compares exact totals. #272 added `POST /search/live` (register the shown query), `DELETE /search/live/{id}`, and `event: search_live {id, matched, through}` on `/events`, throttled per connection. The banner should use them: exact, cheaper, and no polling.

## Read first
- **`docs/decisions/2026-10-31-B-search-live.md`** (the API, the bounds, the 30 s grace, the 60 s connectionless idle, the 4-per-session cap, and "what the banner should switch to"), plus `docs/decisions/2026-10-31-S-search-6.md` (how the banner works today).
- `web/src/lib/features/search/live/**`, `results/**`, the SSE client (`lib/state/**` events store, read only; find how other features subscribe to topics), and `web/src/lib/api/schema.d.ts` (`SearchLiveRequest`, `SearchLiveRegistration`, `SearchLiveEvent`).
- CLAUDE.md's UI rules, `runner/standards.md`, §10.4 Live row and §10.13 (announcement throttling).

## Do
1. When the shown query changes, register it with `POST /search/live` (the same AST and `hidden` as the search) and release the previous id with `DELETE`. Subscribe to the `search_live` topic on the existing `/events` connection. Don't open a new EventSource if one exists.
2. Show "N new results · Show" from `matched` for the current id only; ignore events for other ids. Keep today's rules: Show reloads from the top; nothing reorders under the user; the banner pauses while scrolled deep or a selection is held; one throttled polite announcement.
3. Remove the 30 s polling path. Keep a fallback only if registration fails (429 cap, offline): fall back to today's polling and say so in the decisions file.
4. Release on navigation away and on page hide where the API allows it. The server's 30 s grace and 60 s idle cover the rest.
5. **Sizes:** /search first load must not grow; the live chunk stays on demand. Update the live banner's size entry only if the chunk changes, at measured + 10%, without raising any other limit. CI Lighthouse total-byte-weight on /search must stay under 102,400.
6. **Tests:**
   - vitest: register on query change, release the old id, count from `matched` for the current id only, ignore other ids, and fall back on 429;
   - Playwright: with the mock emitting `search_live`, the banner appears with the right N, Show reloads, keyboard and axe;
   - condition waits only, no fixed sleeps;
   - no baseline changes expected.
7. **Decisions file:** `docs/decisions/<date>-S-search-live-ui.md`.

## Own
`web/src/lib/features/search/live/**`, minimal edits in `search/results/**` to wire it, the search mock's live-feed fixtures, `web/e2e/search-live.*`, the live banner's size entry, and the decisions file. **Don't edit** `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`. If the events store lacks a way to subscribe to `search_live`, stop and tell the runner.

## Size
≲ 500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the search specs, 3× for the live spec. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked local config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head SHA and size deltas as soon as it opens. Never merge. Work frugally. Push once per round of fixes.
