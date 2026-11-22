# X-workflow-tests-filter (runner brief, 2026-11-22Z)

Kind: helper (CI). Dep X-infra-filter-workflows (#1058) is merged. This comes from #1058's review (recorded in `docs/decisions/2026-11-22-runner-records-r11.md`).

## The gaps
1. `.github/scripts/test_workflow_concurrency.py` reads `web.yml` and `e2e.yml`, but runs only in the `lint` job of `.github/workflows/ci.yml` (about lines 98-127), which needs `rust == 'true'`. A PR that changes only `web.yml` or `e2e.yml` skips it.
2. `infra/fleet/tests/test_workflow.py:132` globs `*.yml` only, so a `.yaml` workflow with a deployment environment would escape its policy check.

## What to do
1. At step 0, say where test_workflow_concurrency.py should run so that any workflow change selects it. Prefer a change to ci_changes.py outputs over a ci.yml edit. If a ci.yml edit is the clean fix, show the exact diff first and wait for my answer.
   - No job may lose a condition that keeps it from running when it should.
   - No permission, secret or trigger changes.
2. Make test_workflow.py glob both `.yml` and `.yaml`, with a test that a `.yaml` file is checked.
3. Add test_ci_changes.py cases: a web.yml-only PR now selects whatever runs test_workflow_concurrency.py.

## Rules
- No workflow permission, secret or trigger changes.
- Never beside another slice that owns `ci_changes.py`.
- #632 (open, idle since 2026-11-12) also touches test_workflow.py and ci.yml. Don't touch #632.
- Give the extra CI cost (which job, about how long) in the decisions file.

## Owns
- `.github/scripts/ci_changes.py`
- `.github/scripts/test_ci_changes.py`
- `infra/fleet/tests/test_workflow.py`
- `docs/decisions/2026-11-22-X-workflow-tests-filter.md`
- this slice's `runner/slices.yaml` entry
- `.github/workflows/ci.yml` only if I approve it at step 0

## Step 0 (before building)
1. Where the concurrency test runs, and the change.
2. Files and an estimate.

## Branch and PR
Branch `slice/X-workflow-tests-filter`, PR title "X-workflow-tests-filter: CI runs the workflow-reading tests on any workflow change". Run `python3 .github/scripts/test_ci_changes.py`, `python3 -m pytest -q .github/scripts`, and `infra/fleet/run_tests.sh` (or how ci.yml runs the fleet tests). Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
