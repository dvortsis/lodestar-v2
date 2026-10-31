# S-logs-1: the live log page (`/logs`)
**Model:** Sonnet: a UI page per §10.10.12 on existing APIs (B-log2). Runner brief, 2026-10-31. S-logs is split: **S-logs-1** is the live log page; **S-logs-2** is Settings → Logging, after this and M-security-settings merge. The fresh reviewer runs a security pass, because log text is untrusted content rendered in the UI.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md` (`live_log`, `full_log`, `log_event`, `log_data_kind`).
- **`docs/architecture.md` §10.10.12, "The live log (`/logs`)"** (the spec for this slice), §10.1 (budgets; batching per animation frame), §10.15 (layout and breakpoints), §10.10.11 (accessibility) and §12.1 (logging privacy).
- `docs/decisions/2026-10-23-B-log2.md` and `-B-log2-files.md` (the `/logs`, `/logs/stream`, `/logs/catalog`, `/logs/files` APIs, cursors and server-side filters), and the API shapes in `web/src/lib/api/schema.d.ts`.
- How other routes lazy-load and stream (`features/search` live code, `lib/state/events.svelte.ts`), and how TanStack Virtual is used elsewhere in the repo, if it is; if not, say what you chose.

## Do
1. **Route `web/src/routes/logs/**` and feature `web/src/lib/features/logs/**`,** full width (not the 720 px column).
2. **Opening:** show the ring buffer's recent lines at once, then follow new ones over `GET /logs/stream`, **not** the `/events` hub, batched per animation frame. The toolbar's filters go with the stream request, so the server filters.
3. **The list:**
   - virtualized, with fixed-height one-line rows in Atkinson Hyperlegible Mono;
   - each row shows time, level (icon + text), component and message;
   - a click or `Enter` expands the row's fields;
   - the client keeps at most 10,000 lines and drops older ones, with a note.
4. **Following:**
   - following is on at the bottom; scrolling up pauses it and shows "N new lines · Jump to latest";
   - **Pause** (`Space`) freezes the view while lines keep arriving;
   - the stream is suspended while the tab is hidden and catches up from the cursor on return.
5. **Toolbar:**
   - minimum level;
   - component chips;
   - text filter (`/`), sent to the server;
   - local time or UTC;
   - wrap lines;
   - **Load earlier** (paging back through `GET /logs`);
   - **Copy visible** and **Download visible**;
   - **Open full log**, linking to `/settings/logging` (a plain link; the screen arrives in S-logs-2).
6. **Untrusted text:** every field renders as text only, never as markup. Long or binary-looking messages are truncated safely, and control characters are made visible or stripped. Show the identifying-data banner (time remaining, "Turn off now") only if an existing API exposes that state; otherwise leave a slot and say so.
7. **States:** loading, empty, stream error with retry (backoff, no tight loop), the 10,000-line cap note, and offline.
8. **Sizes:** `/logs` is a new route; add its entries at measured + 10%. The virtualizer and stream code load with the route only, and no other route's first load grows. Never raise an existing limit.
9. **i18n:** `logs_*` keys in en and zh in the feature's messages, regenerated with the repo's generator.
10. **Tests:**
    - vitest: frame batching, the 10,000 cap, pause and follow, the hidden-tab suspend/resume, filters sent to the stream, and text-only rendering (a `<script>` in a message shows literally);
    - Playwright: the page renders against the mock with streaming lines, keyboard-only (`/`, `Space`, `Enter`), with axe; mock `/logs/stream` in `e2e/mock` if needed;
    - condition waits only, no fixed sleeps;
    - no existing baselines change.
11. **Decisions file:** `docs/decisions/<date>-S-logs-1.md`.

## Own
`web/src/routes/logs/**`, `web/src/lib/features/logs/**`, its message keys and regenerated maps, the `/logs*` mock fixtures in `web/e2e/mock`, `web/e2e/logs.*`, its size-limit entries and your decisions file. **Don't edit** `lib/shell/**` (the overflow-menu link comes later), `lib/state/**`, `lib/ui/**`, `lib/api/**` or any Settings code: M-security-settings is running there.

## Size
≲ 1,200 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the new spec 3× plus the core specs. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
