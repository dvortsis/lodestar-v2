# X-detail-actions-timeout (runner brief, 2026-11-28)

**Slice:** X-detail-actions-timeout. Opus (flake root cause). Same session as X-all-themes-settle.
**Branch:** `slice/X-detail-actions-timeout` from current `main`.
**PR title (exact):** `X-detail-actions-timeout: The detail actions menu test has room for the waits it makes`
**Owns:** `web/src/lib/features/detail/DetailActions.test.ts`, and the vitest config only if step 0
shows the cause is there. Plus `docs/decisions/*-X-detail-actions-timeout.md`.

## Source
The fresh review of #1178 saw `DetailActions.test.ts:79` ("puts Open full page in the menu for
the drawer only, and Block in both") time out at 5 s in a full `vitest run src/lib/features/detail`,
then pass when run alone. The test makes two `findByRole(..., { timeout: 10_000 })` waits, but the
test itself has vitest's default 5 s budget, so the waits can never use their deadline.

## Do
1. **Step 0.** Find the cause:
   - Is the menu's content lazily imported?
   - What makes it slow under a full run?
   - Do other specs have the same mismatch between the test's timeout and the waits inside it?
     Grep `web/src` for `timeout: 10_000` inside tests with no test-level timeout, and read the
     vitest config's `testTimeout`.

   Send the runner the cause and a fix that removes it, with the timeout made consistent as
   well. If many specs share the mismatch, propose one repo-wide fix, a config `testTimeout`
   above the longest wait, and list the files. Wait for the reply.
2. Reproduce it (for example, by running the full detail folder several times, or by forcing
   a delay on the lazy import), and show the fix holding.
3. No fixed sleeps, no skips, and no weakened assertions. Write a decisions file, run the
   CLAUDE.md `web/` checks, follow `.claude/skills/steward/SKILL.md`, and report
   `ready <PR> <sha>`.
