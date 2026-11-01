# X-flake-security-test: fix the settings/security vitest flake under full-suite load
**Model:** Sonnet (test robustness, following existing patterns). The runner reviews. Runner brief, 2026-11-01.

## Why
`web/src/lib/features/settings/security/security.test.ts` fails intermittently in full `npx vitest run` runs and passes alone:
- "Cancel on the password confirmation sends nothing"
- "an empty password sends nothing"

Seen by the #304 reviewer (two 10 s timeouts) and by B-update-5b (2 of 3 full local runs failed). Distinct from the #301 teardown fix. A flake gets fixed once (CLAUDE.md "Multi-session work").

## Read first
- The test file and the component(s) it drives under `web/src/lib/features/settings/security/`.
- Similar tests that wait on conditions (`until(...)`, `vi.dynamicImportSettled()`, `findBy*`), e.g. `settings/updates/updates.test.ts`, `filters/block-entry.test.ts`.
- `web/vitest.config.*` (pool, isolation, timeouts) and `docs/decisions/*X-flake*` for earlier flake fixes and their method.

## Do
1. **Reproduce** under load: run the full suite several times (or run the file with `--pool` settings that match CI and parallel load) until it fails, and capture the actual error (timeout where? which await never resolves?). Report the root cause in the PR. "Flake" isn't a root cause.
2. **Fix the cause in the test** (a missing wait for a lazy chunk, a dialog that opens after a dynamic import, shared module state between tests, a fake timer left on, a fetch mock not reset). If the root cause is in app code (a real race), stop and tell the runner with the evidence before changing app code.
3. **Keep the assertions' strength.** "Sends nothing" must still prove that no request was made after Cancel and after an empty password. Wait on a condition that shows the UI settled (the dialog closed, the button re-enabled) before asserting no request, not a fixed sleep. Never skip, `.todo`, or retry the test, and don't raise the timeout as the fix.
4. **Prove it:** run the full suite 5 times; all pass. Also run the file alone 10 times.
5. Short decisions file `docs/decisions/2026-11-01-X-flake-security-test.md` (cause, fix, proof).

## Own
`web/src/lib/features/settings/security/security.test.ts` (and a test helper it needs), your decisions file.
**Don't edit** app code (see 2), other tests, `vitest.config.*` (unless the cause is there; then tell the runner first), or anything outside `web/`.

## Size
≲ 150 changed lines.

## Done when
`npm run check` and 5 clean full `npx vitest run` passes in `web/`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the root cause. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes. Don't run formatters over files you didn't change.
