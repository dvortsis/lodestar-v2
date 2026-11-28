# X-live-banner-flake (runner brief, 2026-11-28)

**Slice:** X-live-banner-flake (runner/slices.yaml). Opus, helper.
**Branch:** `slice/X-live-banner-flake` from current `main`.
**PR title (exact):** `X-live-banner-flake: search-live e2e: send events only once the page can receive them`
**Owns:** `web/e2e/search-live.routes.test.ts`, `web/e2e/support/events-server.ts`, `docs/decisions/*-X-live-banner-flake.md`.

## Why now
- `search-live.routes.test.ts:29` ("new matches show a banner…") has now failed twice the
  same way: banner `results-live` never appears. It failed on #1082 and on #1165. The web
  runtime was unchanged both times, and it passes locally.
- `e2e-ok` is now a required check in the ruleset (owner, 2026-11-28), so this flake blocks
  merges for every web PR. It's the top of the queue.

## What's known (slices.yaml notes)
- The test already waits for the `registerSearchLive` call (line ~38). Never add that wait again.
- What it doesn't wait for:
  - (1) the page's `/events` stream being open. `events.send` writes only to streams already
    open, so an event sent before the shell opens one is lost silently;
  - (2) the client having applied the returned id `a1`.

## Do
1. Step 0: find which race it is (read `events-server.ts` and the client's live-search and
   events code). Send the runner the cause and the proposed fix before writing it.
   - A fix in the helper is better than one in each test. For example, `events.send` could
     wait until a stream is open, or a `events.connected()` condition could be added.
   - Check the other specs that use `liveEvents` for the same race, and fix them in the
     same way if they have it (they're in this slice's spirit; tell the runner which files).
2. Condition waits with a deadline of at least 10 s. Never a fixed sleep, never skip,
   never loosen what the test proves.
3. No product (web/src) changes without asking the runner.
4. Prove it: run the spec many times locally, e.g. `--repeat-each=30`, and on the
   chromium core project. Say the counts in the PR.
5. Decisions file: `docs/decisions/<date>-X-live-banner-flake.md`.
6. Follow `.claude/skills/steward/SKILL.md`. Run the web checks from CLAUDE.md. Use
   `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`. Report
   `ready <PR> <sha>` to the runner session.
