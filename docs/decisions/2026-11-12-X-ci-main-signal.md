# X-ci-main-signal: main pushes no longer cancel each other's CI runs

Runner call (`docs/owner-decisions.md` §2.33), under the owner's testing-first priority (§2.62).

## The problem
`main` takes dozens of merges a day. `ci.yml`, `web.yml` and `e2e.yml` set `cancel-in-progress: true` on a group keyed by `github.ref`, so each merge cancelled the main run before it. `test (windows-latest)` takes about 50 minutes and the macOS job often waits for a runner, so those jobs almost never finished on main. `ci-ok` on main read "failure" (cancelled) most of the day. The only real Windows and macOS signal came from the nightly run, so drift that two green PRs cause together went unseen until then.

## The change
In all three workflows: `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`. The `group:` expressions are unchanged, including ci.yml's nightly group. `release.yml` already uses this pattern. A small `unittest` (`.github/scripts/test_workflow_concurrency.py`) keeps the three workflows on this form.

## The load bound
GitHub keeps at most one running and one pending run per group, and a newer pending run replaces an older pending one. Main runs therefore complete in order and the load stays bounded: one running plus one pending per workflow.

## The cost
More CI minutes on main. At worst one extra full run per merge burst (the pending run that is replaced never starts; the running one now finishes).

## Pull requests
No change. A newer push to a PR still cancels that PR's superseded run.
