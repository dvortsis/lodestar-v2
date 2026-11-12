# Brief: X-ci-main-signal (Sonnet, helper; testing priority)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-ci-main-signal` from `origin/main`. **PR title:** `X-ci-main-signal: main pushes no longer cancel each other's CI runs`.

**Why now:** testing comes first (owner §2.62).
- `main` takes dozens of merges a day. `ci.yml`, `web.yml` and `e2e.yml` all set `cancel-in-progress: true` on a group keyed by `github.ref`, so each merge cancels the run before it.
- `test (windows-latest)` takes about 50 minutes, and the macOS job often waits for a runner. So those two almost never finish on main, and `ci-ok` on main reads "failure" (cancelled) most of the day.
- The only real Windows and macOS signal on main comes from the nightly run. Drift that two green PRs cause together isn't seen until then.

## What to build
1. **In `ci.yml`, `web.yml` and `e2e.yml`, set `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`.**
   - Keep each `group:` expression exactly as it is, including ci.yml's nightly group.
   - Pull requests still cancel their own superseded runs. A push to main no longer cancels the main run already in progress.
   - GitHub keeps at most one running and one pending run per group, and a newer pending run replaces an older pending one. So main runs complete in order and the load stays bounded.
   - `release.yml` already uses this pattern; quote it in the decisions file.
2. **Comments:** add one line next to each changed `cancel-in-progress` saying why. In ci.yml, update the existing comment above the group.
3. **Tests:** check whether any test under `.github/scripts/` (for example `test_ci_changes.py` or a workflow-lint test) asserts concurrency settings. Update it, or add a small `unittest` that checks the three workflows use the pull-request-only form. Standard library only; parse the YAML text with a simple regex or line match, since PyYAML may not be installed.
4. **Decisions file** `docs/decisions/2026-11-12-X-ci-main-signal.md`, a runner call (§2.33):
   - the problem;
   - the change;
   - the load bound (one running plus one pending per group);
   - the cost: more main CI minutes; at worst one extra full run per merge burst;
   - that pull requests behave as before.

## Owned paths
`.github/workflows/ci.yml`, `.github/workflows/web.yml` and `.github/workflows/e2e.yml` (the `concurrency:` block and its comment only), any test under `.github/scripts/` you add or touch for this, and the decisions file.

## Don't
- Change permissions, secrets, environments, triggers, job lists, `needs`, pins or any other workflow.
- Touch the merge or required-check settings.
- Ask the runner before editing anything outside the owned paths.

## Checks before the push
- `python3 -m unittest discover -s .github/scripts -p 'test_*.py'`
- `actionlint` if it's installed. If not, `python3 -c "import yaml"` and parse each changed file, or check them by eye if PyYAML is missing.
- Re-read the diff: only the three `cancel-in-progress` lines, their comments, the test and the decisions file.

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once. When ci-ok, web-ok and e2e-ok are green, message the runner "ready <pr> <full 40-char sha>".
