# X-ci-main-signal decisions

Runner call (`docs/owner-decisions.md` §2.33), under the owner's testing-first priority (§2.62).

## The problem
`main` takes dozens of merges a day. `ci.yml`, `web.yml` and `e2e.yml` set `cancel-in-progress: true` on a group keyed by `github.ref`, so each merge cancelled the main run before it in `ci.yml` and `web.yml`. `e2e.yml` has no `push` trigger (only `pull_request`, `schedule`, `workflow_dispatch`), so merges never cancelled it; there the change only stops a manual dispatch and the nightly run from cancelling each other (the newer one queues). `test (windows-latest)` takes about 50 minutes and the macOS job often waits for a runner, so those jobs almost never finished on main. `ci-ok` on main read "failure" (cancelled) most of the day. The only real Windows and macOS signal came from the nightly run, so drift that two green PRs cause together went unseen until then.

## The change
In all three workflows: `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`. The `group:` expressions are unchanged, including ci.yml's nightly group. `release.yml` already uses this pattern (`release.yml:45`: `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`). A small `unittest` (`.github/scripts/test_workflow_concurrency.py`) keeps the three workflows on this form.

## The load bound
GitHub keeps at most one running and one pending run per group, and a newer pending run replaces an older pending one. Main runs therefore complete in order and the load stays bounded: one running plus one pending per workflow.

## The cost
More CI minutes on main. At worst one extra full run per merge burst (the pending run that is replaced never starts; the running one now finishes).

## Pull requests
No change. A newer push to a PR still cancels that PR's superseded run.

## The test runs in CI
`test_workflow_concurrency.py` runs in `ci.yml`'s lint job ("logging check" step), next to `test_log_lint.py` (runner-approved edit outside the slice's paths). That job runs only when the `rust` output is true. A change to `ci.yml` sets it (`EVERYTHING` in `ci_changes.py`), so editing the concurrency block there is always checked. A change to only `web.yml`, `e2e.yml` or the test file does not set it, so the test would not run on such a PR. The filter is left as is, pending the runner's call.

`e2e.yml` keeps its edit: it still stops a manual dispatch and the nightly run from cancelling each other, and it keeps the three workflows uniform.
