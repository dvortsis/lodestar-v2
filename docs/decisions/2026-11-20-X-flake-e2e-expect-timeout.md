# X-flake-e2e-expect-timeout decisions

Test-only. No product code changes. Prompted by `web/e2e/shell.routes.test.ts:439` ("401 hand-off › a signed-out visit goes to /login…"), which failed on #992. The page was still on `/alerts?x=1` when `toHaveURL` gave up.

- **Playwright's `expect` default is 10 s** (`web/playwright.config.ts`, `expect: { timeout: 10_000 }`; Playwright's own default is 5 s). This matches CLAUDE.md's rule that a condition wait has a deadline of at least 10 s, so the slowest CI runner (Windows) still passes. No project overrides it. Assertions that pass do so as soon as their condition holds, so a passing run takes no longer.
- **`engage()` waits for a condition, not a fixed time** (`web/e2e/support/events-server.ts`). It used to send 40 nudges 100 ms apart (`waitForTimeout(100)`) and return. If the shell hydrated after those 4 s, every nudge was missed. The chrome and stream then waited for the shell's 30 s fallback, which is longer than any assertion waits.
  - It now waits for `#boot` to go, then nudges once. The shell removes `#boot` in the same mount that attaches its first-interaction listener (as `support/interact.ts` already relies on), so that nudge is always taken.
  - The caller's next assertion, at the new 10 s default, is the condition that the chrome, the stream or the 401 hand-off got there.
  - It doesn't wait for the stream itself: in the 401 tests the page may reach `/login` or `/setup` another way first, with no stream. The `/alerts` 401 from `listSavedSearches`, or the search page's own `GET /auth` first-run check, races the stream. Either path passes the test.
- **The likely cause of the flake** was that fixed window running out on a loaded runner, plus the 5 s assertion. That's an inference from the code: the failed run's trace shows only that the page stayed on `/alerts?x=1`.
- **`liveEvents()`' own nudges stop on a condition:** when the stream is asked for, or after 30 s (the shell's own fallback). Before, they stopped after 60 nudges (6 s), which had the same slow-runner gap.
- **One fixed window stays, on purpose:** `engageBare(page)`, for the test that the sign-in, setup and welcome screens open no stream.
  - It engages, then waits until the chrome has mounted and taken the interaction: its toaster region, which the bare chrome loads too. Then it keeps nudging for 2 s and the test checks that no stream was requested.
  - An absence has no condition to wait for, so the last window is a fixed time. It now starts only once the chrome is there to start a stream, so it can't pass before the chrome has loaded.
- **Per-assertion timeouts under 10 s are left alone where they are inner retries.**
  - The 800 ms and 1 s timeouts in `search-query.keyboard`, `shortcuts.routes`, `restart.routes`, `search-live.routes` and `support/events-server` (`openPaletteWithKey`) each sit inside an `expect(...).toPass({ timeout: ≥ 10_000 })`. The short wait is how long one attempt waits before the loop presses the key or advances the clock again. The outer `toPass` is the condition's deadline, and it is already 10–15 s.
  - Raising the inner waits would only slow the retries.
- **Other timers in `web/e2e` are not fixed waits:**
  - `fixture-session.ts` polls `/healthz` every 500 ms up to a 60 s deadline;
  - `logs.keyboard.test.ts` reads the scroll position twice, 100 ms apart, inside an `expect.poll`, to see that it has settled;
  - `filters-flows.routes.test.ts` caps its wait for animations at 10 s;
  - `detail.fixture.test.ts` gives up on an answer after 10 s;
  - `ui-lists.keyboard.test.ts` waits two animation frames to time a key press to the next paint.
