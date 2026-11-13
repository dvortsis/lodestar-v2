# X-offline-retry: a page opened while the server is down says "Couldn't reach Lodestar" and retries

Runner, 2026-11-13. Track U step 4 (UI review L1). Model: Sonnet. Branch `slice/X-offline-retry`. PR title `X-offline-retry: Couldn't reach Lodestar, and retry until the server answers`.

## The problem
Open a page while the Lodestar server is down, and it stays on "Error 500" even after the server comes back. The user has to reload by hand and isn't told what happened.

## What to build
- When the first load fails because the server can't be reached (a network error or no answer, not an HTTP error the server actually sent), show the connection view: **"Couldn't reach Lodestar"** with one plain line on what to check, instead of Error 500.
- Retry on its own with backoff (for example 1 s, 2 s, 4 s … capped at 30 s), plus a **Try again** button. When the server answers, load the page the user asked for, at the same URL, with no full reload loop.
- A real server error (a 5xx with a body) still shows the error view as today.
- Messages in en and zh, in `web/src/lib/features/shell/messages/**`.
- Respect `prefers-reduced-motion` for any spinner. Use an `aria-live="polite"` status for the retry text. Use tokens only (no hex), and spacing from the §10.2 ramp.

## Owns
`web/src/routes/+error.svelte`, `web/src/lib/shell/ErrorView.svelte`, `web/src/lib/shell/ConnectionView.svelte`, `web/src/lib/features/shell/messages/**`, `web/e2e/boot-shell.routes.test.ts`. Also add `docs/decisions/2026-11-13-X-offline-retry.md` if you make a call the spec leaves open. Ask the runner before touching anything else.

## Spec
`docs/architecture.md` §10.3 and §10.14; owner UI decisions `docs/owner-decisions.md` §2.66; `docs/ui-glossary.md` for wording; `runner/standards.md`.

## Tests
- vitest: offline is told apart from a server error; backoff schedule and cap; Try again; recovery loads the requested route.
- Playwright in `boot-shell.routes.test.ts`: the server is unreachable, then answers, and the page loads by itself. Wait on a condition (`expect.poll` / `toBeVisible` with a timeout ≥ 10 s), never a fixed sleep (owner §2.24). Use fake timers or route mocking for the backoff, so the test doesn't wait 30 s.
- Before pushing, from `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test e2e/boot-shell.routes.test.ts` with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`. Never run `playwright install`.

## Process
Follow `.claude/skills/steward/SKILL.md`. One push per round of fixes. When CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `ready <PR> <full 40-char sha>`. The runner reviews and merges; you never merge. No cloud or AWS calls.
