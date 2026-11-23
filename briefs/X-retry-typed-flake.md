# X-retry-typed-flake (runner brief, 2026-11-23Z)

Web slice, Opus (a flake that may be a product race). Fresh Opus review before merge. Placed by `docs/decisions/2026-11-23-runner-records-r14.md`; its slices.yaml entry is `X-retry-typed-flake`.

## Why
`web/e2e/ui-checks/chunk-load.routes.test.ts:291` ("Retry pressed right after typing keeps the typed query") fails about 1 run in 3 on main: 10 of 30 without #1085's key buffer, 5 of 30 with it (X-skip-results-flake measured it at repeat 30). Every failure has the same signature: the URL ends `q=lanternharbor`, so select-all lost its selection and the typed text was appended to the old query. The key buffer (#1085) never holds typing keys, so it isn't the cause. Unverified lead: the search box's content or selection is reset when the chrome (or the search page's lazy chunks) arrives. If that's real, users lose a select-all on a slow load too, so it's a product bug first.

## Goal
1. Reproduce on main (`--repeat-each=30 --retries=0`), and find the mechanism with evidence (an event log, a trace, or a probe). Don't guess.
2. If the product resets the input's value or selection after mount (a re-render, a `value` rebind, a focus or `select()` call, hydration of a lazy chunk), fix it in the product: the search box keeps what the user typed and selected, whatever loads later. If the cause is only in the test, fix the test to wait for a real condition (deadline of at least 10 s), never a sleep, and say why the product is fine.
3. Prove it: repeat 30 with no retries passes 30/30 on the fixed head, and fails at the old rate with the fix reverted. Add a focused test (vitest or e2e) that fails without the fix.

## Step 0 (send me, then wait for "go")
1. The mechanism, with the evidence.
2. The fix and its file:line. A product fix will need the search box's file (name it); the runner checks it against other running slices' owns.
3. Owns, to refine: `web/e2e/ui-checks/chunk-load.routes.test.ts`, `docs/decisions/2026-11-23-X-retry-typed-flake.md`, plus the product file(s).
4. Size: the Search route is at 44.98 / 45 kB and takes no raise. Say what the fix costs.

## Rules
- Never skip, quarantine or add retries to a test. No fixed sleeps.
- `.size-limit.mjs` is held by X-size-own-modules (API lane); if you think you need it, ask.
- en and zh messages if any copy changes (unlikely).

## Branch and PR
- Branch `slice/X-retry-typed-flake`. PR title "X-retry-typed-flake: Retry right after typing keeps the typed query".
- Gates per CLAUDE.md for `web/`. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
