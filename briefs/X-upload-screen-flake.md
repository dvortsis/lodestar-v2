# X-upload-screen-flake brief (runner, 2026-11-28Z)

**Slice:** X-upload-screen-flake. UploadScreen's one-at-a-time test waits on what the screen does, not on timing.
**Model:** opus. **Branch:** `slice/X-upload-screen-flake`. **PR title:** `X-upload-screen-flake: UploadScreen's one-at-a-time test waits on what the screen does, not on timing`.

## Why
- Since 2026-11-07 the "Protect main" ruleset requires `ci-ok`, `web-ok` and `e2e-ok`. A flake inside `web-ok` blocks every merge.
- `web/src/lib/features/upload/UploadScreen.test.ts`, test "shows one result per file, in order, one request at a time": X-vpn-bind's local full vitest run saw it fail once. It passed 3 of 3 alone.
- Recorded in runner records r23 (`docs/decisions/2026-11-28-runner-records-r23.md`).

## Do
1. **Step 0 (message the runner before writing code):**
   - Reproduce the flake. Run the test under load, for example `npx vitest run --sequence.concurrent` or the full suite repeated, or with a CPU hog alongside.
   - Name the race: what the test waits on, and what the screen actually does between requests.
   - Send the runner the cause with evidence: failure count over N runs and the failing assertion.
2. Fix the test to wait on the screen's own observable condition, with a deadline of at least 10 s.
   - Never use a fixed sleep. Never skip the test or loosen its assertions: it must still prove one request at a time, in order.
3. **If the race is a product bug in `UploadScreen.svelte`** (for example, the next request starting before the previous result renders), stop and ask the runner before touching the component.
4. Show it's stable:
   - Run the test 50 times under load before the fix (it should fail some of the time) and after (it should pass every time).
   - Put both counts in the PR.

## Owns
- `web/src/lib/features/upload/UploadScreen.test.ts`
- `docs/decisions/*-X-upload-screen-flake.md`, headed `# X-upload-screen-flake decisions (#PR)`.

## Rules
- Follow `.claude/skills/steward/SKILL.md`: run the web checks from CLAUDE.md before pushing, and push once per round.
- No model identifiers in commits, the PR or code. Times are in UTC with `Z`.
- The runner merges; you never do. Send `ready <PR> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) by `send_message` when CI is green.
