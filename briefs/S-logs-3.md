# S-logs-3: Logging event labels, console format labels, catalog refresh
**Model:** Sonnet (message keys plus small UI wiring). An Opus reviewer follows. Runner brief, 2026-10-31. Follow-ups from #289 and #294.

## Why
- The Logging event list shows stable event ids such as `pipeline.stored` where §10.10.12 asks for a plain name and one line of help. #289 left the lookup hook in `web/src/lib/features/settings/logging/event-labels.ts`.
- The console format options show the bare values `text`/`json`. #294 dropped their labels because they didn't fit the 0.86 KB Logging label map.
- A kind turned on in another tab only appears here after a change or a deadline. The catalog isn't refreshed (#289 review, O2).

## Read first
- CLAUDE.md UI rules.
- `docs/ui-glossary.md`. Labels use plain words, with the technical term where it helps.
- `docs/architecture.md` §10.10.12 and §12.1.
- `docs/decisions/2026-10-31-S-logs-2.md` and `-S-logs-2b.md`, which cover the label maps, the `logoutput` lazy map and the sizes.
- The event catalogue the server serves: `GET /logs/catalog`, and its source in `crates/` (`logs_catalog.txt` or the equivalent golden list). The labels must cover every catalogued event.

## Do
1. **Event labels.**
   - Add a plain name and a one-line help for every catalogued event, in en and zh, through the existing hook.
   - The labels must not land in Logging's first view. Load them with the event list's lazy part, or as their own lazy map loaded with it, at measured + 10%.
   - Unknown or new events fall back to the id.
   - Add a test that fails if a catalogued event has no label. Read the catalogue list the server uses, so a new server event breaks the test until it's labelled.
2. **Console format labels.** Restore "Plain text (text)" and "JSON lines (json)", or similar plain words with the term.
   - They go in the Logging label map, because the engine looks options up by section.
   - That map may go from 0.86 KB to at most 0.95 KB. This is a runner decision under §2.26; put a comment on the line and record it in your decisions file.
   - Logging's first view keeps ≥ 0.3 KB headroom.
3. **Catalog refresh.**
   - Reload `GET /logs/catalog` when the page becomes visible again (`visibilitychange`) and on the existing 30 s tick, if one is running.
   - Never stack requests. A failed reload keeps the last known state; the banners still follow #289's rules.
4. **Sizes.**
   - No other first view grows.
   - Alert, Search results and Results toolbar must not grow at all.
   - New entries go in at measured + 10%. Never raise an existing limit except the one in item 2.
5. **Tests.**
   - vitest covering label coverage, the fallback and the refresh (visibility, no stacking, a failure keeps the state).
   - The settings-logging Playwright spec still passes.
   - Condition waits only.
6. **Decisions file:** `docs/decisions/<date>-S-logs-3.md`.

## Own
- `web/src/lib/features/settings/logging/**`
- its message keys and regenerated maps
- its size-limit entries
- `web/e2e/settings-logging.*`
- your decisions file

**Don't edit** the engine (`form/**`), the registry or other screens. S-about is running in `settings/about/**`.

## Size
≲ 900 changed lines. Most of it will be message keys.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/settings-logging* e2e/logs*`

Chromium is at /opt/pw-browsers; never run `playwright install`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
