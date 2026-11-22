# X-infra-filter-workflows (runner brief, 2026-11-22Z)

Kind: helper (CI). Dep X-backoff-escalate-passthrough (#1044) is merged; this comes from its review.

## The gap
`infra/fleet/tests/test_workflow.py` reads every `.github/workflows/*.yml`, but the `INFRA` filter in `.github/scripts/ci_changes.py` lists only the fleet and operator workflows (`fleet`, `fleet-emergency-down`, `operator-wake`, `operator-apply`, `operator-injection`, `operator-tripwire*`). A change to any other workflow can break those tests without the infra job running.

## What to do
1. At step 0, list every file the infra tests read (`infra/fleet/tests/**`, `infra/operator/tests/**`, plus anything else the infra job runs), and compare that with what `INFRA` matches. Report every gap, not only this one.
2. Widen `INFRA` to cover every `.github/workflows/*.yml`, or exactly the set the tests read if that is narrower and stays correct when a workflow is added. Prefer the rule that can't go stale. Say which one you chose and why.
3. Add cases to `.github/scripts/test_ci_changes.py`: a change to an unrelated workflow (e.g. `web.yml`) now selects infra, and a change outside workflows and infra still doesn't.
4. Keep every other output of ci_changes.py unchanged. Show this with the existing tests, plus a before-and-after table of outputs for a handful of representative paths in the decisions file.

## Rules
- No workflow permission, secret or trigger changes; those are the owner's. Don't edit any `.github/workflows/*.yml`.
- Never beside another slice that owns `ci_changes.py`. At step 0, check that no open PR touches it.
- The infra job's runtime cost goes up for workflow-only PRs. Give the job's typical duration from a recent run in the decisions file (no run ids).

## Owns
- `.github/scripts/ci_changes.py`
- `.github/scripts/test_ci_changes.py`
- `docs/decisions/2026-11-22-X-infra-filter-workflows.md`
- this slice's `runner/slices.yaml` entry

## Step 0 (before building)
1. The list of read paths and gaps.
2. The rule chosen.
3. Open PRs touching ci_changes.py.
4. Files and an estimate.

## Branch and PR
Branch `slice/X-infra-filter-workflows`, PR title "X-infra-filter-workflows: CI runs the infra job when a workflow file changes". Run `python3 -m pytest .github/scripts/test_ci_changes.py` (or the repo's way, see ci.yml) and any CLAUDE.md gates your change touches. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
