# X-flakes-1107: two e2e timing flakes, fixed once
**Model:** Sonnet (test-only web e2e work; a product fix only if the race is real in UI code). A fresh Opus reviewer follows. Runner brief, 2026-11-07T18:35Z.

## Why
Each test failed under CI load on 2026-11-07 in a PR that touches no `web/` code, and passed on re-run. Per CLAUDE.md, a flake gets fixed once, not re-run onto the next PR. No running slice owns either test.

1. **`web/e2e/shell.routes.test.ts:368`**, "a server that never answers shows the unreachable page, and Try again reloads".
   - Timed out clicking **Try again** (line ~387) on #582 (run 42386800085, job 128445845679).
   - Likely race: the test unroutes the API and restores the event stream (`liveEvents`) *before* the click. The app's own probe can recover first and remove the button, so the click waits on a button that's gone.
2. **`web/e2e/filters-block.keyboard.test.ts:70`**, "from a result: Shift+F10, Block this…, preview, Apply adds one rule".
   - After `End`, the "Block this…" menu item didn't become focused within 10 s (#583 run 42392077532).
   - Find out whether `End` is pressed before the menu is ready for keys (open animation, focus not yet inside the menu), or whether the menu really drops the key. A real dropped key is a product bug: fix it in the menu component.

## Do (branch `slice/X-flakes-1107` from `origin/main`)
For each test:
1. Name the race from the test and the code it drives.
2. Make the test assert the same property deterministically. For test 1, one option: keep the API dead until the button has been clicked (or route the recovery so the app can only recover through the click), then assert that Search returns. The test must still prove that Try again reloads.
   - Wait on conditions (`expect(...).toBe...`, `expect.poll`) with timeouts of at least 10 s.
   - No fixed sleeps or `waitForTimeout`. Never skip, retry, `test.fixme`, or loosen what the test proves.
3. Prove it. Run each spec repeatedly: `npx playwright test <spec> -g "<title>" --repeat-each=50 --workers=4 --trace off`, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (see `web/e2e/README.md`; never `playwright install`). Also run it under CPU load (for example `stress-ng --cpu 4` or a busy loop alongside, if available). Report the counts and what you ran.
4. Write `docs/decisions/2026-11-07-X-flakes-1107.md` (short): each cause and fix.

## Own
- the two test files, and any e2e helper they use (`web/e2e/**` helpers only as far as these tests need);
- the menu component, only if test 2's race is a real product bug: the smallest fix, justified in the decisions file;
- the decisions file.

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, and the two specs pass, plus the full e2e project those specs belong to. Open the PR against `main` titled "X-flakes-1107: fix the shell unreachable and filters-block keyboard e2e flakes". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the repeat counts, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
