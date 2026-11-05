# X-flake-search-keyboard: fix the focus race in search-results.keyboard e2e
**Model:** Sonnet (an e2e test fix that follows an existing pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-05T06:55Z.

## Why
`web/e2e/search-results.keyboard.test.ts` fails at random with a focus race: a keystroke meant for the results list reaches the search box. It has failed twice:
- on #469 (RP-UC), 2026-11-05, at `:19`. The query came out as `res:2160prn` instead of `res:2160p`, so the `n`/`j` meant for the list landed in the box;
- earlier on #306, at `:70`.

It passes locally. Per CLAUDE.md, a flake gets fixed once.

## Do (branch `slice/X-flake-search-keyboard` from `origin/main`)
1. **Find the race.** The test types the query, presses Enter, then Tab, `j` or `k`. Typing likely starts before the box's value is settled, or the list key is pressed before focus has moved to the list. Read the test and the search box and results-list focus code (`web/src/lib/features/search/**`) to confirm.
2. **Fix the test by waiting on conditions, never on time.**
   - Before pressing a list key, wait until the list (or the row) has focus: `await expect(locator).toBeFocused()`.
   - After typing, wait until the box holds exactly the typed value: `toHaveValue`.
   - Apply this to each step in the spec where focus moves (`:19`, `:52`, `:71`, `:84`), and to the shared `kbd` helper if it's the right place.
   - No fixed sleeps. Never skip, retry or quarantine.
3. **If the race is real in the product** (a key handled while focus is moving, which a user could hit too), say so in the PR and propose the smallest product fix. Make that fix only if it's confined to the results list's key handling.
4. **Prove it.**
   - Run `npx playwright test e2e/search-results.keyboard.test.ts --repeat-each=40` with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome` and report the counts.
   - Then run it again under CPU load (for example, a parallel `--workers=4` run of the whole keyboard project) and report those counts too.

## Own
`web/e2e/search-results.keyboard.test.ts`, the shared keyboard helper under `web/e2e/support/` if you change it (list every spec that uses it), and the product fix from step 3 if one is needed.

## Done when
In `web/`, `npm run check`, the e2e typecheck and the repeated runs all pass. Open the PR against `main` titled "X-flake-search-keyboard: wait for focus in search-results keyboard e2e". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
