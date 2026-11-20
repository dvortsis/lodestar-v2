# X-flake-e2e-expect-timeout decisions

Test-only. No product code changes. Prompted by `web/e2e/shell.routes.test.ts:439` ("401 hand-off › a signed-out visit goes to /login…"), which failed on #992. The page was still on `/alerts?x=1` when `toHaveURL` gave up.

- **Playwright's `expect` default is 10 s** (`web/playwright.config.ts`, `expect: { timeout: 10_000 }`; Playwright's own default is 5 s). This matches CLAUDE.md's rule that a condition wait has a deadline of at least 10 s, so the slowest CI runner (Windows) still passes. No project overrides it. Assertions that pass do so as soon as their condition holds, so a passing run takes no longer.
- **`engage()` waits for a condition, not a fixed time** (`web/e2e/support/events-server.ts`). It used to send 40 nudges 100 ms apart (`waitForTimeout(100)`) and return. If the shell hydrated after those 4 s, every nudge was missed, and the stream (the 401 path the failing test watches) only started at the shell's 30 s fallback. That is longer than any assertion waits.
  - It now nudges every 100 ms until the stream has been asked for: a request for `/api/v1/events`, or one `liveEvents()` answered. The deadline is 15 s, and it fails with a message naming what never happened.
  - A nudge that lands as the page navigates away (the 401 hand-off) is ignored.
  - This is the likely cause of the flake: on a loaded runner, the 4 s nudge window plus the 5 s assertion wasn't enough.
- **One fixed window stays, on purpose:** `nudgeFor(page, 4_000)`. The test that the sign-in, setup and welcome screens open no stream checks for an absence, so there's no condition to wait for. It nudges for a fixed 4 s and then checks that no stream was requested, as before. Nothing waits on it to pass.
- **Per-assertion timeouts under 10 s are left alone where they are inner retries.**
  - The 800 ms and 1 s timeouts in `search-query.keyboard`, `shortcuts.routes`, `restart.routes`, `search-live.routes` and `support/events-server` (`openPaletteWithKey`) each sit inside an `expect(...).toPass({ timeout: ≥ 10_000 })`. The short wait is how long one attempt waits before the loop presses the key or advances the clock again. The outer `toPass` is the condition's deadline, and it is already 10–15 s.
  - Raising the inner waits would only slow the retries.
- **Other timers in `web/e2e` are not fixed waits:**
  - `fixture-session.ts` polls `/healthz` every 500 ms up to a 60 s deadline;
  - `logs.keyboard.test.ts` reads the scroll position twice, 100 ms apart, inside an `expect.poll`, to see that it has settled;
  - `filters-flows.routes.test.ts` caps its wait for animations at 10 s.
