# X-fixed-sleep-asserts

**Goal:** replace every test assertion that follows a fixed sleep with a wait on a condition, with a deadline of at least 10 s (CLAUDE.md "Multi-session work"; owner §2.24). Track Q (code review 2026-11-12): TEST-02, DHT-03, LOGSET-04, WEBCORE-05. A fixed sleep before an assertion is a blocking review finding.

**Owns** (as in `runner/slices.yaml`; the runner gives the exact files by message at launch):
- `crates/dht/tests/**`
- the tests in `crates/lodestar/src/logging/` and `crates/lodestar/src/settings/`
- `web/src/lib/**/*.test.ts`
- `docs/decisions/2026-11-13-X-fixed-sleep-asserts.md`

**Rules**
- Tests only. No product code changes. If a test can't wait on a condition without a product hook, stop and ask the runner.
- **Positive assertions** ("X happened"): poll the condition until it holds or the deadline (≥ 10 s) passes, then assert with a message naming what was awaited.
- **Negative assertions** ("nothing happened"): don't prove absence with a sleep. Use a deterministic signal instead (fake timers, an explicit barrier, or a later positive event that orders after the thing that must not happen), and explain the choice in a comment.
- A sleep that only paces a loop (not followed by an assertion on its effect) may stay. List each one you keep, and why, in the PR body.
- Never skip, ignore or loosen a test. Never add tokio `test-util` or `testing` to the `lodestar` crate.
- If you add a shared wait helper, put it beside the tests that use it, inside your owns.
- Run the checks CLAUDE.md lists for every crate and package you touch, and repeat each changed test file at least 5 times locally to show it's stable.

**PR:** branch `slice/X-fixed-sleep-asserts`, title `X-fixed-sleep-asserts: Replace assertions after a fixed sleep with condition waits`. Follow `.claude/skills/steward/SKILL.md`. Push once per round. When CI is green, message the runner `ready <PR> <full sha>`. No cloud work, no AWS.
