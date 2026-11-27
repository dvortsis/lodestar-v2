# X-operator-cap-60 decisions (#920)

The owner lowered the `lodestar-operator` Console workspace's spend limit from $200 to $60 (owner, 2026-11-15, relayed by session_01MMjhnA1awRT8P19wZUdtY9). The operator's own caps follow it, so the code stops before the Console does (`docs/plan/2026-11-08-cloud-operator.md` §1).

## The caps
- **The wake's total cap** is $60 (`infra/operator/wake.py`, `TOTAL_CAP_CENTS` 6000, was 20000). The bridge still sums every session's `list_cost` in the workspace, archived ones included, and refuses a wake whose budget would pass it. The per-session budgets ($3 routine, $15 triage) and the rate limits are unchanged.
- **The injection test (check 8)** stops at $30 (`infra/operator/injection/harness.py`, `HARNESS_CAP_CENTS` 3000, was 5000). A session still starts only if the test's spend, plus a $1.50 hold for every session in flight, stays within it, and the workspace's total still binds as well.
- **The refusal and summary messages take their figures from the constants** (`wake.dollars`), so a later change to a cap can't leave a message naming the old one. One test per file checks that the figure follows its constant.

## The split of the $60 (runner, 2026-11-15)
- **Checks 5–7:** up to $10.
- **Check 8, the injection test:** $30, the harness's own cap.
- **The rest,** about $20, for operator work after OP-G.
- **Only the $30 is enforced in code.** The others are the runner's plan; the wake's $60 total bounds them all.

## What check 8 can cover now
- **A full level costs more than $30 in the worst case:** 81 runs at a $1.50 hold is $121.50. So a level can end `INCOMPLETE` with `cap`, and a level 3 or 4 run is likelier to than before.
  - Runs go breadth first, so a cap stop still covers every item at least once.
  - The runner decides whether a later level fits, from what the first run records a session really costs.

## Not changed
- **Older decisions files and the plan** keep the $200 and $50 they recorded. This file supersedes them for the caps.
  - `docs/plan/2026-11-08-cloud-operator.md` and `docs/owner-decisions.md` still say $200. Those are the runner's to update.
- **The docs that tell the owner what to do now say $60 and $30:** `infra/operator/README.md`, the first-run checklist's check 8, and the emergency-revoke undo step in the fleet runbook.
