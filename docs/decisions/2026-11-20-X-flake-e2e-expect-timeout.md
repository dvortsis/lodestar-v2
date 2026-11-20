# X-flake-e2e-expect-timeout decisions

Test-only. No product code changes; the product cause found here is queued as X-auth-early-401 (below). Prompted by `web/e2e/shell.routes.test.ts:439` ("401 hand-off › a signed-out visit goes to /login…"), which failed on #992. The page was still on `/alerts?x=1` when `toHaveURL` gave up.

- **Playwright's `expect` default is 10 s** (`web/playwright.config.ts`, `expect: { timeout: 10_000 }`; Playwright's own default is 5 s). This matches CLAUDE.md's rule that a condition wait has a deadline of at least 10 s, so the slowest CI runner (Windows) still passes. No project overrides it. Assertions that pass do so as soon as their condition holds, so a passing run takes no longer.
- **Why the 401 hand-off test flaked** (the runner's review of #995). There were two causes, and neither was only a short timeout.
  - **The test caught a moment instead of a state.** It didn't mock `getAuthStatus`, so the mock answered "signed in". The stream's 401 sent the page to `/login`, `/login` saw "signed in" and sent it back to `next`, and the next 401 sent it to `/login` again. The runner's review logged the URL bouncing every 30–50 ms for 3–6 s, and `toHaveURL` passed only when a poll landed on a `/login` phase. That fits the failed run, which read `/alerts?x=1`.
    - The test now mocks a signed-out `getAuthStatus`, as its sibling test does.
    - It also checks that the page *stays* on `/login`: after `/login` has asked who is signed in and the network has gone quiet, the URL is still `/login?next=…`.
    - Without the mock, the new check fails (3 of 3 runs, checked locally).
  - **Only the stream ever hands off, and only after an interaction.**
    - `listSavedSearches`' 401 arrives about 13 ms after load (as the runner's review measured it) and is dropped. The client has no `loginHandler` yet, because `Shell.svelte` is still loading `installAuthHandlers` by dynamic import (`client.ts`, `Shell.svelte` `onMount`).
    - So a signed-out visitor can sit on a page until the first interaction, or up to the shell's 30 s fallback.
    - That's a product bug, outside this slice's owns. The runner has queued it as **X-auth-early-401**.
- **`engage()` waits for the shell, then nudges once** (`web/e2e/support/events-server.ts`). It used to send 40 nudges 100 ms apart (`waitForTimeout(100)`) and return. If the shell mounted after those 4 s, every nudge was missed, and the stream waited for the 30 s fallback.
  - It now waits for `<main id="main">`. Only `Shell.svelte` renders it, in the same mount that attaches the first-interaction listener, so the one nudge is always taken.
  - `#boot` is no signal: on `/login`, `/setup` and `/welcome` the inline boot script removes it before the app starts.
  - The caller's next assertion, at the new 10 s default, is the condition.
- **`networkQuiet(page)`**, a new helper, waits until nothing has been in flight for 500 ms, with a 15 s deadline. It counts from when it's created, so the requests an action starts are covered. (Playwright's `networkidle` load state resolves at once if the page went idle before the action, so it wouldn't cover lazy chunks loaded after an interaction.)
- **`liveEvents()`' own nudges stop on a condition:** when the stream is asked for, or after 30 s (the shell's own fallback). Before, they stopped after 60 nudges (6 s), which had the same slow-runner gap.
- **The no-stream check has no fixed window** (`engageBare(page)`), for the test that the sign-in, setup and welcome screens open no stream.
  - It engages, waits until the chrome has mounted and taken the interaction (its toaster region, which the bare chrome loads too), then waits for `networkQuiet`. By then every chunk the interaction loads has arrived.
  - A stream started by mistake shows in `liveEvents`' requests, or keeps the network busy and fails the wait. It can't pass silently on a slow runner.
- **Per-assertion timeouts under 10 s are left alone where they are inner retries.**
  - The 800 ms and 1 s timeouts in `search-query.keyboard`, `shortcuts.routes`, `restart.routes`, `search-live.routes` and `support/events-server` (`openPaletteWithKey`) each sit inside an `expect(...).toPass({ timeout: ≥ 10_000 })`. The short wait is how long one attempt waits before the loop presses the key or advances the clock again. The outer `toPass` is the condition's deadline, and it is already 10–15 s.
  - Raising the inner waits would only slow the retries.
- **Other timers in `web/e2e` are not fixed waits:**
  - `fixture-session.ts` polls `/healthz` every 500 ms up to a 60 s deadline;
  - `logs.keyboard.test.ts` reads the scroll position twice, 100 ms apart, inside an `expect.poll`, to see that it has settled;
  - `filters-flows.routes.test.ts` caps its wait for animations at 10 s;
  - `detail.fixture.test.ts` gives up on an answer after 10 s;
  - `ui-lists.keyboard.test.ts` waits two animation frames to time a key press to the next paint.
